//! Job lanes: the app-wide translation gate and the single transcription lane (mac-viewmodels §5.2–5.3,
//! win-viewmodels §1.3, §13; UNIFIED.md §3.7).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use sa_core::cancel::CancellationToken;
use uuid::Uuid;

#[derive(Default)]
struct GateState {
    held: bool,
    next_id: u64,
    waiters: VecDeque<(u64, Arc<Waiter>)>,
}

#[derive(Default)]
struct Waiter {
    granted: Mutex<bool>,
    wake: Condvar,
}

/// Process-wide "one translation at a time", FIFO (satisfies both specs' tests). Thread-safe.
pub struct TranslationGate {
    state: Mutex<GateState>,
}

/// RAII permit; releasing hands the gate to the next waiter.
pub struct TranslationPermit {
    gate: Arc<TranslationGate>,
}

impl std::fmt::Debug for TranslationPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TranslationPermit")
    }
}

impl TranslationGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(GateState::default()) })
    }

    pub fn is_busy(&self) -> bool {
        self.state.lock().held
    }

    pub fn waiting_count(&self) -> usize {
        self.state.lock().waiters.len()
    }

    /// Never suspends; never jumps a queue (there is none while the gate is free).
    pub fn try_acquire(self: &Arc<Self>) -> Option<TranslationPermit> {
        let mut g = self.state.lock();
        if g.held {
            return None;
        }
        g.held = true;
        Some(TranslationPermit { gate: self.clone() })
    }

    /// Blocks a worker until acquired (FIFO) or cancelled (`None`).
    pub fn acquire(self: &Arc<Self>, cancel: &CancellationToken) -> Option<TranslationPermit> {
        if cancel.is_cancelled() {
            return None;
        }
        let (id, waiter) = {
            let mut g = self.state.lock();
            if !g.held {
                g.held = true;
                return Some(TranslationPermit { gate: self.clone() });
            }
            g.next_id += 1;
            let id = g.next_id;
            let waiter = Arc::new(Waiter::default());
            g.waiters.push_back((id, waiter.clone()));
            (id, waiter)
        };
        // Wake on cancel too.
        {
            let w = waiter.clone();
            cancel.on_cancel(move || {
                let _g = w.granted.lock();
                w.wake.notify_all();
            });
        }
        loop {
            {
                let mut granted = waiter.granted.lock();
                if *granted {
                    drop(granted);
                    if cancel.is_cancelled() {
                        // Handed over in the same instant as the cancel: pass it on.
                        self.release();
                        return None;
                    }
                    return Some(TranslationPermit { gate: self.clone() });
                }
                if cancel.is_cancelled() {
                    drop(granted);
                    let mut g = self.state.lock();
                    if let Some(pos) = g.waiters.iter().position(|(i, _)| *i == id) {
                        g.waiters.remove(pos);
                        return None;
                    }
                    drop(g);
                    // Granted between the checks: we own the gate now; release it.
                    if *waiter.granted.lock() {
                        self.release();
                    }
                    return None;
                }
                waiter.wake.wait_for(&mut granted, Duration::from_millis(100));
            }
        }
    }

    fn release(&self) {
        let mut g = self.state.lock();
        if !g.held {
            return;
        }
        match g.waiters.pop_front() {
            None => g.held = false,
            Some((_, w)) => {
                drop(g);
                let mut granted = w.granted.lock();
                *granted = true;
                w.wake.notify_all();
            }
        }
    }
}

impl Drop for TranslationPermit {
    fn drop(&mut self) {
        self.gate.release();
    }
}

/// A lane entry: one requested job for one session.
#[derive(Debug, Clone)]
pub struct QueueEntry<E> {
    pub id: Uuid,
    pub session: u64,
    pub payload: E,
}

/// Outcome of a submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    Started,
    Queued,
}

/// What [`JobQueue::advance`] handed the lane to.
#[derive(Debug)]
pub enum Advanced<E> {
    Nothing,
    /// Start this entry now (it is `running`).
    Start(QueueEntry<E>),
    /// The parked batch waiter got the lane.
    BatchGranted,
}

/// Single-transcription lane (macOS `JobQueue`; Windows FIFO queue semantics are the same: one job at a time, a
/// re-queued video replaces its entry in place, waiting singles go before the batch's next item). UI thread only.
pub struct JobQueue<E> {
    running: Option<QueueEntry<E>>,
    waiting: Vec<QueueEntry<E>>,
    batch_holds_lane: bool,
    batch_waiter: Option<crossbeam_channel::Sender<bool>>,
}

impl<E> Default for JobQueue<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E> JobQueue<E> {
    pub fn new() -> Self {
        Self { running: None, waiting: Vec::new(), batch_holds_lane: false, batch_waiter: None }
    }

    pub fn is_busy(&self) -> bool {
        self.running.is_some() || !self.waiting.is_empty() || self.batch_holds_lane
    }

    pub fn is_lane_free(&self) -> bool {
        self.running.is_none() && !self.batch_holds_lane && self.waiting.is_empty()
    }

    pub fn running(&self) -> Option<&QueueEntry<E>> {
        self.running.as_ref()
    }

    pub fn waiting(&self) -> &[QueueEntry<E>] {
        &self.waiting
    }

    pub fn batch_holds_lane(&self) -> bool {
        self.batch_holds_lane
    }

    pub fn is_running(&self, session: u64) -> bool {
        self.running.as_ref().is_some_and(|e| e.session == session)
    }

    pub fn is_queued(&self, session: u64) -> bool {
        self.waiting.iter().any(|e| e.session == session)
    }

    /// 0-based index in the waiting list.
    pub fn position(&self, session: u64) -> Option<usize> {
        self.waiting.iter().position(|e| e.session == session)
    }

    pub fn queued_entry(&self, session: u64) -> Option<&QueueEntry<E>> {
        self.waiting.iter().find(|e| e.session == session)
    }

    /// Replaces a waiting entry of the same session in place, else starts when the lane is free, else appends.
    /// The returned entry (on `Started`) must be started by the caller.
    pub fn submit(&mut self, entry: QueueEntry<E>) -> (SubmitOutcome, Option<QueueEntry<E>>)
    where
        E: Clone,
    {
        if let Some(pos) = self.position(entry.session) {
            self.waiting[pos] = entry;
            return (SubmitOutcome::Queued, None);
        }
        if self.is_lane_free() {
            self.running = Some(entry.clone());
            return (SubmitOutcome::Started, Some(entry));
        }
        self.waiting.push(entry);
        (SubmitOutcome::Queued, None)
    }

    /// Only when `running` is the entry's session: replaces it (the caller starts it).
    pub fn take_over(&mut self, entry: QueueEntry<E>) -> bool {
        if self.is_running(entry.session) {
            self.running = Some(entry);
            true
        } else {
            false
        }
    }

    /// Removes a waiting entry.
    pub fn remove(&mut self, session: u64) -> Option<QueueEntry<E>> {
        let pos = self.position(session)?;
        Some(self.waiting.remove(pos))
    }

    pub fn clear_waiting(&mut self) -> Vec<QueueEntry<E>> {
        std::mem::take(&mut self.waiting)
    }

    /// Only if `running` is `session`: frees the lane and advances.
    pub fn job_ended(&mut self, session: u64) -> Advanced<E>
    where
        E: Clone,
    {
        if self.is_running(session) {
            self.running = None;
            self.advance()
        } else {
            Advanced::Nothing
        }
    }

    /// The batch asks for the lane: `Some(true)` = granted now; `None` = parked (`waiter` receives `true` later).
    pub fn acquire_batch_slot(&mut self, waiter: crossbeam_channel::Sender<bool>) -> Option<bool> {
        if self.is_lane_free() {
            self.batch_holds_lane = true;
            return Some(true);
        }
        self.batch_waiter = Some(waiter);
        None
    }

    /// Removes a parked batch waiter (its task was cancelled).
    pub fn cancel_batch_waiter(&mut self) {
        if let Some(w) = self.batch_waiter.take() {
            let _ = w.send(false);
        }
    }

    pub fn release_batch_slot(&mut self) -> Advanced<E>
    where
        E: Clone,
    {
        if self.batch_holds_lane {
            self.batch_holds_lane = false;
            self.advance()
        } else {
            Advanced::Nothing
        }
    }

    /// Waiting single jobs go before the batch's next item.
    pub fn advance(&mut self) -> Advanced<E>
    where
        E: Clone,
    {
        if self.running.is_some() || self.batch_holds_lane {
            return Advanced::Nothing;
        }
        if !self.waiting.is_empty() {
            let e = self.waiting.remove(0);
            self.running = Some(e.clone());
            return Advanced::Start(e);
        }
        if let Some(w) = self.batch_waiter.take() {
            self.batch_holds_lane = true;
            if w.send(true).is_err() {
                // The waiter is gone: free the lane again.
                self.batch_holds_lane = false;
            }
            return Advanced::BatchGranted;
        }
        Advanced::Nothing
    }
}

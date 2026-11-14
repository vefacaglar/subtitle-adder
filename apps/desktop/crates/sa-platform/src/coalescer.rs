//! `ExternalOpenCoalescer` (mac-ui §13.2; Swift `ExternalOpen.swift`): Finder delivers "open these files" as several
//! Apple Events within a few milliseconds. Each path restarts a short quiet period; when it passes without another
//! path (or [`MAX_WAIT`] since the first one is over, so a steady stream cannot hold the batch back) everything
//! received is handed over at once, in arrival order and without repeats.
//!
//! [`ExternalOpenCoalescer`] is the pure timing logic, driven by the caller's clock. [`CoalescingDelivery`] runs it on
//! a timer thread and delivers to a sink that may be set later (a cold launch with files): paths that arrived before
//! the sink exists are kept and fed through the coalescer again when it is set, as the Swift
//! `ExternalOpenDelegate.waiting` list is replayed into the new `ExternalOpenCoalescer`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

pub const QUIET: Duration = Duration::from_millis(250);
pub const MAX_WAIT: Duration = Duration::from_secs(1);

/// How long to wait for another path: the quiet period, shortened so the batch is out `max_wait` after its first
/// path (Swift `ExternalOpenCoalescer.delay`).
pub fn delay(quiet: Duration, max_wait: Duration, since_first: Duration) -> Duration {
    quiet.min(max_wait.saturating_sub(since_first))
}

#[derive(Debug)]
pub struct ExternalOpenCoalescer {
    quiet: Duration,
    max_wait: Duration,
    pending: Vec<PathBuf>,
    first_at: Option<Instant>,
    deadline: Option<Instant>,
}

impl Default for ExternalOpenCoalescer {
    fn default() -> Self {
        Self::new()
    }
}

impl ExternalOpenCoalescer {
    pub fn new() -> Self {
        Self::with_timing(QUIET, MAX_WAIT)
    }

    pub fn with_timing(quiet: Duration, max_wait: Duration) -> Self {
        Self { quiet, max_wait, pending: Vec::new(), first_at: None, deadline: None }
    }

    /// Adds the paths of one event (repeats are dropped) and restarts the quiet period.
    pub fn add(&mut self, paths: Vec<PathBuf>, now: Instant) {
        for p in paths {
            if !self.pending.contains(&p) {
                self.pending.push(p);
            }
        }
        let first = *self.first_at.get_or_insert(now);
        self.deadline = Some(now + delay(self.quiet, self.max_wait, now.saturating_duration_since(first)));
    }

    /// Feeds `paths` received earlier (before the sink existed) in again, ahead of what is pending, and restarts the
    /// quiet period like [`Self::add`].
    pub fn replay(&mut self, paths: Vec<PathBuf>, now: Instant) {
        let pending = std::mem::take(&mut self.pending);
        self.add(paths, now);
        for p in pending {
            if !self.pending.contains(&p) {
                self.pending.push(p);
            }
        }
    }

    /// When the current batch is due (`None` when nothing is pending).
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Returns the batch when due (quiet period elapsed or max wait reached).
    pub fn poll(&mut self, now: Instant) -> Option<Vec<PathBuf>> {
        match self.deadline {
            Some(d) if now >= d => self.flush(),
            _ => None,
        }
    }

    /// Hands over what was received so far without waiting (Swift `flush()`): `None` when empty. Only once per batch.
    pub fn flush(&mut self) -> Option<Vec<PathBuf>> {
        self.deadline = None;
        self.first_at = None;
        if self.pending.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut self.pending))
    }
}

type Sink = Box<dyn FnMut(Vec<PathBuf>) + Send>;

struct DeliveryState {
    coalescer: ExternalOpenCoalescer,
    sink: Option<Sink>,
    /// Batches completed before the sink was set (cold launch with files).
    waiting: Vec<PathBuf>,
    shutdown: bool,
    timer_running: bool,
    /// A delivery is running the sink (outside the lock).
    delivering: bool,
}

/// A coalescer on its own timer thread. `receive` may be called from any thread (the macOS Apple Event handler runs
/// on the main thread); the sink is called on the timer thread (it posts to the UI thread itself).
#[derive(Clone)]
pub struct CoalescingDelivery {
    shared: Arc<(Mutex<DeliveryState>, Condvar)>,
}

impl Default for CoalescingDelivery {
    fn default() -> Self {
        Self::new(QUIET, MAX_WAIT)
    }
}

impl CoalescingDelivery {
    pub fn new(quiet: Duration, max_wait: Duration) -> Self {
        Self {
            shared: Arc::new((
                Mutex::new(DeliveryState {
                    coalescer: ExternalOpenCoalescer::with_timing(quiet, max_wait),
                    sink: None,
                    waiting: Vec::new(),
                    shutdown: false,
                    timer_running: false,
                    delivering: false,
                }),
                Condvar::new(),
            )),
        }
    }

    /// Adds the paths of one event and (re)starts the timer.
    pub fn receive(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let (lock, cv) = &*self.shared;
        let mut st = lock.lock();
        if st.shutdown {
            return;
        }
        st.coalescer.add(paths, Instant::now());
        self.ensure_timer(&mut st);
        cv.notify_all();
    }

    /// Sets the sink. Anything that arrived before is fed through the coalescer again, ahead of any path still
    /// pending (Swift: setting `ExternalOpenDelegate.coalescer` replays the waiting URLs with `receive`), so it is
    /// delivered one quiet period later as one batch together with the rest of a request still arriving.
    pub fn set_sink(&self, sink: Sink) {
        let (lock, cv) = &*self.shared;
        let mut st = lock.lock();
        st.sink = Some(sink);
        let waiting = std::mem::take(&mut st.waiting);
        if waiting.is_empty() || st.shutdown {
            return;
        }
        st.coalescer.replay(waiting, Instant::now());
        self.ensure_timer(&mut st);
        cv.notify_all();
    }

    fn ensure_timer(&self, st: &mut DeliveryState) {
        if st.timer_running {
            return;
        }
        st.timer_running = true;
        let shared = self.shared.clone();
        let spawned = std::thread::Builder::new().name("sa-external-open".into()).spawn(move || timer_loop(shared));
        if spawned.is_err() {
            st.timer_running = false;
        }
    }

    /// Delivers the pending batch now (tests; app termination).
    pub fn flush(&self) {
        let (lock, _) = &*self.shared;
        let batch = lock.lock().coalescer.flush();
        if let Some(b) = batch {
            deliver(&self.shared, b);
        }
    }

    /// Stops the timer thread; pending paths are dropped.
    pub fn shutdown(&self) {
        let (lock, cv) = &*self.shared;
        lock.lock().shutdown = true;
        cv.notify_all();
    }
}

/// Hands `batch` to the sink, or keeps it until the sink is set. The sink runs outside the lock; a batch completed
/// while it runs (a `flush` racing the timer) waits in `waiting` and is handed over by the running delivery right
/// after, so nothing is stranded until the next event.
fn deliver(shared: &Arc<(Mutex<DeliveryState>, Condvar)>, batch: Vec<PathBuf>) {
    let (lock, _) = &**shared;
    let mut st = lock.lock();
    for p in batch {
        if !st.waiting.contains(&p) {
            st.waiting.push(p);
        }
    }
    if st.delivering {
        return;
    }
    let Some(mut sink) = st.sink.take() else { return };
    st.delivering = true;
    loop {
        let next = std::mem::take(&mut st.waiting);
        if next.is_empty() {
            break;
        }
        drop(st);
        sink(next);
        st = lock.lock();
    }
    st.delivering = false;
    if st.sink.is_none() {
        st.sink = Some(sink);
    }
}

fn timer_loop(shared: Arc<(Mutex<DeliveryState>, Condvar)>) {
    let (lock, cv) = &*shared;
    loop {
        let mut st = lock.lock();
        if st.shutdown {
            st.timer_running = false;
            return;
        }
        match st.coalescer.deadline() {
            None => {
                st.timer_running = false;
                return;
            }
            Some(d) => {
                let now = Instant::now();
                if now < d {
                    cv.wait_for(&mut st, d - now);
                    continue;
                }
                let batch = st.coalescer.poll(now);
                drop(st);
                if let Some(b) = batch {
                    deliver(&shared, b);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    // Swift OpeningAndLayoutTests.theWaitForAnotherURLShrinksSoTheBatchIsOutByTheMaximumWait
    #[test]
    fn the_wait_shrinks_so_the_batch_is_out_by_the_maximum_wait() {
        let ms = Duration::from_millis;
        assert_eq!(delay(QUIET, MAX_WAIT, ms(0)), QUIET);
        assert_eq!(delay(QUIET, MAX_WAIT, ms(700)), QUIET);
        assert_eq!(delay(QUIET, MAX_WAIT, ms(900)), ms(100));
        assert_eq!(delay(QUIET, MAX_WAIT, ms(1000)), Duration::ZERO);
        assert_eq!(delay(QUIET, MAX_WAIT, ms(1500)), Duration::ZERO);
    }

    // severalURLsInQuickSuccessionArriveAsOneBatch
    #[test]
    fn several_paths_in_quick_succession_arrive_as_one_batch() {
        let t0 = Instant::now();
        let mut c = ExternalOpenCoalescer::new();
        c.add(vec![p("/a.mp4")], t0);
        c.add(vec![p("/b.mp4"), p("/a.mp4")], t0 + Duration::from_millis(5));
        c.add(vec![p("/c.mp4")], t0 + Duration::from_millis(10));
        assert_eq!(c.poll(t0 + Duration::from_millis(200)), None);
        assert_eq!(c.poll(t0 + Duration::from_millis(260)), Some(vec![p("/a.mp4"), p("/b.mp4"), p("/c.mp4")]));
        assert_eq!(c.poll(t0 + Duration::from_millis(600)), None);
    }

    #[test]
    fn a_steady_stream_is_cut_at_the_maximum_wait() {
        let t0 = Instant::now();
        let mut c = ExternalOpenCoalescer::new();
        for i in 0..10u64 {
            let now = t0 + Duration::from_millis(i * 100);
            assert_eq!(c.poll(now), None, "nothing due at {i}");
            c.add(vec![p(&format!("/{i}.mp4"))], now);
        }
        // Last add at 900 ms: wait = min(250, 1000 - 900) = 100 ms.
        assert_eq!(c.deadline(), Some(t0 + Duration::from_millis(1000)));
        assert_eq!(c.poll(t0 + Duration::from_millis(1000)).map(|b| b.len()), Some(10));
    }

    // flushDeliversImmediatelyAndOnlyOnce
    #[test]
    fn flush_delivers_immediately_and_only_once() {
        let t0 = Instant::now();
        let mut c = ExternalOpenCoalescer::new();
        c.add(vec![p("/a.srt")], t0);
        assert_eq!(c.flush(), Some(vec![p("/a.srt")]));
        assert_eq!(c.flush(), None);
        assert_eq!(c.poll(t0 + Duration::from_secs(5)), None);
    }

    #[test]
    fn delivery_buffers_until_the_sink_is_set_and_then_replays_once() {
        let d = CoalescingDelivery::new(Duration::from_millis(120), Duration::from_millis(500));
        d.receive(vec![p("/a.mp4")]);
        d.receive(vec![p("/b.mp4")]);
        std::thread::sleep(Duration::from_millis(80));
        let got = Arc::new(Mutex::new(Vec::<Vec<PathBuf>>::new()));
        let g = got.clone();
        d.set_sink(Box::new(move |b| g.lock().push(b)));
        // Replayed through the coalescer (Swift `ExternalOpenDelegate.coalescer didSet`): nothing is handed over
        // synchronously, and the rest of a request that is still arriving joins the same batch.
        assert!(got.lock().is_empty());
        d.receive(vec![p("/c.mp4"), p("/a.mp4")]);
        wait_for(&got, 1);
        assert_eq!(*got.lock(), vec![vec![p("/a.mp4"), p("/b.mp4"), p("/c.mp4")]]);
        d.receive(vec![p("/d.mp4")]);
        d.receive(vec![p("/e.mp4")]);
        wait_for(&got, 2);
        assert_eq!(got.lock()[1], vec![p("/d.mp4"), p("/e.mp4")]);
        d.shutdown();
    }

    #[test]
    fn replayed_paths_go_ahead_of_pending_ones() {
        let t0 = Instant::now();
        let mut c = ExternalOpenCoalescer::new();
        c.add(vec![p("/late.mp4"), p("/b.mp4")], t0);
        c.replay(vec![p("/a.mp4"), p("/b.mp4")], t0 + Duration::from_millis(10));
        assert_eq!(c.deadline(), Some(t0 + Duration::from_millis(260)));
        assert_eq!(c.flush(), Some(vec![p("/a.mp4"), p("/b.mp4"), p("/late.mp4")]));
    }

    fn wait_for(got: &Arc<Mutex<Vec<Vec<PathBuf>>>>, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while got.lock().len() < n && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn delivery_flush_hands_over_without_waiting() {
        let d = CoalescingDelivery::new(Duration::from_secs(10), Duration::from_secs(10));
        let got = Arc::new(Mutex::new(Vec::<Vec<PathBuf>>::new()));
        let g = got.clone();
        d.set_sink(Box::new(move |b| g.lock().push(b)));
        d.receive(vec![p("/x.mp4")]);
        d.flush();
        assert_eq!(*got.lock(), vec![vec![p("/x.mp4")]]);
        d.shutdown();
    }

    #[test]
    fn a_batch_completed_while_the_sink_runs_is_handed_over_right_after() {
        let d = CoalescingDelivery::new(Duration::from_secs(10), Duration::from_secs(10));
        let got = Arc::new(Mutex::new(Vec::<Vec<PathBuf>>::new()));
        let g = got.clone();
        let d2 = d.clone();
        let reentered = Arc::new(Mutex::new(false));
        let r = reentered.clone();
        d.set_sink(Box::new(move |b| {
            g.lock().push(b);
            // A second batch completes while this delivery is still in the sink.
            if !std::mem::replace(&mut *r.lock(), true) {
                d2.receive(vec![p("/late.mp4")]);
                d2.flush();
            }
        }));
        d.receive(vec![p("/first.mp4")]);
        d.flush();
        assert_eq!(*got.lock(), vec![vec![p("/first.mp4")], vec![p("/late.mp4")]]);
        d.shutdown();
    }
}

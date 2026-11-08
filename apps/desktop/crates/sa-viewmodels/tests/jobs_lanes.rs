//! Ports of `TranslationGateTests` (gate part), the `JobQueue` tests of `SessionTests`, `StoreLanes` ordering and
//! `SettingsRefreshSubscription` (`TranslationProgressDoesNotRefreshSettingsControls`).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;
use sa_core::dispatch::{QueueDispatcher, UiDispatcher};
use sa_viewmodels::jobs::{Advanced, JobQueue, QueueEntry, SubmitOutcome, TranslationGate};
use sa_viewmodels::lanes::{Lane, StoreLanes};
use sa_viewmodels::settings_refresh::SettingsRefreshSubscription;
use sa_viewmodels::types::SubtitleChange;
use uuid::Uuid;

fn wait(cond: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    cond()
}

#[test]
fn waiters_are_served_in_arrival_order_and_nobody_jumps_the_queue() {
    let gate = TranslationGate::new();
    let first = gate.try_acquire().expect("free");
    assert!(gate.is_busy());
    assert!(gate.try_acquire().is_none());
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for i in 1..=3 {
        let g = gate.clone();
        let o = order.clone();
        handles.push(std::thread::spawn(move || {
            let p = g.acquire(&CancellationToken::new()).expect("granted");
            o.lock().push(i);
            std::thread::sleep(Duration::from_millis(5));
            drop(p);
        }));
        assert!(wait(|| gate.waiting_count() == i), "waiter {i} parked");
    }
    assert!(gate.try_acquire().is_none(), "try_acquire never jumps the queue");
    drop(first);
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(*order.lock(), vec![1, 2, 3]);
    assert!(!gate.is_busy());
}

#[test]
fn a_cancelled_waiter_leaves_the_queue_and_the_next_one_still_gets_the_gate() {
    let gate = TranslationGate::new();
    let held = gate.try_acquire().unwrap();
    let t1 = CancellationToken::new();
    let (g1, c1) = (gate.clone(), t1.clone());
    let w1 = std::thread::spawn(move || g1.acquire(&c1).is_some());
    assert!(wait(|| gate.waiting_count() == 1));
    let g2 = gate.clone();
    let w2 = std::thread::spawn(move || g2.acquire(&CancellationToken::new()).map(|_| true).unwrap_or(false));
    assert!(wait(|| gate.waiting_count() == 2));
    t1.cancel();
    assert!(!w1.join().unwrap());
    assert!(wait(|| gate.waiting_count() == 1));
    drop(held);
    assert!(w2.join().unwrap());
    assert!(wait(|| !gate.is_busy()));
}

#[test]
fn a_cancelled_task_never_takes_a_free_gate() {
    let gate = TranslationGate::new();
    let t = CancellationToken::new();
    t.cancel();
    assert!(gate.acquire(&t).is_none());
    assert!(!gate.is_busy());
}

#[test]
fn the_gate_is_free_again_after_a_permit_is_dropped() {
    let gate = TranslationGate::new();
    {
        let _p = gate.acquire(&CancellationToken::new()).unwrap();
        assert!(gate.is_busy());
    }
    assert!(!gate.is_busy());
    assert!(gate.try_acquire().is_some());
}

fn entry(session: u64, payload: &'static str) -> QueueEntry<&'static str> {
    QueueEntry { id: Uuid::new_v4(), session, payload }
}

#[test]
fn the_queue_starts_the_first_job_and_hands_the_lane_on_in_order() {
    let mut q: JobQueue<&str> = JobQueue::new();
    let (o, started) = q.submit(entry(1, "a"));
    assert_eq!(o, SubmitOutcome::Started);
    assert_eq!(started.unwrap().payload, "a");
    assert_eq!(q.submit(entry(2, "b")).0, SubmitOutcome::Queued);
    assert_eq!(q.submit(entry(3, "c")).0, SubmitOutcome::Queued);
    assert!(matches!(q.job_ended(2), Advanced::Nothing), "only the running job can end");
    match q.job_ended(1) {
        Advanced::Start(e) => assert_eq!(e.payload, "b"),
        _ => panic!("b starts"),
    }
    assert_eq!(q.position(3), Some(0));
}

#[test]
fn a_new_request_for_a_waiting_video_replaces_it_in_place_and_never_the_running_one() {
    let mut q: JobQueue<&str> = JobQueue::new();
    q.submit(entry(1, "a"));
    q.submit(entry(2, "b"));
    q.submit(entry(3, "c"));
    assert_eq!(q.submit(entry(2, "again")).0, SubmitOutcome::Queued);
    let waiting: Vec<&str> = q.waiting().iter().map(|e| e.payload).collect();
    assert_eq!(waiting, vec!["again", "c"]);
    assert_eq!(q.running().unwrap().payload, "a");
    assert_eq!(q.submit(entry(1, "a2")).0, SubmitOutcome::Queued, "a running video queues behind");
}

#[test]
fn removing_and_clearing_only_touch_the_waiting_entries() {
    let mut q: JobQueue<&str> = JobQueue::new();
    q.submit(entry(1, "a"));
    q.submit(entry(2, "b"));
    q.submit(entry(3, "c"));
    assert!(q.remove(1).is_none());
    assert_eq!(q.remove(2).unwrap().payload, "b");
    assert_eq!(q.clear_waiting().len(), 1);
    assert!(q.is_running(1));
    assert!(q.is_busy());
}

#[test]
fn the_batch_takes_the_lane_when_it_is_free_and_waiting_jobs_go_before_it() {
    let mut q: JobQueue<&str> = JobQueue::new();
    let (tx, rx) = crossbeam_channel::bounded(1);
    assert_eq!(q.acquire_batch_slot(tx), Some(true));
    assert!(q.batch_holds_lane());
    assert_eq!(q.submit(entry(1, "single")).0, SubmitOutcome::Queued);
    match q.release_batch_slot() {
        Advanced::Start(e) => assert_eq!(e.payload, "single"),
        _ => panic!("the waiting single goes first"),
    }
    let (tx2, rx2) = crossbeam_channel::bounded(1);
    assert_eq!(q.acquire_batch_slot(tx2), None, "parked");
    assert!(matches!(q.job_ended(1), Advanced::BatchGranted));
    assert_eq!(rx2.try_recv(), Ok(true));
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_batch_that_waits_for_the_lane_leaves_when_its_task_is_cancelled() {
    let mut q: JobQueue<&str> = JobQueue::new();
    q.submit(entry(1, "a"));
    let (tx, rx) = crossbeam_channel::bounded(1);
    assert_eq!(q.acquire_batch_slot(tx), None);
    q.cancel_batch_waiter();
    assert_eq!(rx.try_recv(), Ok(false));
    q.job_ended(1);
    assert!(!q.batch_holds_lane(), "a cancelled batch waiter is not left holding the lane");
    assert!(!q.is_busy());
}

#[test]
fn store_lanes_run_in_order_and_apply_on_the_ui_thread() {
    let ui = QueueDispatcher::new();
    let lanes = StoreLanes::new(ui.clone() as Arc<dyn UiDispatcher>);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let ui_thread = std::thread::current().id();
    for i in 0..5 {
        let s1 = seen.clone();
        let s2 = seen.clone();
        lanes.run(
            Lane::Actions,
            move || {
                s1.lock().push(format!("work {i}"));
                i
            },
            move |r| {
                assert_eq!(std::thread::current().id(), ui_thread);
                s2.lock().push(format!("apply {r}"));
            },
        );
    }
    assert!(lanes.drain(Lane::Actions, Duration::from_secs(5)));
    assert!(ui.run_until(Duration::from_secs(5), || seen.lock().len() == 10));
    let works: Vec<String> = seen.lock().iter().filter(|x| x.starts_with("work")).cloned().collect();
    assert_eq!(works, (0..5).map(|i| format!("work {i}")).collect::<Vec<_>>());
    let applies: Vec<String> = seen.lock().iter().filter(|x| x.starts_with("apply")).cloned().collect();
    assert_eq!(applies, (0..5).map(|i| format!("apply {i}")).collect::<Vec<_>>());
}

#[test]
fn a_panicking_store_call_does_not_kill_the_lane() {
    let ui = QueueDispatcher::new();
    let lanes = StoreLanes::new(ui.clone() as Arc<dyn UiDispatcher>);
    lanes.spawn(Lane::Feed, || panic!("store failure"));
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    lanes.spawn(Lane::Feed, move || {
        n2.fetch_add(1, Ordering::SeqCst);
    });
    assert!(lanes.drain(Lane::Feed, Duration::from_secs(5)));
    assert_eq!(n.load(Ordering::SeqCst), 1);
}

#[test]
fn translation_progress_does_not_refresh_settings_controls() {
    let queue: Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>> = Arc::new(Mutex::new(Vec::new()));
    let q2 = queue.clone();
    let refreshes = Arc::new(Mutex::new(Vec::new()));
    let r2 = refreshes.clone();
    let sub = SettingsRefreshSubscription::new(move |f| q2.lock().push(f), move |refill| r2.lock().push(refill));
    for _ in 0..50 {
        sub.on_view_model_change(SubtitleChange::Status);
        sub.on_view_model_change(SubtitleChange::Translations);
        sub.on_view_model_change(SubtitleChange::Playlist);
    }
    assert!(queue.lock().is_empty(), "job progress never schedules a refresh");
    sub.on_view_model_change(SubtitleChange::Settings);
    sub.on_view_model_change(SubtitleChange::Models);
    sub.on_view_model_change(SubtitleChange::All);
    assert_eq!(queue.lock().len(), 1, "coalesced into one deferred refresh");
    let f = queue.lock().pop().unwrap();
    f();
    assert_eq!(*refreshes.lock(), vec![true], "refill accumulates");
    sub.request_refresh(false);
    sub.dispose();
    let f = queue.lock().pop().unwrap();
    f();
    assert_eq!(refreshes.lock().len(), 1, "nothing after dispose");
}

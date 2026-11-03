//! Keep UI locks out of CEF callbacks. One worker owns dispatch, two deadline
//! slots coalesce work, and each dispatch waits for a main-thread pump acknowledgement.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Pending {
    repaint: Option<Instant>,
    pump: Option<Instant>,
    dispatched: bool,
    stopped: bool,
}

impl Pending {
    fn request_repaint(&mut self, deadline: Option<Instant>) {
        if let Some(deadline) = deadline {
            self.repaint = Some(self.repaint.map_or(deadline, |old| old.min(deadline)));
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        if self.dispatched || self.stopped {
            return None;
        }
        match (self.repaint, self.pump) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn clear_due(&mut self, now: Instant) {
        if self.repaint.is_some_and(|deadline| deadline <= now) {
            self.repaint = None;
        }
        if self.pump.is_some_and(|deadline| deadline <= now) {
            self.pump = None;
        }
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if self.next_deadline().is_some_and(|deadline| deadline <= now) {
            self.clear_due(now);
            self.dispatched = true;
            true
        } else {
            false
        }
    }

    fn begin_pump(&mut self, now: Instant) {
        self.dispatched = false;
        // This pump services work already due, but must retain a future CEF timer
        // when an unrelated paint/input caused the earlier frame.
        self.clear_due(now);
    }
}

#[derive(Default)]
struct Shared {
    pending: Mutex<Pending>,
    changed: Condvar,
}

pub(crate) struct RepaintWaker {
    shared: Arc<Shared>,
}

impl RepaintWaker {
    pub(crate) fn new(repaint: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        std::thread::Builder::new().name("crossbow-repaint".into()).spawn(move || {
            let mut pending = worker.pending.lock().unwrap();
            loop {
                if pending.stopped {
                    return;
                }
                if pending.take_due(Instant::now()) {
                    // Never hold the scheduling lock across UI code. A CEF callback
                    // can arrive while the UI holds its Context lock, including shutdown.
                    drop(pending);
                    repaint();
                    pending = worker.pending.lock().unwrap();
                } else if let Some(deadline) = pending.next_deadline() {
                    let wait = deadline.saturating_duration_since(Instant::now());
                    pending = worker.changed.wait_timeout(pending, wait).unwrap().0;
                } else {
                    pending = worker.changed.wait(pending).unwrap();
                }
            }
        }).expect("start repaint dispatcher");
        Self { shared }
    }

    pub(crate) fn wake(&self, after: Duration) {
        let deadline = Instant::now().checked_add(after);
        let mut pending = self.shared.pending.lock().unwrap();
        if !pending.stopped {
            let previous = pending.next_deadline();
            pending.request_repaint(deadline);
            if pending.next_deadline() != previous {
                self.shared.changed.notify_one();
            }
        }
    }

    pub(crate) fn schedule_pump(&self, after: Duration) {
        let deadline = Instant::now().checked_add(after);
        let mut pending = self.shared.pending.lock().unwrap();
        if !pending.stopped {
            // CEF's latest scheduled pump replaces its previous timer. It must
            // not cancel a paint notification, which has its own bounded slot.
            let previous = pending.next_deadline();
            pending.pump = deadline;
            if pending.next_deadline() != previous {
                self.shared.changed.notify_one();
            }
        }
    }

    pub(crate) fn begin_pump(&self) {
        let mut pending = self.shared.pending.lock().unwrap();
        pending.begin_pump(Instant::now());
        self.shared.changed.notify_one();
    }

    pub(crate) fn stop(&self) {
        let mut pending = self.shared.pending.lock().unwrap();
        pending.stopped = true;
        pending.repaint = None;
        pending.pump = None;
        self.shared.changed.notify_one();
        // Do not join from the UI/CEF thread: an in-flight repaint may be waiting
        // for the UI's Context lock. It owns no CEF state and exits on returning.
    }
}

impl Drop for RepaintWaker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const WAIT: Duration = Duration::from_secs(2);

    #[test]
    fn immediate_repaint_preempts_but_preserves_future_pump() {
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        let mut pending = Pending { pump: Some(later), ..Default::default() };
        assert!(!pending.take_due(now));
        pending.request_repaint(Some(now));
        assert!(pending.take_due(now));
        assert_eq!(pending.pump, Some(later));
        assert_eq!(pending.next_deadline(), None);
        pending.begin_pump(now);
        assert_eq!(pending.next_deadline(), Some(later));
        assert!(!pending.take_due(later - Duration::from_nanos(1)));
        assert!(pending.take_due(later));
        assert!(!pending.take_due(later));
    }

    #[test]
    fn input_frame_keeps_future_timer_and_consumes_due_work() {
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        let mut pending = Pending { pump: Some(later), ..Default::default() };
        pending.request_repaint(Some(now));
        pending.begin_pump(now);
        assert_eq!(pending.repaint, None);
        assert_eq!(pending.next_deadline(), Some(later));
        pending.begin_pump(later);
        assert_eq!(pending.next_deadline(), None);
    }

    #[test]
    fn latest_cef_timer_replaces_earlier_and_later_deadlines() {
        let waker = RepaintWaker::new(|| panic!("timer fired before its deadline"));
        waker.schedule_pump(Duration::from_secs(60));
        let first = waker.shared.pending.lock().unwrap().pump.unwrap();
        waker.schedule_pump(Duration::from_secs(120));
        let later = waker.shared.pending.lock().unwrap().pump.unwrap();
        assert!(later > first);
        waker.schedule_pump(Duration::from_secs(30));
        assert!(waker.shared.pending.lock().unwrap().pump.unwrap() < first);
    }

    #[test]
    fn immediate_wake_interrupts_timer_wait_without_running_on_caller() {
        let caller = std::thread::current().id();
        let (tx, rx) = mpsc::channel();
        let waker = RepaintWaker::new(move || tx.send(std::thread::current().id()).unwrap());
        waker.schedule_pump(Duration::from_secs(60));
        waker.wake(Duration::ZERO);
        assert_ne!(rx.recv_timeout(WAIT).unwrap(), caller);
        assert!(waker.shared.pending.lock().unwrap().pump.is_some());
    }

    #[test]
    fn delayed_pump_dispatches_at_deadline_without_another_frame() {
        let (tx, rx) = mpsc::channel();
        let waker = RepaintWaker::new(move || tx.send(Instant::now()).unwrap());
        let before = Instant::now();
        let delay = Duration::from_millis(20);
        waker.schedule_pump(delay);
        let dispatched = rx.recv_timeout(WAIT).unwrap();
        assert!(dispatched >= before + delay);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn wake_storm_is_bounded_until_main_thread_pumps() {
        let (tx, rx) = mpsc::channel();
        let waker = RepaintWaker::new(move || tx.send(()).unwrap());
        waker.wake(Duration::ZERO);
        rx.recv_timeout(WAIT).unwrap();
        for _ in 0..10_000 {
            waker.wake(Duration::ZERO);
        }
        {
            let pending = waker.shared.pending.lock().unwrap();
            assert!(pending.dispatched);
            assert!(pending.repaint.is_some());
            assert_eq!(pending.next_deadline(), None);
        }
        assert!(rx.try_recv().is_err());
        waker.begin_pump();
        assert!(waker.shared.pending.lock().unwrap().repaint.is_none());
        waker.wake(Duration::ZERO);
        rx.recv_timeout(WAIT).unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn wake_during_dispatch_is_not_lost_or_blocked_by_sink() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let waker = RepaintWaker::new(move || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
        });
        waker.wake(Duration::ZERO);
        started_rx.recv_timeout(WAIT).unwrap();
        // The first repaint is in flight. Neither acknowledgement nor a new CEF
        // wake may need the sink to return or lose the next frame's notification.
        waker.begin_pump();
        waker.wake(Duration::ZERO);
        release_tx.send(()).unwrap();
        started_rx.recv_timeout(WAIT).unwrap();
        waker.stop();
        release_tx.send(()).unwrap();
    }

    #[test]
    fn stop_does_not_join_blocked_sink_or_dispatch_queued_work() {
        struct OnDrop(mpsc::Sender<()>);
        impl Drop for OnDrop {
            fn drop(&mut self) { self.0.send(()).unwrap(); }
        }
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (exited_tx, exited_rx) = mpsc::channel();
        let on_drop = OnDrop(exited_tx);
        let waker = RepaintWaker::new(move || {
            let _ = &on_drop;
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
        });
        waker.wake(Duration::ZERO);
        started_rx.recv_timeout(WAIT).unwrap();
        waker.begin_pump();
        waker.wake(Duration::ZERO);
        waker.stop();
        waker.wake(Duration::ZERO);
        waker.schedule_pump(Duration::ZERO);
        release_tx.send(()).unwrap();
        exited_rx.recv_timeout(WAIT).unwrap();
        assert!(started_rx.try_recv().is_err());
        assert_eq!(waker.shared.pending.lock().unwrap().next_deadline(), None);
    }
}

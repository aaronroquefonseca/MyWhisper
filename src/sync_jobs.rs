//! Bounded per-resource scheduling with a retained dirty request.
//!
//! A resource has at most one active job and one latest pending request. An
//! update arriving during execution is replayed after that job, at the back
//! of the queue so a busy peer cannot starve other peers.
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex};

struct Entry<R> {
    pending: Option<R>,
}
struct State<K, R> {
    entries: HashMap<K, Entry<R>>,
    ready: VecDeque<K>,
    stopped: bool,
}
struct Shared<K, R> {
    state: Mutex<State<K, R>>,
    ready: Condvar,
    capacity: usize,
}
pub(crate) struct Coalescer<K, R> {
    shared: Arc<Shared<K, R>>,
    workers: Vec<crate::runtime::OwnedThread>,
}
impl<K, R> Coalescer<K, R>
where
    K: Clone + Eq + Hash + Send + 'static,
    R: Send + 'static,
{
    pub(crate) fn new(
        workers: usize,
        capacity: usize,
        run: impl Fn(R) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        if workers == 0 || capacity < workers {
            return Err("invalid sync scheduler budget".into());
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                entries: HashMap::new(),
                ready: VecDeque::new(),
                stopped: false,
            }),
            ready: Condvar::new(),
            capacity,
        });
        let mut scheduler = Self {
            shared,
            workers: Vec::new(),
        };
        let run = Arc::new(run);
        for _ in 0..workers {
            let shared = scheduler.shared.clone();
            let run = run.clone();
            scheduler.workers.push(crate::runtime::spawn_dedicated(
                "sync-outgoing",
                move || {
                    loop {
                        let (key, request) = {
                            let mut state = shared.state.lock().unwrap_or_else(|p| p.into_inner());
                            while state.ready.is_empty() && !state.stopped {
                                state = shared.ready.wait(state).unwrap_or_else(|p| p.into_inner());
                            }
                            if state.stopped {
                                return;
                            }
                            let key = state.ready.pop_front().expect("ready resource");
                            let entry = state.entries.get_mut(&key).expect("scheduled resource");
                            (key, entry.pending.take().expect("scheduled request"))
                        };
                        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(request)))
                            .is_err()
                        {
                            eprintln!("[sync] outgoing job failed; queued updates retained");
                        }
                        let mut state = shared.state.lock().unwrap_or_else(|p| p.into_inner());
                        if state.stopped {
                            return;
                        }
                        let entry = state.entries.get_mut(&key).expect("active resource");
                        if entry.pending.is_some() {
                            state.ready.push_back(key);
                            shared.ready.notify_one();
                        } else {
                            state.entries.remove(&key);
                        }
                    }
                },
            )?);
        }
        Ok(scheduler)
    }

    pub(crate) fn submit(&self, key: K, request: R) -> Result<(), String> {
        self.submit_policy(key, request, true).map(|_| ())
    }

    /// An enumeration continuation must not overwrite a newer user request.
    pub(crate) fn continue_if_clean(&self, key: K, request: R) -> Result<bool, String> {
        self.submit_policy(key, request, false)
    }

    fn submit_policy(&self, key: K, request: R, replace: bool) -> Result<bool, String> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| "sync scheduler unavailable")?;
        if state.stopped || crate::runtime::closing() {
            return Err("sync scheduler stopped".into());
        }
        if let Some(entry) = state.entries.get_mut(&key) {
            if !replace && entry.pending.is_some() {
                return Ok(false);
            }
            // A queued key is already in ready; an active key will be requeued
            // by its worker after completion. Neither loses this dirty request.
            entry.pending = Some(request);
            return Ok(true);
        }
        if state.entries.len() >= self.shared.capacity {
            return Err(
                "outgoing sync resource budget exhausted; retry when a peer finishes".into(),
            );
        }
        state.entries.insert(
            key.clone(),
            Entry {
                pending: Some(request),
            },
        );
        state.ready.push_back(key);
        self.shared.ready.notify_one();
        Ok(true)
    }

    pub(crate) fn stop(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        state.stopped = true;
        state.entries.clear();
        state.ready.clear();
        self.shared.ready.notify_all();
    }
}
impl<K, R> Drop for Coalescer<K, R> {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
            state.stopped = true;
            state.entries.clear();
            state.ready.clear();
        }
        self.shared.ready.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn dirty_updates_replay_latest_after_active_round_without_starving_other_peers() {
        let (entered, events) = mpsc::channel();
        let (release, waiting) = mpsc::channel();
        let waiting = Mutex::new(waiting);
        let scheduler = Coalescer::new(1, 2, move |(peer, revision)| {
            entered.send((peer, revision)).unwrap();
            if peer == "a" && revision == 1 {
                waiting
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
            }
        })
        .unwrap();
        scheduler.submit("a", ("a", 1)).unwrap();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(1)).unwrap(),
            ("a", 1)
        );
        for revision in 2..=100 {
            scheduler.submit("a", ("a", revision)).unwrap();
        }
        assert!(!scheduler.continue_if_clean("a", ("a", 50)).unwrap());
        scheduler.submit("b", ("b", 1)).unwrap();
        assert!(
            scheduler
                .submit("c", ("c", 1))
                .unwrap_err()
                .contains("budget")
        );
        release.send(()).unwrap();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(1)).unwrap(),
            ("b", 1)
        );
        assert_eq!(
            events.recv_timeout(Duration::from_secs(1)).unwrap(),
            ("a", 100)
        );
        assert!(events.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn resources_run_independently_and_panics_do_not_drop_pending_updates() {
        let (entered, events) = mpsc::channel();
        let (release, waiting) = mpsc::channel();
        let waiting = Mutex::new(waiting);
        let scheduler = Coalescer::new(2, 2, move |(peer, revision)| {
            entered.send((peer, revision)).unwrap();
            if peer == "a" && revision == 1 {
                waiting
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                panic!("synthetic worker failure");
            }
        })
        .unwrap();
        scheduler.submit("a", ("a", 1)).unwrap();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(1)).unwrap(),
            ("a", 1)
        );
        scheduler.submit("a", ("a", 2)).unwrap();
        scheduler.submit("b", ("b", 1)).unwrap();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(1)).unwrap(),
            ("b", 1)
        );
        release.send(()).unwrap();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(1)).unwrap(),
            ("a", 2)
        );
    }
}

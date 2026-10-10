//! Central task runtime for the engine.
//!
//! Previously every background job called `std::thread::spawn`
//! ad hoc (20+ sites). Under resource pressure `spawn` panics, and
//! on the GTK-adjacent paths that panic aborts the whole service
//! (it cannot unwind through the C trampoline). This module is the
//! single choke point:
//!
//! - Fixed worker pool, created once at startup. Our thread budget
//!   is constant no matter how many utterances, presses, or sounds
//!   flow through; we can never fork-bomb ourselves.
//! - Bounded queue ([`JOB_QUEUE_DEPTH`]). `submit` never blocks and
//!   never panics: a full queue returns `Err`. Capture/finalization
//!   callers report admission failure and release the owning session;
//!   optional feedback jobs may be skipped.
//! - Jobs run under `catch_unwind`: a panicking job is logged and
//!   the worker survives. Pool threads only exit at process end.
//!
//! Long-lived loops (hotkey listener, sync, mic readers) are NOT
//! pool jobs — they would pin workers forever. They keep dedicated
//! threads, but spawned through [`spawn_dedicated`] (Builder +
//! `Result`), never the panicking `thread::spawn`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

/// Short-job workers. Streaming loops use owned SessionTasks, outgoing sync
/// uses its per-peer scheduler, and finalization uses its ordered worker.
pub const POOL_WORKERS: usize = 4;
/// Pending jobs before `submit` refuses. Deep enough for press +
/// release + completion of one utterance with headroom.
pub const JOB_QUEUE_DEPTH: usize = 16;

type Job = Box<dyn FnOnce() + Send + 'static>;

struct Pool {
    tx: mpsc::SyncSender<Job>,
}

static GLOBAL: OnceLock<Pool> = OnceLock::new();
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Create the pool. Call once at service startup; an `Err` here (we
/// cannot even start our fixed threads) is fatal with a message, by
/// design — better than limping into press handling.
pub fn init() -> Result<(), String> {
    let _init = INIT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if GLOBAL.get().is_some() {
        return Ok(());
    }
    let (tx, rx) = mpsc::sync_channel::<Job>(JOB_QUEUE_DEPTH);
    let rx = Arc::new(Mutex::new(rx));
    for id in 0..POOL_WORKERS {
        let rx = rx.clone();
        std::thread::Builder::new()
            .name(format!("mywhisper-pool-{id}"))
            .spawn(move || {
                loop {
                    let job = {
                        let Ok(rx) = rx.lock() else { return };
                        rx.recv()
                    };
                    let Ok(job) = job else { return }; // pool dropped
                    if let Err(e) = catch_unwind(AssertUnwindSafe(job)) {
                        eprintln!("[runtime] job panicked; worker survives: {e:?}");
                    }
                }
            })
            .map_err(|e| format!("task runtime: cannot start workers: {e}"))?;
    }
    let _ = GLOBAL.set(Pool { tx });
    Ok(())
}

fn pool() -> Result<&'static Pool, String> {
    GLOBAL
        .get()
        .ok_or_else(|| "task runtime not initialized".to_string())
}

/// Queue a job. Never blocks, never panics. `Err` means the system
/// is saturated (or shutting down): log it and degrade, never abort.
pub fn submit(name: &'static str, job: impl FnOnce() + Send + 'static) -> Result<(), String> {
    let pool = pool()?;
    pool.tx
        .try_send(Box::new(job))
        .map_err(|e| format!("[runtime] queue full, dropping {name}: {e}"))
}

/// Dedicated long-lived thread (listener loops, mic readers).
/// Builder-based: failure is a `Result`, never a panic.
pub fn spawn_dedicated(
    name: &'static str,
    f: impl FnOnce() + Send + 'static,
) -> Result<std::thread::JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(f)
        .map_err(|e| format!("[runtime] cannot spawn {name}: {e}"))
}

/// Session loops have an explicit process budget and retain ownership
/// until joined. They never occupy the short-job pool.
pub struct SessionTask {
    handle: Option<std::thread::JoinHandle<()>>,
}
static SESSION_TASKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const SESSION_TASK_LIMIT: usize = 8;
enum ReaperMessage {
    Join(std::thread::JoinHandle<()>),
    Launch(Job),
}
static SESSION_REAPER: OnceLock<Result<mpsc::Sender<ReaperMessage>, String>> = OnceLock::new();
static PROCESS_LAUNCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct LaunchPermit;
impl Drop for LaunchPermit {
    fn drop(&mut self) {
        PROCESS_LAUNCH.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// The existing persistent ownership thread creates supervised processes.
/// One launch may be queued/active, independently of the short-job pool.
pub(crate) fn submit_process_launch(job: impl FnOnce() + Send + 'static) -> Result<(), String> {
    let owner = session_reaper()?;
    PROCESS_LAUNCH
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .map_err(|_| "a supervised process launch is already pending")?;
    let permit = LaunchPermit;
    owner
        .send(ReaperMessage::Launch(Box::new(move || {
            let _permit = permit;
            job();
        })))
        .map_err(|_| "process ownership thread unavailable".into())
}

fn receive_owned_work(message: ReaperMessage, pending: &mut Vec<std::thread::JoinHandle<()>>) {
    match message {
        ReaperMessage::Join(handle) => pending.push(handle),
        ReaperMessage::Launch(job) => {
            if catch_unwind(AssertUnwindSafe(job)).is_err() {
                eprintln!("[runtime] supervised process launch panicked");
            }
        }
    }
}

fn session_reaper() -> Result<&'static mpsc::Sender<ReaperMessage>, String> {
    SESSION_REAPER
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<ReaperMessage>();
            std::thread::Builder::new()
                .name("session-reaper".into())
                .spawn(move || {
                    let mut pending = Vec::new();
                    loop {
                        if let Ok(message) = rx.recv_timeout(std::time::Duration::from_millis(20)) {
                            receive_owned_work(message, &mut pending);
                        }
                        // The channel contains at most the eight owned joins
                        // and the single launch permit. Reaping never waits
                        // for a live session thread to exit.
                        for message in rx.try_iter() {
                            receive_owned_work(message, &mut pending);
                        }
                        let mut i = 0;
                        while i < pending.len() {
                            if pending[i].is_finished() {
                                let _ = pending.swap_remove(i).join();
                                SESSION_TASKS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                            } else {
                                i += 1;
                            }
                        }
                    }
                })
                .map_err(|e| format!("session reaper: {e}"))?;
            Ok(tx)
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub fn spawn_session(
    name: &'static str,
    f: impl FnOnce() + Send + 'static,
) -> Result<SessionTask, String> {
    let reaper = session_reaper()?;
    let old = SESSION_TASKS.try_update(
        std::sync::atomic::Ordering::AcqRel,
        std::sync::atomic::Ordering::Acquire,
        |count| (count < SESSION_TASK_LIMIT).then_some(count + 1),
    );
    if old.is_err() {
        return Err("session task budget exhausted; wait for cancellation cleanup".into());
    }
    match spawn_dedicated(name, f) {
        Ok(handle) => {
            let _ = reaper;
            Ok(SessionTask {
                handle: Some(handle),
            })
        }
        Err(e) => {
            SESSION_TASKS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            Err(e)
        }
    }
}

impl SessionTask {
    /// Only use for loops whose stop path is nonblocking and already signalled.
    pub fn join(mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
            SESSION_TASKS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        }
    }
}

impl Drop for SessionTask {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take()
            && let Ok(reaper) = session_reaper()
        {
            let _ = reaper.send(ReaperMessage::Join(handle));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn supervised_launch_admission_recovers_after_owned_job_panics() {
        let (started, ready) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        submit_process_launch(move || {
            started.send(()).unwrap();
            wait.recv().unwrap();
            panic!("synthetic launch failure");
        })
        .unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(submit_process_launch(|| panic!("excess launch must not execute")).is_err());
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while PROCESS_LAUNCH.load(std::sync::atomic::Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "launch permit leaked after panic"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let (finished, completion) = mpsc::channel();
        submit_process_launch(move || finished.send(()).unwrap()).unwrap();
        completion.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn pool_runs_jobs() {
        init().unwrap();
        let n = Arc::new(AtomicUsize::new(0));
        for _ in 0..8 {
            let n = n.clone();
            submit("t", move || {
                n.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        }
        let start = std::time::Instant::now();
        while n.load(Ordering::SeqCst) < 8 && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(n.load(Ordering::SeqCst), 8);
    }

    #[test]
    fn panicking_job_does_not_kill_pool() {
        init().unwrap();
        submit("boom", || panic!("intentional")).unwrap();
        let done = Arc::new(AtomicUsize::new(0));
        let d = done.clone();
        // Give the panic a moment to land, then prove the pool lives.
        std::thread::sleep(Duration::from_millis(200));
        submit("after", move || {
            d.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        let start = std::time::Instant::now();
        while done.load(Ordering::SeqCst) < 1 && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(done.load(Ordering::SeqCst), 1);
    }
}

/// One bounded FIFO worker owns finalization through cleanup and delivery.
/// Recording and streaming remain independent; slow cleanup cannot reorder
/// later utterances or consume every short-job worker.
static FINALIZATION_QUEUE: OnceLock<Result<mpsc::SyncSender<Job>, String>> = OnceLock::new();
pub fn submit_finalization(job: impl FnOnce() + Send + 'static) -> Result<(), String> {
    let queue = FINALIZATION_QUEUE
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel::<Job>(4);
            std::thread::Builder::new()
                .name("utterance-finalization".into())
                .spawn(move || {
                    for job in rx {
                        if catch_unwind(AssertUnwindSafe(job)).is_err() {
                            eprintln!("[runtime] finalization panicked; worker survives");
                        }
                    }
                })
                .map_err(|e| format!("cannot start finalization worker: {e}"))?;
            Ok(tx)
        })
        .as_ref()
        .map_err(Clone::clone)?;
    queue
        .try_send(Box::new(job))
        .map_err(|_| "finalization queue is full or stopped".into())
}

#[cfg(test)]
mod finalization_tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn slow_first_delivery_cannot_be_overtaken_and_panic_does_not_stop_queue() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (order_tx, order_rx) = mpsc::channel();
        let first = order_tx.clone();
        submit_finalization(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            first.send(1).unwrap();
        })
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let second = order_tx.clone();
        submit_finalization(move || {
            second.send(2).unwrap();
        })
        .unwrap();
        submit_finalization(|| panic!("injected ordered job failure")).unwrap();
        submit_finalization(move || {
            order_tx.send(3).unwrap();
        })
        .unwrap();
        assert!(order_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        for expected in [1, 2, 3] {
            assert_eq!(
                order_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
                expected
            );
        }
    }
}

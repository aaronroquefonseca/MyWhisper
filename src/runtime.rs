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
//!   the worker survives. Closing disconnects queues and joins workers.
//!
//! Long-lived loops (hotkey listener, sync, mic readers) are NOT
//! pool jobs — they would pin workers forever. They keep dedicated
//! threads, but spawned through [`spawn_dedicated`] (Builder +
//! `Result`), never the panicking `thread::spawn`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc};

static CLOSING: AtomicBool = AtomicBool::new(false);
// Admission and handle publication share this lock. Shutdown cannot miss a
// thread/job admitted just before the closing flag was published.
static ADMISSION: Mutex<()> = Mutex::new(());
static THREADS: Mutex<Vec<Arc<ThreadOwner>>> = Mutex::new(Vec::new());
// Covers the eight-session budget, fixed workers, service/clipboard loops and
// bounded inbound/outbound sync owners. Unjoined handles are never unbounded.
const THREAD_OWNER_LIMIT: usize = 64;
type StopState = (Mutex<bool>, Condvar);
static STOPS: Mutex<Vec<Weak<StopState>>> = Mutex::new(Vec::new());

pub fn closing() -> bool {
    CLOSING.load(Ordering::Acquire)
}

fn admitted() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    let guard = ADMISSION.lock().unwrap_or_else(|p| p.into_inner());
    if closing() {
        return Err("application is shutting down".into());
    }
    Ok(guard)
}

/// Publish cancellation without waiting for workers or doing filesystem IO.
/// Call on application quit/signal; actual joins happen after GTK returns.
pub fn begin_shutdown() {
    let stops = {
        let _admission = ADMISSION.lock().unwrap_or_else(|p| p.into_inner());
        CLOSING.store(true, Ordering::Release);
        STOPS.lock().unwrap_or_else(|p| p.into_inner()).clone()
    };
    for stop in stops.into_iter().filter_map(|stop| stop.upgrade()) {
        *stop.0.lock().unwrap_or_else(|p| p.into_inner()) = true;
        stop.1.notify_all();
    }
}

struct ThreadOwner {
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    session: bool,
}
impl ThreadOwner {
    fn join(&self) -> std::thread::Result<()> {
        // Keep the lock through the actual join, including thread-local
        // destructors. A second joiner must not observe premature completion.
        let mut handle = self.handle.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(handle) = handle.take() {
            let result = handle.join();
            if self.session {
                SESSION_TASKS.fetch_sub(1, Ordering::AcqRel);
            }
            result
        } else {
            Ok(())
        }
    }
    fn finished(&self) -> bool {
        self.handle.try_lock().is_ok_and(|handle| {
            handle
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
        })
    }
    fn joined(&self) -> bool {
        self.handle.try_lock().is_ok_and(|handle| handle.is_none())
    }
}

/// The caller and process share the same actual handle. Dropping the caller
/// cannot detach unfinished cleanup; either owner can join exactly once.
pub struct OwnedThread(Arc<ThreadOwner>);
impl OwnedThread {
    pub fn join(self) -> std::thread::Result<()> {
        self.0.join()
    }
    pub fn is_finished(&self) -> bool {
        self.0.finished()
    }
}

fn create_thread(
    name: &str,
    session: bool,
    f: impl FnOnce() + Send + 'static,
) -> Result<OwnedThread, String> {
    let mut threads = THREADS.lock().unwrap_or_else(|p| p.into_inner());
    // JoinHandle::is_finished can precede TLS destructors. Never join on an
    // input/admission thread: remove only handles already joined by an owner.
    threads.retain(|thread| !thread.joined());
    if threads.len() >= THREAD_OWNER_LIMIT {
        return Err("process thread ownership budget exhausted; wait for cleanup".into());
    }
    let handle = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(f)
        .map_err(|e| format!("[runtime] cannot spawn {name}: {e}"))?;
    let owner = Arc::new(ThreadOwner {
        handle: Mutex::new(Some(handle)),
        session,
    });
    threads.push(owner.clone());
    Ok(OwnedThread(owner))
}

/// Short-job workers. Streaming loops use owned SessionTasks, outgoing sync
/// uses its per-peer scheduler, and finalization uses its ordered worker.
pub const POOL_WORKERS: usize = 4;
/// Pending jobs before `submit` refuses. Deep enough for press +
/// release + completion of one utterance with headroom.
pub const JOB_QUEUE_DEPTH: usize = 16;

type Job = Box<dyn FnOnce() + Send + 'static>;

static ACTIVE_WORK: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

struct WorkPermit;
impl Drop for WorkPermit {
    fn drop(&mut self) {
        let mut pending = ACTIVE_WORK.0.lock().unwrap_or_else(|p| p.into_inner());
        *pending -= 1;
        ACTIVE_WORK.1.notify_all();
    }
}

// Fields drop in declaration order. Rejected/abandoned jobs must drop their
// captured stores and other resources before publishing completion as well.
struct TrackedJob {
    job: Option<Job>,
    _permit: WorkPermit,
}
impl TrackedJob {
    fn run(mut self) {
        if !closing()
            && let Some(job) = self.job.take()
        {
            job();
        }
    }
}

fn tracked(job: impl FnOnce() + Send + 'static) -> Job {
    *ACTIVE_WORK.0.lock().unwrap_or_else(|p| p.into_inner()) += 1;
    let owned = TrackedJob {
        job: Some(Box::new(job)),
        _permit: WorkPermit,
    };
    Box::new(move || owned.run())
}

/// Observe completion of actual accepted jobs, including captured-resource
/// destruction. Queue sentinels establish dequeue order, not completion order
/// when several workers can run earlier jobs concurrently. Call off GTK.
pub fn drain(timeout: std::time::Duration) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut pending = ACTIVE_WORK.0.lock().unwrap_or_else(|p| p.into_inner());
    while *pending != 0 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "{} owned tasks have not completed cleanup",
                *pending
            ));
        }
        pending = ACTIVE_WORK
            .1
            .wait_timeout(pending, remaining)
            .unwrap_or_else(|p| p.into_inner())
            .0;
    }
    Ok(())
}

struct Pool {
    tx: Mutex<Option<mpsc::SyncSender<Job>>>,
}

static GLOBAL: OnceLock<Pool> = OnceLock::new();
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Create the pool. Call once at service startup; an `Err` here (we
/// cannot even start our fixed threads) is fatal with a message, by
/// design — better than limping into press handling.
pub fn init() -> Result<(), String> {
    let _init = INIT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _admission = admitted()?;
    if GLOBAL.get().is_some() {
        return Ok(());
    }
    let (tx, rx) = mpsc::sync_channel::<Job>(JOB_QUEUE_DEPTH);
    let rx = Arc::new(Mutex::new(rx));
    let mut workers = Vec::new();
    for id in 0..POOL_WORKERS {
        let rx = rx.clone();
        let result = create_thread(&format!("mywhisper-pool-{id}"), false, move || {
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
        });
        match result {
            Ok(worker) => workers.push(worker),
            Err(error) => {
                drop(tx);
                for worker in workers {
                    let _ = worker.join();
                }
                return Err(error);
            }
        }
    }
    let _ = GLOBAL.set(Pool {
        tx: Mutex::new(Some(tx)),
    });
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
    let _admission = admitted()?;
    let pool = pool()?;
    let sender = pool.tx.lock().unwrap_or_else(|p| p.into_inner());
    sender
        .as_ref()
        .ok_or("task runtime stopped")?
        .try_send(tracked(job))
        .map_err(|e| format!("[runtime] queue full, dropping {name}: {e}"))
}

/// Dedicated long-lived thread (listener loops, mic readers).
/// Builder-based: failure is a `Result`, never a panic.
pub fn spawn_dedicated(
    name: &'static str,
    f: impl FnOnce() + Send + 'static,
) -> Result<OwnedThread, String> {
    let _admission = admitted()?;
    create_thread(name, false, f)
}

/// Interruptible timer for an owned background loop. Stopping interrupts the
/// current wait; an in-progress pass must finish before its owner can join it.
#[derive(Clone)]
pub struct StopToken(Arc<StopState>);
impl StopToken {
    pub(crate) fn new() -> Result<Self, String> {
        let _admission = admitted()?;
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let mut stops = STOPS.lock().unwrap_or_else(|p| p.into_inner());
        stops.retain(|stop| stop.strong_count() != 0);
        stops.push(Arc::downgrade(&stop));
        Ok(Self(stop))
    }
    pub(crate) fn stop(&self) {
        *self.0.0.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.0.1.notify_all();
    }
    pub fn stopped(&self) -> bool {
        closing() || *self.0.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn wait(&self, delay: std::time::Duration) -> bool {
        let stopped = self.0.0.lock().unwrap_or_else(|p| p.into_inner());
        *self
            .0
            .1
            .wait_timeout_while(stopped, delay, |stopped| !*stopped)
            .unwrap_or_else(|p| p.into_inner())
            .0
    }
}

/// Retain the actual thread and join it when its process-level owner ends.
/// Drop outside GTK: shutdown must not race a watcher still using SQLite.
#[must_use = "dropping a background task stops and joins its loop"]
pub struct BackgroundTask {
    stop: StopToken,
    handle: Option<OwnedThread>,
}
impl BackgroundTask {
    pub fn spawn(
        name: &'static str,
        f: impl FnOnce(StopToken) + Send + 'static,
    ) -> Result<Self, String> {
        let stop = StopToken::new()?;
        let worker = stop.clone();
        let handle = spawn_dedicated(name, move || f(worker))?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }

    pub fn stop(&self) {
        self.stop.stop();
    }
}
impl Drop for BackgroundTask {
    fn drop(&mut self) {
        self.stop();
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            eprintln!("[runtime] owned background task exited after a panic");
        }
    }
}

/// Session loops have an explicit process budget and retain ownership
/// until joined. They never occupy the short-job pool.
pub struct SessionTask {
    handle: Option<OwnedThread>,
}
static SESSION_TASKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const SESSION_TASK_LIMIT: usize = 8;
enum ReaperMessage {
    Join(OwnedThread),
    Launch(Job),
}
struct ReaperQueue(Mutex<Option<mpsc::Sender<ReaperMessage>>>);
static SESSION_REAPER: OnceLock<Result<ReaperQueue, String>> = OnceLock::new();
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
    let _admission = admitted()?;
    PROCESS_LAUNCH
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .map_err(|_| "a supervised process launch is already pending")?;
    let permit = LaunchPermit;
    let sender = owner.0.lock().unwrap_or_else(|p| p.into_inner());
    sender
        .as_ref()
        .ok_or("process ownership thread stopped")?
        .send(ReaperMessage::Launch(tracked(move || {
            let _permit = permit;
            job();
        })))
        .map_err(|_| "process ownership thread unavailable".into())
}

fn receive_owned_work(message: ReaperMessage, pending: &mut Vec<OwnedThread>) {
    match message {
        ReaperMessage::Join(handle) => pending.push(handle),
        ReaperMessage::Launch(job) => {
            if catch_unwind(AssertUnwindSafe(job)).is_err() {
                eprintln!("[runtime] supervised process launch panicked");
            }
        }
    }
}

fn session_reaper() -> Result<&'static ReaperQueue, String> {
    SESSION_REAPER
        .get_or_init(|| {
            let _admission = admitted()?;
            let (tx, rx) = mpsc::channel::<ReaperMessage>();
            create_thread("session-reaper", false, move || {
                let mut pending = Vec::new();
                let mut disconnected = false;
                loop {
                    match rx.recv_timeout(std::time::Duration::from_millis(20)) {
                        Ok(message) => receive_owned_work(message, &mut pending),
                        Err(mpsc::RecvTimeoutError::Disconnected) => disconnected = true,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
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
                        } else {
                            i += 1;
                        }
                    }
                    if disconnected {
                        if pending.is_empty() {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                }
            })?;
            Ok(ReaperQueue(Mutex::new(Some(tx))))
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub fn spawn_session(
    name: &'static str,
    f: impl FnOnce() + Send + 'static,
) -> Result<SessionTask, String> {
    let reaper = session_reaper()?;
    let _admission = admitted()?;
    let old = SESSION_TASKS.try_update(
        std::sync::atomic::Ordering::AcqRel,
        std::sync::atomic::Ordering::Acquire,
        |count| (count < SESSION_TASK_LIMIT).then_some(count + 1),
    );
    if old.is_err() {
        return Err("session task budget exhausted; wait for cancellation cleanup".into());
    }
    match create_thread(name, true, tracked(f)) {
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
        }
    }
}

impl Drop for SessionTask {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take()
            && let Some(Ok(reaper)) = SESSION_REAPER.get()
        {
            let sender = reaper.0.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(sender) = sender.as_ref() {
                let _ = sender.send(ReaperMessage::Join(handle));
            }
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

    #[test]
    fn background_owner_interrupts_idle_wait_and_joins_active_cleanup() {
        let (ready, started) = mpsc::channel();
        let (exited, finished) = mpsc::channel();
        let owner = BackgroundTask::spawn("synthetic-idle-owner", move |stop| {
            ready.send(()).unwrap();
            assert!(stop.wait(Duration::from_secs(60)));
            exited.send(()).unwrap();
        })
        .unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let began = std::time::Instant::now();
        drop(owner);
        assert!(began.elapsed() < Duration::from_millis(500));
        finished.try_recv().unwrap();

        // The actual encrypted connection must close before join returns,
        // even when a stop request arrives during an in-progress pass.
        let root = tempfile::tempdir().unwrap();
        let store =
            crate::store::Store::open_with_key(&root.path().join("state.db"), &[23; 32]).unwrap();
        let (ready, started) = mpsc::channel();
        let (release, waiting) = mpsc::channel();
        let (closed, cleanup) = mpsc::channel();
        let owner = BackgroundTask::spawn("synthetic-active-owner", move |stop| {
            ready.send(()).unwrap();
            waiting.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(stop.stopped());
            drop(store);
            closed.send(()).unwrap();
        })
        .unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        owner.stop();
        let (joined, complete) = mpsc::channel();
        let closer = std::thread::spawn(move || {
            drop(owner);
            joined.send(()).unwrap();
        });
        assert!(complete.recv_timeout(Duration::from_millis(20)).is_err());
        release.send(()).unwrap();
        complete.recv_timeout(Duration::from_secs(2)).unwrap();
        cleanup.try_recv().unwrap();
        closer.join().unwrap();
    }
}

/// One bounded FIFO worker owns finalization through cleanup and delivery.
/// Recording and streaming remain independent; slow cleanup cannot reorder
/// later utterances or consume every short-job worker.
static FINALIZATION_QUEUE: OnceLock<Result<Pool, String>> = OnceLock::new();
pub fn submit_finalization(job: impl FnOnce() + Send + 'static) -> Result<(), String> {
    let queue = FINALIZATION_QUEUE
        .get_or_init(|| {
            let _admission = admitted()?;
            let (tx, rx) = mpsc::sync_channel::<Job>(4);
            create_thread("utterance-finalization", false, move || {
                for job in rx {
                    if catch_unwind(AssertUnwindSafe(job)).is_err() {
                        eprintln!("[runtime] finalization panicked; worker survives");
                    }
                }
            })?;
            Ok(Pool {
                tx: Mutex::new(Some(tx)),
            })
        })
        .as_ref()
        .map_err(Clone::clone)?;
    let _admission = admitted()?;
    let sender = queue.tx.lock().unwrap_or_else(|p| p.into_inner());
    sender
        .as_ref()
        .ok_or("finalization worker stopped")?
        .try_send(tracked(job))
        .map_err(|_| "finalization queue is full or stopped".into())
}

/// After producers are stopped, cancel active operations, disconnect queues
/// and join every actual thread before SQLite/global library finalizers run.
/// Call from the main thread outside GTK; never substitute dequeue sentinels.
pub fn finish_shutdown() -> Result<(), String> {
    begin_shutdown();
    crate::net::shutdown_outgoing();
    if let Some(pool) = GLOBAL.get() {
        pool.tx.lock().unwrap_or_else(|p| p.into_inner()).take();
    }
    if let Some(Ok(pool)) = FINALIZATION_QUEUE.get() {
        pool.tx.lock().unwrap_or_else(|p| p.into_inner()).take();
    }
    if let Some(Ok(reaper)) = SESSION_REAPER.get() {
        reaper.0.lock().unwrap_or_else(|p| p.into_inner()).take();
    }
    let threads = std::mem::take(&mut *THREADS.lock().unwrap_or_else(|p| p.into_inner()));
    let mut failed = false;
    for thread in threads {
        if thread.join().is_err() {
            failed = true;
            eprintln!("[runtime] owned thread exited after a panic during shutdown");
        }
    }
    drain(std::time::Duration::ZERO)?;
    if failed {
        Err("a worker panicked during shutdown".into())
    } else {
        Ok(())
    }
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

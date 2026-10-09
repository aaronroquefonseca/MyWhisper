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
//!   never panics: a full queue returns `Err`, and callers degrade
//!   (skip the cue, drop the utterance, hide the pill) instead of
//!   dying.
//! - Jobs run under `catch_unwind`: a panicking job is logged and
//!   the worker survives. Pool threads only exit at process end.
//!
//! Long-lived loops (hotkey listener, sync, mic readers) are NOT
//! pool jobs — they would pin workers forever. They keep dedicated
//! threads, but spawned through [`spawn_dedicated`] (Builder +
//! `Result`), never the panicking `thread::spawn`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

/// Pool workers. Dictation needs at most: 1 transcribe worker +
/// 1 streaming pump + 1 warmup + slack for cues/reapers.
pub const POOL_WORKERS: usize = 4;
/// Pending jobs before `submit` refuses. Deep enough for press +
/// release + completion of one utterance with headroom.
pub const JOB_QUEUE_DEPTH: usize = 16;

type Job = Box<dyn FnOnce() + Send + 'static>;

struct Pool {
    tx: mpsc::SyncSender<Job>,
}

static GLOBAL: OnceLock<Pool> = OnceLock::new();

/// Create the pool. Call once at service startup; an `Err` here (we
/// cannot even start our fixed threads) is fatal with a message, by
/// design — better than limping into press handling.
pub fn init() -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

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

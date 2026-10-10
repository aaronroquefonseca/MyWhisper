//! Idle retention owned by a frontend's application state.
//!
//! The worker has its own connection and never runs SQLite or filesystem
//! cleanup on GTK. Closing the owner interrupts its timer; any active pass
//! finishes under the writer lock before the task is reaped.
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

struct Control {
    stopped: Mutex<bool>,
    wake: Condvar,
}

pub(crate) struct RetentionMaintenance {
    control: Arc<Control>,
    _task: crate::runtime::SessionTask,
}

impl RetentionMaintenance {
    pub(crate) fn start(path: PathBuf) -> Result<Self, String> {
        let mut store = None;
        Self::spawn(Duration::from_secs(60), move || {
            if store.is_none() {
                match crate::store::Store::open_existing(&path) {
                    Ok(connection) => store = Some(connection),
                    Err(error) => {
                        eprintln!("[retention] database unavailable: {error}");
                        return;
                    }
                }
            }
            if let Err(error) = store
                .as_ref()
                .expect("opened retention connection")
                .maintain_retention_at(crate::store::now_ms() / 1000)
            {
                eprintln!("[retention] cleanup failed; will retry: {error}");
            }
        })
    }

    fn spawn(interval: Duration, mut pass: impl FnMut() + Send + 'static) -> Result<Self, String> {
        let control = Arc::new(Control {
            stopped: Mutex::new(false),
            wake: Condvar::new(),
        });
        let worker = control.clone();
        let task = crate::runtime::spawn_session("retention-maintenance", move || {
            loop {
                if *worker.stopped.lock().unwrap_or_else(|p| p.into_inner()) {
                    return;
                }
                pass();
                let stopped = worker.stopped.lock().unwrap_or_else(|p| p.into_inner());
                let (stopped, _) = worker
                    .wake
                    .wait_timeout_while(stopped, interval, |stopped| !*stopped)
                    .unwrap_or_else(|p| p.into_inner());
                if *stopped {
                    return;
                }
            }
        })?;
        Ok(Self {
            control,
            _task: task,
        })
    }
}

impl Drop for RetentionMaintenance {
    fn drop(&mut self) {
        *self
            .control
            .stopped
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = true;
        self.control.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{HistoryRecord, Store};
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::mpsc;

    #[test]
    fn idle_worker_expires_existing_data_with_recording_off_and_stops_with_owner() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = Store::open(&path).unwrap();
        let now = crate::store::now_ms() / 1000;
        let mut settings = store.load_settings();
        settings.privacy.keep_audio_recordings = false;
        settings.privacy.transcript_auto_delete_days = 1;
        settings.privacy.audio_retention_days = 1;
        store.save_settings(&settings).unwrap();
        store
            .apply_history(&[HistoryRecord {
                uuid: "synthetic-idle".into(),
                text: "synthetic private text".into(),
                raw_text: "synthetic private raw".into(),
                words: 3,
                duration_secs: 1,
                deleted: false,
                discarded: false,
                recorded_at: now,
            }])
            .unwrap();
        let vault = crate::audio::audio_vault_dir(root.path());
        crate::private_fs::directory(&vault).unwrap();
        let clip = vault.join("synthetic-idle.wav");
        std::fs::write(&clip, b"synthetic retained WAV").unwrap();
        let clock = Arc::new(AtomicI64::new(now));
        let worker_clock = clock.clone();
        let (completed, events) = mpsc::channel();
        let mut connection = None;
        let worker = RetentionMaintenance::spawn(Duration::from_millis(10), move || {
            let store = connection.get_or_insert_with(|| Store::open(&path).unwrap());
            let at = worker_clock.load(Ordering::Acquire);
            let result = store.maintain_retention_at(at).unwrap();
            let _ = completed.send((at, result));
        })
        .unwrap();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            (now, (0, 0))
        );
        assert!(clip.exists());
        clock.store(now + 2 * 86400, Ordering::Release);
        loop {
            let (at, result) = events.recv_timeout(Duration::from_secs(2)).unwrap();
            if at > now {
                assert_eq!(result, (1, 1));
                break;
            }
        }
        assert!(!clip.exists());
        let history = store.export_history().unwrap();
        assert!(history[0].deleted && history[0].text.is_empty() && history[0].raw_text.is_empty());
        drop(worker);
        // Drain a pass already in flight, then prove the sender/task actually
        // exits rather than merely ceasing visible timer updates.
        loop {
            match events.recv_timeout(Duration::from_secs(2)) {
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(error) => panic!("retention worker outlived its owner: {error}"),
            }
        }
    }

    #[test]
    fn closing_owner_interrupts_a_long_idle_timer() {
        let (completed, events) = mpsc::channel();
        let worker = RetentionMaintenance::spawn(Duration::from_secs(3600), move || {
            completed.send(()).unwrap();
        })
        .unwrap();
        events.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(worker);
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }
}

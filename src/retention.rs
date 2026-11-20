//! Retention maintenance independent of dictation: transcript
//! auto-delete and audio retention run at startup, hourly and after
//! preference changes on one background thread (never the GTK thread),
//! so old data expires even when nobody dictates or delivery fails.
//! The engine owner also removes staging leftovers of a crashed run.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::Duration;

const PERIOD: Duration = Duration::from_secs(60 * 60);

static WAKE: OnceLock<SyncSender<()>> = OnceLock::new();

/// Apply both retention windows once. Audio retention covers whatever
/// the vault holds, even after the keep-audio switch was turned off.
pub(crate) fn apply(store: &crate::store::Store, data_dir: &Path) {
    let privacy = store.load_settings().privacy;
    if privacy.audio_retention_days > 0 {
        let n = crate::audio::purge_old_audio(data_dir, privacy.audio_retention_days);
        if n > 0 {
            eprintln!("[retention] audio auto-delete: {n} files");
        }
    }
    if privacy.transcript_auto_delete_days > 0
        && let Err(e) = store.purge_old_history(privacy.transcript_auto_delete_days)
    {
        eprintln!("[retention] transcript auto-delete: {e}");
    }
}

fn run(data_dir: &Path) {
    match crate::store::Store::open(&data_dir.join(crate::paths::DB_FILE)) {
        Ok(store) => apply(&store, data_dir),
        Err(e) => eprintln!("[retention] store: {e}"),
    }
}

fn serve(data_dir: &Path, wake: Receiver<()>, period: Duration) {
    loop {
        run(data_dir);
        if let Err(RecvTimeoutError::Disconnected) = wake.recv_timeout(period) {
            return;
        }
    }
}

/// Start the per-process maintenance thread (first pass immediately).
pub fn start(data_dir: PathBuf) {
    let (tx, rx) = sync_channel(1);
    if WAKE.set(tx).is_err() {
        return;
    }
    if let Err(e) = std::thread::Builder::new()
        .name("retention".into())
        .spawn(move || serve(&data_dir, rx, PERIOD))
    {
        eprintln!("[retention] not started: {e}");
    }
}

/// Run maintenance soon (preference change). Coalesces bursts.
pub fn request() {
    if let Some(tx) = WAKE.get() {
        let _ = tx.try_send(());
    }
}

/// Remove staging leftovers of a crashed engine: tmpfs Whistle files,
/// FIFO dirs and half-written vault files. Call only as engine owner,
/// before dictation starts, so no live staging file can be hit.
pub fn sweep_leftovers(data_dir: &Path) {
    sweep(
        Path::new(crate::transcribe::TMPFS_DIR),
        crate::transcribe::TMPFS_PREFIX,
    );
    sweep(&crate::audio::audio_vault_dir(data_dir), ".tmp");
}

fn sweep(dir: &Path, prefix: &str) {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let uid = unsafe { libc::geteuid() };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(prefix.as_bytes())
        {
            continue;
        }
        // DirEntry metadata never follows a symlink; only our own entries.
        let Ok(meta) = entry.metadata() else { continue };
        if meta.uid() != uid {
            continue;
        }
        let path = entry.path();
        let removed = if meta.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(e) = removed {
            eprintln!("[retention] leftover {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn old(path: &Path) {
        let then = std::time::SystemTime::now() - Duration::from_secs(10 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(then)
            .unwrap();
    }

    /// Nothing is delivered: startup and wake-ups alone expire data.
    #[test]
    fn maintenance_expires_history_and_audio_without_delivery() {
        let data = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(&data.path().join(crate::paths::DB_FILE)).unwrap();
        let mut settings = store.load_settings();
        settings.privacy.transcript_auto_delete_days = 1;
        settings.privacy.audio_retention_days = 7;
        // Recordings made before the switch was turned off still expire.
        settings.privacy.keep_audio_recordings = false;
        store.save_settings(&settings).unwrap();
        let fresh = store.add_history("raw", "fresh", 1, 1, "node").unwrap();
        let record = |uuid: &str| crate::store::HistoryRecord {
            uuid: uuid.into(),
            text: "old".into(),
            raw_text: "old".into(),
            words: 1,
            duration_secs: 1,
            deleted: false,
            discarded: false,
            recorded_at: 1_000,
        };
        store.apply_history(&[record("aged")]).unwrap();
        let vault = crate::audio::audio_vault_dir(data.path());
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(vault.join("aged.wav"), b"RIFF").unwrap();
        std::fs::write(vault.join("fresh.wav"), b"RIFF").unwrap();
        old(&vault.join("aged.wav"));

        // No wake and no period elapses: only the startup pass runs.
        let (wake, rx) = sync_channel(1);
        drop(wake);
        serve(data.path(), rx, Duration::from_secs(3600));
        assert!(!vault.join("aged.wav").exists());
        let live: Vec<String> = store
            .load_history(10, true)
            .unwrap()
            .into_iter()
            .map(|h| h.uuid)
            .collect();
        assert_eq!(live, vec![fresh]);
        assert!(vault.join("fresh.wav").exists());

        // A queued wake (preference change) runs another pass instead of
        // waiting out the period; a closed channel ends the thread.
        std::fs::write(vault.join("later.wav"), b"RIFF").unwrap();
        old(&vault.join("later.wav"));
        let (wake, rx) = sync_channel(1);
        wake.send(()).unwrap();
        drop(wake);
        serve(data.path(), rx, Duration::from_secs(3600));
        assert!(!vault.join("later.wav").exists());
    }

    #[test]
    fn sweep_removes_only_own_prefixed_leftovers_without_following_links() {
        let root = tempfile::tempdir().unwrap();
        let shm = root.path().join("shm");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(shm.join("mywhisper-whistle-abc")).unwrap();
        std::fs::write(shm.join("mywhisper-whistle-abc/audio.wav"), b"x").unwrap();
        std::fs::write(shm.join("mywhisper-whistle-kw-123"), b"words").unwrap();
        std::fs::write(shm.join("unrelated"), b"keep").unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("target"), b"keep").unwrap();
        symlink(&outside, shm.join("mywhisper-whistle-link")).unwrap();
        sweep(&shm, crate::transcribe::TMPFS_PREFIX);
        let mut left: Vec<_> = std::fs::read_dir(&shm)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, vec!["unrelated"]);
        assert!(outside.join("target").exists());

        let vault = crate::audio::audio_vault_dir(root.path());
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(vault.join(".tmpAbC123"), b"partial").unwrap();
        std::fs::write(vault.join("kept.wav"), b"RIFF").unwrap();
        sweep(&vault, ".tmp");
        assert!(!vault.join(".tmpAbC123").exists());
        assert!(vault.join("kept.wav").exists());
    }
}

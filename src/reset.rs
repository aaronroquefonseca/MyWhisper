//! Durable reset ownership across database commit, cleanup and process restart.
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use crate::settings::AppSettings;
use crate::store::Store;

const INTENT: &str = ".reset-intent";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u8,
    nonce: String,
    device: u64,
    inode: u64,
}

pub(crate) fn requested(data: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(data.join(INTENT)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_intent(data: &Path) -> Result<Option<Intent>, String> {
    match crate::private_fs::read_bounded(&data.join(INTENT), 1024) {
        Ok(bytes) => {
            let intent: Intent = serde_json::from_slice(&bytes)
                .map_err(|_| "reset recovery record is invalid; existing data preserved")?;
            if intent.version != 1
                || intent.nonce.len() != 32
                || !intent.nonce.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("reset recovery record is unsupported; existing data preserved".into());
            }
            Ok(Some(intent))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("reset recovery record is unreadable; existing data preserved".into()),
    }
}

fn write_intent(data: &Path, store: &Store) -> Result<Intent, String> {
    let (device, inode) = store
        .file_identity()
        .ok_or("reset needs an existing database")?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| "reset identity could not be generated")?;
    let intent = Intent {
        version: 1,
        nonce: nonce.iter().map(|b| format!("{b:02x}")).collect(),
        device,
        inode,
    };
    persist_intent(data, &intent)?;
    Ok(intent)
}

fn persist_intent(data: &Path, intent: &Intent) -> Result<(), String> {
    // Final installer promotion checks the intent while holding this same lock.
    let control = data.join(".install");
    crate::private_fs::directory(&control).map_err(|_| "reset control directory unavailable")?;
    let _promotion = crate::private_fs::lock(&control.join("metadata.lock"))
        .map_err(|_| "reset could not acquire installation metadata ownership")?;
    crate::private_fs::replace(
        &data.join(INTENT),
        &serde_json::to_vec(&intent).map_err(|_| "reset recovery record could not be encoded")?,
    )
    .map_err(|_| "reset recovery record could not be persisted")?;
    Ok(())
}

fn check_database(db: &Path, store: &Store, intent: &Intent) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let current = std::fs::symlink_metadata(db)
        .map_err(|_| "reset database is unavailable; cleanup record preserved")?;
    if store.file_identity() != Some((intent.device, intent.inode))
        || !current.is_file()
        || current.dev() != intent.device
        || current.ino() != intent.inode
    {
        return Err("reset database was replaced; original cleanup record preserved".into());
    }
    Ok(())
}

fn remove_intent(data: &Path) -> Result<(), String> {
    std::fs::remove_file(data.join(INTENT))
        .and_then(|()| File::open(data)?.sync_all())
        .map_err(|_| "reset completed but its recovery record could not be removed".into())
}

pub(crate) enum Event {
    Snapshot(Box<AppSettings>, Option<&'static str>),
    Finished(Result<(), String>),
}

pub(crate) struct Job {
    pub(crate) events: Receiver<Event>,
    _owner: crate::runtime::SessionTask,
}

/// Admission does no file/database work and never waits on GTK. The owned
/// worker serializes resets; a durable intent keeps operations denied on error.
pub(crate) fn start(db: PathBuf, resume_only: bool) -> Result<Job, String> {
    spawn(db, resume_only, None)
}

fn spawn(db: PathBuf, resume_only: bool, ownership: Option<File>) -> Result<Job, String> {
    let (tx, events) = mpsc::channel();
    let owner = crate::runtime::spawn_session("reset-data", move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let result = run_owned(&db, resume_only, ownership, |store| {
                let settings = store
                    .try_load_settings()
                    .map_err(|_| "reset settings could not be read")?;
                let _ = tx.send(Event::Snapshot(
                    Box::new(settings),
                    store.settings_warning(),
                ));
                Ok(())
            });
            if result.is_err()
                && let Ok(store) = Store::open_existing(&db)
                && let Ok(settings) = store.try_load_settings()
            {
                let _ = tx.send(Event::Snapshot(
                    Box::new(settings),
                    store.settings_warning(),
                ));
            }
            result
        }))
        .unwrap_or_else(|_| Err("reset worker failed; pending cleanup can be retried".into()));
        if let Err(error) = &result {
            eprintln!("[reset] {error}");
        }
        let _ = tx.send(Event::Finished(result));
    })?;
    Ok(Job {
        events,
        _owner: owner,
    })
}

fn run_owned(
    db: &Path,
    resume_only: bool,
    ownership: Option<File>,
    mut snapshot: impl FnMut(&Store) -> Result<(), String>,
) -> Result<(), String> {
    let data = db.parent().ok_or("reset database has no data directory")?;
    let _owner = match ownership {
        Some(owner) => owner,
        None => crate::private_fs::try_lock(&data.join(".reset-operation.lock"))
            .map_err(|_| "another reset owns cleanup or reset ownership is unavailable")?,
    };
    let store = Store::open_existing(db)
        .map_err(|_| "reset database could not be opened; existing data preserved")?;
    let recovery = read_intent(data)?;
    let recovery = match (
        recovery,
        store
            .pending_reset_nonce()
            .map_err(|_| "pending reset could not be read")?,
    ) {
        (None, Some(nonce)) => {
            let (device, inode) = store
                .file_identity()
                .ok_or("reset needs an existing database")?;
            if nonce.len() != 32 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("pending reset identity is invalid; existing data preserved".into());
            }
            let intent = Intent {
                version: 1,
                nonce,
                device,
                inode,
            };
            persist_intent(data, &intent)?;
            Some(intent)
        }
        (intent, _) => intent,
    };
    let intent = match recovery {
        Some(intent) => intent,
        None if resume_only => return snapshot(&store),
        None => write_intent(data, &store)?,
    };
    check_database(db, &store, &intent)?;
    if store
        .completed_reset(&intent.nonce)
        .map_err(|_| "reset completion could not be read")?
    {
        // A crash after commit and before unlink must never reset newer edits.
        remove_intent(data)?;
        return snapshot(&store);
    }
    store
        .begin_reset(&intent.nonce)
        .map_err(|_| "reset transaction could not commit; retry is available")?;
    snapshot(&store)?;
    check_database(db, &store, &intent)?;
    crate::transcribe::invalidate_all_capture();
    crate::transcribe::drop_servers(data);
    crate::stt::clear_download_cache(data)
        .map_err(|_| "reset committed; model cleanup is incomplete and can be retried")?;
    check_database(db, &store, &intent)?;
    crate::audio::clear_audio(data)
        .map_err(|_| "reset committed; audio cleanup is incomplete and can be retried")?;
    check_database(db, &store, &intent)?;
    store
        .complete_reset(&intent.nonce)
        .map_err(|_| "reset cleanup finished; completion could not commit and can be retried")?;
    remove_intent(data)?;
    snapshot(&store)
}

/// The existing service watcher recovers an interrupted reset once it can
/// take ownership. A failed cleanup remains available for an explicit retry;
/// it is not retried every poll or allowed to pin the short-job workers.
#[derive(Default)]
pub(crate) struct Recovery {
    attempted: Option<String>,
}

impl Recovery {
    pub(crate) fn tick(&mut self, db: &Path, store: &Store) {
        let Some(data) = db.parent() else { return };
        let intent = match read_intent(data) {
            Ok(Some(intent)) => intent,
            Ok(None) => match store.pending_reset_nonce() {
                Ok(Some(nonce)) => {
                    let Some((device, inode)) = store.file_identity() else {
                        return;
                    };
                    Intent {
                        version: 1,
                        nonce,
                        device,
                        inode,
                    }
                }
                Ok(None) => {
                    self.attempted = None;
                    return;
                }
                Err(_) => {
                    eprintln!("[reset] pending reset could not be read");
                    return;
                }
            },
            Err(error) => {
                eprintln!("[reset] {error}");
                return;
            }
        };
        if self.attempted.as_deref() == Some(&intent.nonce) {
            return;
        }
        let ownership = match crate::private_fs::try_lock(&data.join(".reset-operation.lock")) {
            Ok(owner) => owner,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(_) => {
                eprintln!("[reset] recovery ownership unavailable");
                return;
            }
        };
        match spawn(db.to_owned(), true, Some(ownership)) {
            Ok(job) => {
                self.attempted = Some(intent.nonce);
                drop(job);
            }
            Err(error) => eprintln!("[reset] recovery admission: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    fn fixture() -> (tempfile::TempDir, PathBuf, Store) {
        let data = tempfile::tempdir().unwrap();
        let db = data.path().join("state.db");
        let store = Store::open(&db).unwrap();
        store
            .add_history(
                "synthetic private raw",
                "synthetic private final",
                3,
                2,
                "synthetic",
            )
            .unwrap();
        store
            .upsert_word("old synthetic word", "synthetic")
            .unwrap();
        (data, db, store)
    }

    fn run(db: &Path, resume: bool) -> Result<(), String> {
        run_owned(db, resume, None, |_| Ok(()))
    }

    #[test]
    fn interrupted_reset_recovers_each_durable_phase_without_wiping_later_edits() {
        for phase in [
            "intent",
            "committed",
            "committed-without-intent",
            "completed",
        ] {
            let (data, db, store) = fixture();
            crate::private_fs::directory(&data.path().join("models")).unwrap();
            std::fs::write(data.path().join("models/old-model"), b"synthetic model").unwrap();
            let intent = write_intent(data.path(), &store).unwrap();
            if phase != "intent" {
                store.begin_reset(&intent.nonce).unwrap();
            }
            if phase == "completed" {
                crate::stt::clear_download_cache(data.path()).unwrap();
                crate::audio::clear_audio(data.path()).unwrap();
                store.complete_reset(&intent.nonce).unwrap();
            }
            if phase == "committed-without-intent" {
                std::fs::remove_file(data.path().join(INTENT)).unwrap();
                assert!(store.reset_pending().unwrap());
            }
            let committed_epoch = store.capture_epoch().unwrap();
            if phase != "intent" {
                store
                    .upsert_word("new synthetic edit", "synthetic")
                    .unwrap();
            }
            drop(store);
            run(&db, true).unwrap();
            let recovered = Store::open_existing(&db).unwrap();
            assert!(!requested(data.path()).unwrap());
            assert!(!recovered.reset_pending().unwrap());
            assert!(!recovered.load_settings().sync.enabled);
            assert!(recovered.load_settings().hotkeys.hotkeys_enabled);
            assert!(
                recovered
                    .export_history()
                    .unwrap()
                    .iter()
                    .all(|row| row.deleted && row.text.is_empty() && row.raw_text.is_empty())
            );
            assert!(!data.path().join("models/old-model").exists());
            if phase != "intent" {
                assert_eq!(recovered.capture_epoch().unwrap(), committed_epoch);
                assert_eq!(recovered.load_words().unwrap(), ["new synthetic edit"]);
            } else {
                assert!(recovered.load_words().unwrap().is_empty());
            }
            // A stale recovery request after completion is a read, not a new reset.
            recovered
                .upsert_word("after complete", "synthetic")
                .unwrap();
            run(&db, true).unwrap();
            assert!(
                recovered
                    .load_words()
                    .unwrap()
                    .contains(&"after complete".to_owned())
            );
        }
    }

    #[test]
    fn cleanup_failure_reports_committed_settings_and_retry_preserves_new_edits() {
        let (data, db, store) = fixture();
        let mut old = store.load_settings();
        old.preferences.ui_language = "es".into();
        old.preferences.theme = crate::settings::Theme::Dark;
        store.save_settings(&old).unwrap();
        std::fs::write(data.path().join("models"), b"synthetic obstruction").unwrap();
        let mut snapshots = vec![];
        let error = run_owned(&db, false, None, |store| {
            snapshots.push(store.try_load_settings().unwrap());
            Ok(())
        })
        .unwrap_err();
        assert!(error.contains("reset committed"));
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].preferences.ui_language, "system");
        assert_eq!(
            snapshots[0].preferences.theme,
            crate::settings::Theme::System
        );
        assert!(!snapshots[0].hotkeys.hotkeys_enabled);
        assert!(!snapshots[0].sync.enabled);
        assert_eq!(
            std::fs::read(data.path().join("models")).unwrap(),
            b"synthetic obstruction"
        );
        let pending = Store::open_existing(&db).unwrap();
        assert_eq!(pending.settings_warning(), None);
        assert!(!pending.load_settings().hotkeys.hotkeys_enabled);
        assert_eq!(
            pending.settings_warning(),
            Some("settings.reset.incomplete")
        );
        pending
            .upsert_word("preserved after failure", "synthetic")
            .unwrap();
        let mut edited = pending.load_settings();
        edited.preferences.ui_language = "es".into();
        edited.preferences.dictation_sounds = false;
        pending.save_settings(&edited).unwrap();
        let epoch = pending.capture_epoch().unwrap();
        std::fs::remove_file(data.path().join("models")).unwrap();
        run(&db, true).unwrap();
        assert_eq!(pending.capture_epoch().unwrap(), epoch);
        assert_eq!(pending.load_words().unwrap(), ["preserved after failure"]);
        assert_eq!(pending.load_settings().preferences.ui_language, "es");
        assert!(!pending.load_settings().preferences.dictation_sounds);
    }

    #[test]
    fn intent_revokes_active_capture_and_prevents_installer_promotion_before_commit() {
        let (data, db, store) = fixture();
        let epoch = store.capture_epoch().unwrap();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let operation =
            crate::operation::Operation::capture(Store::open_existing(&db).unwrap(), epoch, cancel)
                .unwrap();
        let stage = crate::artifact::Stage::new(
            data.path(),
            "synthetic-model",
            data.path().join("models/model"),
        )
        .unwrap();
        std::fs::write(stage.path().join("model"), b"synthetic").unwrap();
        let peer = crate::device::DeviceIdentity::ephemeral().node_id;
        store
            .upsert_peer(&peer, "synthetic trusted peer", "", true)
            .unwrap();
        let sync_epoch = store.sync_epoch();
        assert_eq!(
            store
                .export_authorized(&peer, sync_epoch)
                .unwrap()
                .history
                .len(),
            1
        );
        let intent = write_intent(data.path(), &store).unwrap();
        assert!(store.export_authorized(&peer, sync_epoch).is_err());
        let incoming = [crate::store::WordRecord {
            word: "synthetic denied incoming record".into(),
            updated_at: crate::store::now_ms(),
            updated_by: peer.clone(),
            deleted: false,
        }];
        assert!(
            store
                .apply_sync_round(&peer, sync_epoch, "synthetic", (&incoming, &[], &[]), &[])
                .is_err()
        );
        assert!(!store.load_words().unwrap().contains(&incoming[0].word));
        let capture = crate::store::CapturedUtterance {
            raw: "synthetic newly captured raw",
            text: "synthetic newly captured final",
            duration_secs: 1,
            recorded_at: crate::store::now_ms() / 1000,
            discarded: false,
            wav: Some(b"synthetic audio"),
        };
        assert!(
            !store
                .persist_capture(epoch, capture, &AtomicBool::new(false))
                .unwrap()
        );
        assert_eq!(store.export_history().unwrap().len(), 1);
        assert_eq!(
            store.capture_epoch().unwrap(),
            epoch,
            "fixture is before the DB reset commit"
        );
        assert!(operation.check().is_err());
        assert!(stage.check(&AtomicBool::new(false)).is_err());
        assert!(stage.promote(&AtomicBool::new(false)).is_err());
        assert!(!data.path().join("models/model/model").exists());
        assert!(
            crate::artifact::Stage::new(data.path(), "new-model", data.path().join("models/new"))
                .is_err()
        );
        assert!(!store.load_settings().sync.enabled);
        store.begin_reset(&intent.nonce).unwrap();
        assert!(operation.check().is_err());
    }

    #[test]
    fn admission_and_state_notifications_do_not_wait_for_database_or_installer_locks() {
        for kind in ["database", "installer"] {
            let (data, db, store) = fixture();
            let control = data.path().join(".install");
            crate::private_fs::directory(&control).unwrap();
            let installer = (kind == "installer")
                .then(|| crate::private_fs::lock(&control.join("reset.lock")).unwrap());
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(&format!("PRAGMA key=\"x'{}'\"", "2a".repeat(32)))
                .unwrap();
            if kind == "database" {
                conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            }
            let begin = Instant::now();
            let job = start(db.clone(), false).unwrap();
            assert!(begin.elapsed() < Duration::from_millis(100));
            let until = Instant::now() + Duration::from_secs(2);
            while !requested(data.path()).unwrap() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(requested(data.path()).unwrap());
            if kind == "installer" {
                match job.events.recv_timeout(Duration::from_secs(2)).unwrap() {
                    Event::Snapshot(settings, _) => assert!(!settings.hotkeys.hotkeys_enabled),
                    Event::Finished(_) => {
                        panic!("installer lock should stall cleanup after DB commit")
                    }
                }
                assert!(matches!(
                    job.events.try_recv(),
                    Err(mpsc::TryRecvError::Empty)
                ));
            }
            if kind == "database" {
                conn.execute_batch("ROLLBACK").unwrap();
            }
            drop(installer);
            loop {
                if let Event::Finished(result) =
                    job.events.recv_timeout(Duration::from_secs(3)).unwrap()
                {
                    result.unwrap();
                    break;
                }
            }
            assert!(store.load_words().unwrap().is_empty());
            assert!(!requested(data.path()).unwrap());
        }
    }

    #[test]
    fn malformed_or_foreign_recovery_records_never_authorize_a_new_database_reset() {
        let (data, db, store) = fixture();
        crate::private_fs::replace(&data.path().join(INTENT), b"invalid synthetic record").unwrap();
        assert!(run(&db, true).is_err());
        assert_eq!(store.load_words().unwrap(), ["old synthetic word"]);
        std::fs::remove_file(data.path().join(INTENT)).unwrap();
        let mut intent = write_intent(data.path(), &store).unwrap();
        intent.inode += 1;
        crate::private_fs::replace(
            &data.path().join(INTENT),
            &serde_json::to_vec(&intent).unwrap(),
        )
        .unwrap();
        assert!(run(&db, true).is_err());
        assert_eq!(store.load_words().unwrap(), ["old synthetic word"]);
        assert!(requested(data.path()).unwrap());
        assert!(!store.load_settings().sync.enabled);
    }
    #[test]
    fn reset_owns_cleanup_across_installer_processes_and_killed_coordinators() {
        const HELPER: &str = "MYWHISPER_RESET_TEST_ROLE";
        if let Ok(role) = std::env::var(HELPER) {
            let data = PathBuf::from(std::env::var("MYWHISPER_RESET_TEST_DATA").unwrap());
            let db = data.join("state.db");
            if role == "installer" {
                let stage = crate::artifact::Stage::new(
                    &data,
                    "synthetic-active",
                    data.join("models/active"),
                )
                .unwrap();
                std::fs::write(stage.path().join("model"), b"synthetic model").unwrap();
                std::fs::write(data.join("ready"), b"ready").unwrap();
                let until = Instant::now() + Duration::from_secs(5);
                while !requested(&data).unwrap() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(requested(&data).unwrap());
                assert!(stage.promote(&AtomicBool::new(false)).is_err());
                assert!(
                    crate::artifact::Stage::new(&data, "synthetic-late", data.join("models/late"))
                        .is_err()
                );
                return;
            }
            let _ownership =
                crate::private_fs::try_lock(&data.join(".reset-operation.lock")).unwrap();
            let store = Store::open_existing(&db).unwrap();
            let intent = write_intent(&data, &store).unwrap();
            if role != "intent" {
                store.begin_reset(&intent.nonce).unwrap();
            }
            if role == "completed" {
                crate::stt::clear_download_cache(&data).unwrap();
                crate::audio::clear_audio(&data).unwrap();
                store.complete_reset(&intent.nonce).unwrap();
            }
            if role == "committed-missing" {
                std::fs::remove_file(data.join(INTENT)).unwrap();
            }
            std::fs::write(data.join("ready"), b"ready").unwrap();
            loop {
                std::thread::park();
            }
        }
        for role in [
            "installer",
            "intent",
            "committed",
            "committed-missing",
            "completed",
        ] {
            let (data, db, store) = fixture();
            let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
            cmd.args(["--exact", "reset::tests::reset_owns_cleanup_across_installer_processes_and_killed_coordinators", "--nocapture"])
                .env(HELPER, role).env("MYWHISPER_RESET_TEST_DATA", data.path())
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::inherit());
            let child = crate::child::OwnedChild::spawn(&mut cmd).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !data.path().join("ready").exists() && Instant::now() < deadline {
                assert!(
                    child.try_status().unwrap().is_none(),
                    "reset fixture exited before barrier"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(data.path().join("ready").exists());
            if role == "installer" {
                run(&db, false).unwrap();
                let deadline = Instant::now() + Duration::from_secs(1);
                let status = loop {
                    if let Some(status) = child.try_status().unwrap() {
                        break status;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "installer did not relinquish reset ownership"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                };
                assert!(status.success());
                assert!(!data.path().join("models/active").exists());
                assert!(!data.path().join("models/late").exists());
                drop(child);
            } else {
                let epoch = store.capture_epoch().unwrap();
                if role != "intent" {
                    store
                        .upsert_word("edit after reset commit", "synthetic")
                        .unwrap();
                }
                // OwnedChild kills and reaps the separate process, releasing its
                // lock and descriptors without executing normal cleanup.
                drop(child);
                let mut recovery = Recovery::default();
                recovery.tick(&db, &store);
                let until = Instant::now() + Duration::from_secs(3);
                while store.reset_pending().unwrap() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(
                    !store.reset_pending().unwrap(),
                    "service did not recover killed coordinator"
                );
                if role != "intent" {
                    assert_eq!(store.capture_epoch().unwrap(), epoch);
                    assert_eq!(store.load_words().unwrap(), ["edit after reset commit"]);
                }
            }
            assert!(
                store
                    .export_history()
                    .unwrap()
                    .iter()
                    .all(|row| row.deleted && row.text.is_empty() && row.raw_text.is_empty())
            );
        }
    }

    #[test]
    fn database_replacement_after_commit_denies_old_reset_filesystem_cleanup() {
        let (data, db, store) = fixture();
        let replacement = tempfile::tempdir().unwrap();
        let other_db = replacement.path().join("state.db");
        let other = Store::open(&other_db).unwrap();
        other.upsert_word("new database data", "synthetic").unwrap();
        other.checkpoint().unwrap();
        drop(other);
        crate::private_fs::directory(&data.path().join("models")).unwrap();
        std::fs::write(data.path().join("models/keep"), b"new installation").unwrap();
        let error = run_owned(&db, false, None, |_| {
            std::fs::rename(&other_db, &db).unwrap();
            Ok(())
        })
        .unwrap_err();
        assert!(error.contains("replaced"));
        assert_eq!(
            std::fs::read(data.path().join("models/keep")).unwrap(),
            b"new installation"
        );
        assert!(requested(data.path()).unwrap());
        drop(store);
    }
    #[test]
    fn schema_five_upgrade_preserves_records_and_conservatively_restores_pending_reset() {
        let (data, db, store) = fixture();
        let epoch = store.capture_epoch().unwrap();
        let nonce = "11".repeat(16);
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(&format!("PRAGMA key=\"x'{}'\"", "2a".repeat(32)))
            .unwrap();
        let raw: String = conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["synthetic_future_setting"] = serde_json::json!({"preserve":true});
        conn.execute(
            "UPDATE kv SET value=?1 WHERE key='settings'",
            [value.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO kv(key,value) VALUES('reset_pending',?1)",
            [&nonce],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version=5").unwrap();
        drop(conn);
        drop(store);
        let upgraded = Store::open_existing(&db).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(&format!("PRAGMA key=\"x'{}'\"", "2a".repeat(32)))
            .unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            7
        );
        let preserved: String = conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&preserved).unwrap()["synthetic_future_setting"],
            value["synthetic_future_setting"]
        );
        assert_eq!(upgraded.load_words().unwrap(), ["old synthetic word"]);
        assert_eq!(
            upgraded.export_history().unwrap()[0].text,
            "synthetic private final"
        );
        assert!(!upgraded.load_settings().hotkeys.hotkeys_enabled);
        assert!(!upgraded.load_settings().sync.enabled);
        assert_eq!(upgraded.capture_epoch().unwrap(), epoch);
        run(&db, true).unwrap();
        assert!(!requested(data.path()).unwrap());
        assert_eq!(upgraded.load_words().unwrap(), ["old synthetic word"]);
        assert_eq!(
            upgraded.export_history().unwrap()[0].text,
            "synthetic private final"
        );
        assert_eq!(
            upgraded.capture_epoch().unwrap(),
            epoch,
            "pending recovery must not repeat a committed DB reset"
        );
    }
}

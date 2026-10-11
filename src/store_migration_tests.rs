//! Real SQLite migration ownership, with synthetic disposable data only.
use super::*;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::sync::mpsc;
use std::time::Duration;

const KEY: [u8; 32] = [81; 32];

fn seed(path: &Path) {
    let plain = Connection::open(path).unwrap();
    plain.execute_batch(SCHEMA).unwrap();
    plain
        .execute_batch("PRAGMA user_version=3; PRAGMA journal_mode=WAL;")
        .unwrap();
    plain
        .execute(
            "INSERT INTO words(word) VALUES('synthetic original 世界')",
            [],
        )
        .unwrap();
    plain.execute("INSERT INTO snippets(trigger,replacement) VALUES('synthetic-trigger','synthetic snippet 世界')", []).unwrap();
    plain.execute("INSERT INTO history(uuid,text,raw_text,words,duration_secs) VALUES('synthetic-history','synthetic private sentence 世界','synthetic raw 世界',4,3)", []).unwrap();
    let mut settings = AppSettings::default();
    settings.preferences.theme = crate::settings::Theme::Dark;
    settings.sync.enabled = false;
    settings.hotkeys.hotkeys_enabled = false;
    let mut value = serde_json::to_value(settings).unwrap();
    value["settings_version"] = serde_json::Value::from(1);
    value["future_root"] = serde_json::json!({"synthetic": "preserved"});
    plain
        .execute(
            "INSERT INTO kv(key,value) VALUES('settings',?1)",
            [value.to_string()],
        )
        .unwrap();
}

fn verify(path: &Path) {
    let store = Store::open_with_key(path, &KEY).unwrap();
    assert_eq!(store.load_words().unwrap(), ["synthetic original 世界"]);
    let snippets = store.load_snippets().unwrap();
    assert_eq!(snippets.len(), 1);
    assert_eq!(snippets[0].replacement, "synthetic snippet 世界");
    let history = store.export_history().unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, "synthetic private sentence 世界");
    assert_eq!(history[0].raw_text, "synthetic raw 世界");
    let settings = store.load_settings();
    assert_eq!(settings.preferences.theme, crate::settings::Theme::Dark);
    assert!(!settings.sync.enabled && !settings.hotkeys.hotkeys_enabled);
    let raw: String = store
        .conn
        .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&raw).unwrap()["future_root"]["synthetic"],
        "preserved"
    );
    store.checkpoint().unwrap();
    assert!(!std::fs::read(path).unwrap().starts_with(b"SQLite format 3"));
    assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o777, 0o600);
}

fn wait_status(child: &crate::child::OwnedChild) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_status().unwrap() {
            return status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "owned migration child did not complete"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn legacy_writer_cannot_commit_between_export_and_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let legacy = Connection::open(&path).unwrap();
    legacy.execute_batch(SCHEMA).unwrap();
    legacy
        .execute("INSERT INTO words(word) VALUES('synthetic original')", [])
        .unwrap();
    legacy.busy_timeout(Duration::ZERO).unwrap();
    let (ready, reached) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let owned_path = path.clone();
    let migration = std::thread::spawn(move || {
        let target = owned_path.clone();
        crate::private_fs::replacement_fault::install(move |phase, path| {
            if phase == "migration-file-synced" && path == target {
                ready.send(()).unwrap();
                resumed.recv_timeout(Duration::from_secs(3)).unwrap();
            }
        });
        Store::open_with_key(&owned_path, &[81; 32])
            .unwrap()
            .load_words()
            .unwrap()
    });
    reached.recv_timeout(Duration::from_secs(3)).unwrap();
    let late = legacy.execute("INSERT INTO words(word) VALUES('synthetic late')", []);
    resume.send(()).unwrap();
    let words = migration.join().unwrap();
    assert_eq!(words, ["synthetic original"]);
    assert!(
        late.is_err(),
        "migration acknowledged a legacy write that disappeared at replacement"
    );
    assert!(
        legacy
            .execute("INSERT INTO words(word) VALUES('synthetic stale')", [])
            .is_err(),
        "a stale legacy handle must not acknowledge writes after replacement"
    );
    assert_eq!(
        Store::open_with_key(&path, &[81; 32])
            .unwrap()
            .load_words()
            .unwrap(),
        words
    );
}

#[test]
fn killed_plaintext_migration_recovers_with_competing_fresh_processes() {
    const TEST: &str = "store::migration_tests::killed_plaintext_migration_recovers_with_competing_fresh_processes";
    if let Some(root) = std::env::var_os("MYWHISPER_MIGRATION_TEST_ROOT") {
        let root = std::path::PathBuf::from(root);
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("mywhisper-migration-audit.")
        );
        let metadata = root.symlink_metadata().unwrap();
        assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
        assert_eq!(metadata.uid(), unsafe { libc::getuid() });
        assert_eq!(metadata.mode() & 0o777, 0o700);
        assert!(root.join(".disposable").is_file());
        let action = std::env::var("MYWHISPER_MIGRATION_TEST_ACTION").unwrap();
        let path = root.join("state.db");
        if let Some(phase) = action.strip_prefix("stop:") {
            let phase = phase.to_owned();
            let target = path.clone();
            let ready = root.join("ready");
            crate::private_fs::replacement_fault::install(move |point, path| {
                if point == phase && path == target {
                    let mut marker = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&ready)
                        .unwrap();
                    marker.write_all(b"owned migration reached").unwrap();
                    marker.sync_all().unwrap();
                    std::thread::sleep(Duration::from_secs(5));
                    panic!("migration barrier was not killed by its owner");
                }
            });
        }
        verify(&path);
        return;
    }
    for phase in [
        "migration-created",
        "migration-exported",
        "migration-file-synced",
        "migration-renamed",
        "migration-directory-synced",
    ] {
        let root = tempfile::Builder::new()
            .prefix("mywhisper-migration-audit.")
            .tempdir()
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.path().join(".disposable"), b"").unwrap();
        seed(&root.path().join("state.db"));
        let child = |action: &str| {
            let output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(root.path().join(format!("{action}.log")))
                .unwrap();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", TEST, "--nocapture"])
                .env("MYWHISPER_MIGRATION_TEST_ROOT", root.path())
                .env("MYWHISPER_MIGRATION_TEST_ACTION", action)
                .stdout(output.try_clone().unwrap())
                .stderr(output);
            crate::child::OwnedChild::spawn(&mut command).unwrap()
        };
        let mut interrupted = child(&format!("stop:{phase}"));
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !root.path().join("ready").exists() {
            assert!(
                interrupted.try_status().unwrap().is_none(),
                "child exited before migration barrier: {phase}"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "migration barrier was not reached: {phase}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        interrupted.0.kill().unwrap();
        assert_eq!(wait_status(&interrupted).signal(), Some(libc::SIGKILL));
        drop(interrupted);
        let recoveries = (0..4)
            .map(|index| child(&format!("recover-{index}")))
            .collect::<Vec<_>>();
        for recovery in recoveries {
            assert!(
                wait_status(&recovery).success(),
                "fresh recovery failed: {phase}"
            );
        }
        verify(&root.path().join("state.db"));
        for entry in std::fs::read_dir(root.path()).unwrap() {
            let entry = entry.unwrap();
            if !entry.file_name().to_string_lossy().starts_with(".tmp") {
                continue;
            }
            let metadata = entry.metadata().unwrap();
            assert!(metadata.is_file());
            assert_eq!(metadata.mode() & 0o777, 0o600);
            let bytes = std::fs::read(entry.path()).unwrap();
            assert!(
                !bytes
                    .windows(b"synthetic private sentence".len())
                    .any(|window| window == b"synthetic private sentence")
            );
        }
        eprintln!(
            "Killed migration recovered: {phase}; four competing processes; Unicode/settings preserved"
        );
    }
}

#[test]
fn unsupported_plaintext_schema_preserves_its_original_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    seed(&path);
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("PRAGMA user_version=99;").unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    assert!(Store::open_with_key(&path, &KEY).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn legacy_wal_reader_blocks_migration_without_losing_committed_data() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    seed(&path);
    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("BEGIN; SELECT word FROM words;")
        .unwrap();
    let writer = Connection::open(&path).unwrap();
    writer
        .execute(
            "INSERT INTO words(word) VALUES('synthetic committed WAL')",
            [],
        )
        .unwrap();
    let error = Store::open_with_key(&path, &KEY)
        .err()
        .expect("active reader allowed migration");
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy)
    );
    assert!(
        std::fs::read(&path)
            .unwrap()
            .starts_with(b"SQLite format 3")
    );
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    drop(writer);
    let store = Store::open_with_key(&path, &KEY).unwrap();
    assert_eq!(
        store.load_words().unwrap(),
        ["synthetic committed WAL", "synthetic original 世界"]
    );
    assert_eq!(
        store.export_history().unwrap()[0].text,
        "synthetic private sentence 世界"
    );
}

#[test]
fn special_database_files_fail_without_waiting_for_a_writer() {
    const TEST: &str =
        "store::migration_tests::special_database_files_fail_without_waiting_for_a_writer";
    if let Some(root) = std::env::var_os("MYWHISPER_DBTYPE_TEST_ROOT") {
        let root = std::path::PathBuf::from(root);
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("mywhisper-dbtype-audit.")
        );
        let metadata = root.symlink_metadata().unwrap();
        assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
        assert_eq!(metadata.uid(), unsafe { libc::getuid() });
        assert_eq!(metadata.mode() & 0o777, 0o700);
        assert!(root.join(".disposable").is_file());
        let started = std::time::Instant::now();
        for name in ["fifo.db", "directory.db", "link.db"] {
            let path = root.join(name);
            assert!(
                Store::open_with_key(&path, &KEY).is_err(),
                "invalid database type accepted: {name}"
            );
            assert!(
                Store::open_existing(&path).is_err(),
                "invalid existing database accepted: {name}"
            );
        }
        assert!(started.elapsed() < Duration::from_millis(250));
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("mywhisper-dbtype-audit.")
        .tempdir()
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(root.path().join(".disposable"), b"").unwrap();
    let fifo = root.path().join("fifo.db");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    std::fs::create_dir(root.path().join("directory.db")).unwrap();
    let target = root.path().join("preserved.txt");
    std::fs::write(&target, b"synthetic preserved target").unwrap();
    std::os::unix::fs::symlink(&target, root.path().join("link.db")).unwrap();
    let target_before = std::fs::metadata(&target).unwrap();
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.path().join("child.log"))
        .unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", TEST, "--nocapture"])
        .env("MYWHISPER_DBTYPE_TEST_ROOT", root.path())
        .stdout(output.try_clone().unwrap())
        .stderr(output);
    let child = crate::child::OwnedChild::spawn(&mut command).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if let Some(status) = child.try_status().unwrap() {
            assert!(
                status.success(),
                "{}",
                std::fs::read_to_string(root.path().join("child.log")).unwrap()
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "database open waited for a writer on a private FIFO"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(child);
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"synthetic preserved target"
    );
    let target_after = std::fs::metadata(&target).unwrap();
    assert_eq!(target_before.mode(), target_after.mode());
    assert_eq!(target_before.ino(), target_after.ino());
    assert_eq!(
        std::fs::symlink_metadata(fifo).unwrap().mode() & 0o777,
        0o600
    );
}

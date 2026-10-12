//! Encrypted local state and record-level synchronization.
//!
//! `state.db` holds settings, dictionary, snippets, history and sync
//! metadata. An active WAL database must not be copied as a standalone
//! file: committed writes may be in its WAL. Network sync exchanges
//! authorized typed records, not database files or encryption keys.
//!
//! Sync model: every mutable record carries `(updated_at_ms,
//! updated_by, deleted)`. Two devices that diverged from a common
//! version exchange record sets; the union converges and concurrent
//! edits to the same key resolve Last-Writer-Wins on
//! `(updated_at_ms, updated_by)`. Dictionary deletions use that LWW
//! rule; history deletion/discard flags are monotonic and redact payloads.

use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::settings::{AppSettings, HistoryEntry, Snippet};

#[path = "sync_pages.rs"]
mod pages;
pub(crate) use pages::{SyncManifest, SyncPage};
#[path = "sync_segments.rs"]
mod segments;
pub(crate) use segments::{SyncCursor, SyncPacket};

/// Milliseconds since the Unix epoch (wall clock). Sync conflict
/// resolution compares these; ties break on node id (total order).
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

const SCHEMA: &str = "
PRAGMA foreign_keys = ON;
PRAGMA secure_delete = ON;
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS words (
    word TEXT PRIMARY KEY,
    updated_at INTEGER NOT NULL DEFAULT 0,
    updated_by TEXT NOT NULL DEFAULT '',
    deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS snippets (
    trigger TEXT PRIMARY KEY,
    replacement TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL DEFAULT 0,
    updated_by TEXT NOT NULL DEFAULT '',
    deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS history (
    uuid TEXT PRIMARY KEY,
    text TEXT NOT NULL,
    raw_text TEXT NOT NULL DEFAULT '',
    words INTEGER NOT NULL DEFAULT 0,
    duration_secs INTEGER NOT NULL DEFAULT 0,
    deleted INTEGER NOT NULL DEFAULT 0,
    discarded INTEGER NOT NULL DEFAULT 0,
    recorded_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);
CREATE TABLE IF NOT EXISTS devices (
    node_id TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    address TEXT NOT NULL DEFAULT '',
    addresses TEXT NOT NULL DEFAULT '[]',
    trusted INTEGER NOT NULL DEFAULT 0,
    pending INTEGER NOT NULL DEFAULT 0,
    introducer INTEGER NOT NULL DEFAULT 0,
    last_seen INTEGER NOT NULL DEFAULT 0,
    last_sync INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS revoked_devices (node_id TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS sync_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Insights read pattern: per-day aggregates over a trailing window.
-- Indexed so it stays a single range scan at any realistic volume.
CREATE INDEX IF NOT EXISTS history_recorded ON history (recorded_at);
";

/// Monotonic data generation: bumped on every mutation (local CRUD
/// and incoming merges). UI pages poll it to refresh lists that
/// changed under them (e.g. words arriving from a sync round).
pub static DATA_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn data_gen() -> u64 {
    DATA_GEN.load(std::sync::atomic::Ordering::SeqCst)
}

fn bump_data_gen() {
    DATA_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Cross-process change token: an explicit counter in `sync_meta`,
/// bumped on every mutation (local CRUD and incoming merges), read
/// fresh on every poll. Unlike `PRAGMA data_version` (which proved
/// unreliable: stale on the writing connection, frozen on `:memory:`),
/// this is plain data and works identically everywhere. Pair with the
/// process-local `data_gen` in UI refresh polls.
pub fn read_gen(store: &Store) -> i64 {
    store
        .conn
        .query_row("SELECT value FROM sync_meta WHERE key = 'gen'", [], |r| {
            r.get::<_, String>(0)
        })
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn bump_gen(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sync_meta (key, value) VALUES ('gen', '1')
         ON CONFLICT (key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
        [],
    )?;
    Ok(())
}

pub struct Store {
    conn: Connection,
    file_identity: Option<(u64, u64)>,
    settings_baseline: std::cell::RefCell<Option<serde_json::Value>>,
    settings_warning: std::cell::Cell<Option<&'static str>>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct ContentSnapshot {
    #[serde(default)]
    pub words: Vec<WordRecord>,
    #[serde(default)]
    pub snippets: Vec<SnippetRecord>,
    #[serde(default)]
    pub history: Vec<HistoryRecord>,
    #[serde(default)]
    pub peers: Vec<PeerGossip>,
}

/// One finalized capture; sensitive payloads are accepted only under the
/// current privacy policy and the capture's original reset/privacy epoch.
pub(crate) struct CapturedUtterance<'a> {
    pub raw: &'a str,
    pub text: &'a str,
    pub duration_secs: i64,
    pub recorded_at: i64,
    pub discarded: bool,
    pub wav: Option<&'a [u8]>,
}

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    conn.prepare(&format!("SELECT {column} FROM {table} LIMIT 0"))
        .is_ok()
}

/// Database encryption key (32 bytes). Order: explicit env override
/// (headless/testing) first, otherwise the OS keyring, creating and
/// storing a random key on first run.
fn db_key(path: &Path, existing_encrypted: bool) -> Result<[u8; 32], String> {
    if cfg!(test) {
        return Ok([42u8; 32]);
    }
    if let Ok(hexkey) = std::env::var("MYWHISPER_DB_KEY") {
        return hex_decode_key(&hexkey)
            .ok_or_else(|| "MYWHISPER_DB_KEY must be 64 hex chars".to_string());
    }
    let paths =
        crate::paths::AppPaths::create().map_err(|e| format!("key provisioning path: {e}"))?;
    let _lock = crate::private_fs::lock(&paths.config_dir.join("db-key.lock"))
        .map_err(|e| format!("key provisioning lock: {e}"))?;
    let keys = crate::database_key::candidates(existing_encrypted)?;
    select_database_key(path, existing_encrypted, &keys)
}

/// A write-once interrupted provision can leave multiple secrets. Existing
/// encrypted data identifies its correct key without modifying the database
/// or any keyring item; new/plaintext databases require a unique candidate.
fn select_database_key(
    path: &Path,
    existing_encrypted: bool,
    keys: &[[u8; 32]],
) -> Result<[u8; 32], String> {
    if keys.len() == 1 {
        return Ok(keys[0]);
    }
    if !existing_encrypted || keys.is_empty() || keys.len() > 8 {
        return Err(
            "ambiguous database keyring entries; repair duplicates before provisioning".into(),
        );
    }
    let mut matches = Vec::new();
    for key in keys {
        let connection =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|_| "cannot inspect database key candidates")?;
        connection
            .busy_timeout(std::time::Duration::from_millis(250))
            .map_err(|_| "cannot bound database key inspection")?;
        let valid = connection
            .execute_batch(&key_pragma(key))
            .and_then(|()| {
                connection.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
                    row.get::<_, i64>(0)
                })
            })
            .is_ok();
        if valid && !matches.contains(key) {
            matches.push(*key);
        }
    }
    match matches.as_slice() {
        [key] => Ok(*key),
        _ => Err(
            "database key candidates cannot uniquely decrypt existing data; restore the original secret"
                .into(),
        ),
    }
}
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

pub(crate) fn hex_decode_key(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = (hex_val(b[2 * i])? << 4) | hex_val(b[2 * i + 1])?;
    }
    Some(out)
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn key_pragma(key: &[u8; 32]) -> String {
    format!("PRAGMA key = \"x'{}'\"", hex_encode(key))
}

/// Encrypt a legacy plaintext database in place via a temp file:
/// attach fresh encrypted db, export, atomically replace.
fn migrate_plaintext_to_encrypted(path: &Path, key: &[u8; 32]) -> rusqlite::Result<()> {
    let staging = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
    #[cfg(test)]
    crate::private_fs::replacement_fault::point("migration-created", path);
    let tmp = staging.path();
    let mut plain = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    plain.busy_timeout(std::time::Duration::from_secs(5))?;
    let initial_version: i64 = plain.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if initial_version > 8 {
        return Err(rusqlite::Error::ToSqlConversionFailure(
            "database schema is newer than this build".into(),
        ));
    }
    // A successful checkpoint is required before changing journal mode. A
    // legacy reader/writer can prevent this; preserve the source and retry.
    let busy: i64 = plain.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
    if busy != 0 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("legacy database checkpoint blocked; close the older app and retry".into()),
        ));
    }
    let mode: String = plain.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
    if mode != "delete" {
        return Err(rusqlite::Error::ToSqlConversionFailure(
            "legacy database journal mode prevents safe migration".into(),
        ));
    }
    // EXCLUSIVE locking mode retains the source lock after COMMIT, through
    // DETACH, fsync and rename. A transaction alone releases it too early.
    plain.execute_batch("PRAGMA locking_mode=EXCLUSIVE;")?;
    let attach = format!(
        "ATTACH DATABASE '{}' AS enc KEY \"x'{}'\"",
        tmp.to_string_lossy().replace('\'', "''"),
        hex_encode(key)
    );
    plain.execute_batch(&attach)?;
    let tx = plain.transaction_with_behavior(rusqlite::TransactionBehavior::Exclusive)?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > 8 {
        return Err(rusqlite::Error::ToSqlConversionFailure(
            "database schema is newer than this build".into(),
        ));
    }
    tx.query_row("SELECT sqlcipher_export('enc')", [], |_| Ok(()))?;
    tx.execute_batch(&format!("PRAGMA enc.user_version={version}"))?;
    tx.commit()?;
    plain.execute_batch("DETACH enc")?;
    #[cfg(test)]
    crate::private_fs::replacement_fault::point("migration-exported", path);
    let verified = Connection::open_with_flags(
        tmp,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    verified.execute_batch(&key_pragma(key))?;
    let integrity: String = verified.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(rusqlite::Error::ToSqlConversionFailure(
            "encrypted migration verification failed; original database preserved".into(),
        ));
    }
    drop(verified);
    staging
        .as_file()
        .sync_all()
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
    #[cfg(test)]
    crate::private_fs::replacement_fault::point("migration-file-synced", path);
    std::fs::rename(tmp, path)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(format!("replace db: {e}").into()))?;
    #[cfg(test)]
    crate::private_fs::replacement_fault::point("migration-renamed", path);
    std::fs::File::open(path.parent().unwrap_or(Path::new(".")))
        .and_then(|parent| parent.sync_all())
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
    #[cfg(test)]
    crate::private_fs::replacement_fault::point("migration-directory-synced", path);
    drop(plain);
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let _lock = crate::private_fs::lock(&path.with_extension("db.init.lock"))
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let plaintext = Self::plaintext_header(path)?;
        let existing_encrypted = path.exists()
            && std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(true)
            && !plaintext;
        let key = db_key(path, existing_encrypted)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(format!("db key: {e}").into()))?;
        Self::initialize(path, &key, plaintext, true)
    }

    /// Background maintenance must not recreate a database removed after its
    /// work was scheduled. Both file and SQLite opens exclude creation.
    pub(crate) fn open_existing(path: &Path) -> rusqlite::Result<Self> {
        let existing = crate::private_fs::open_regular(path)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let _lock = crate::private_fs::lock(&path.with_extension("db.init.lock"))
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let plaintext = Self::plaintext_header(path)?;
        let nonempty = existing
            .metadata()
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?
            .len()
            > 0;
        let key = db_key(path, nonempty && !plaintext)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(format!("db key: {e}").into()))?;
        Self::initialize(path, &key, plaintext, false)
    }

    /// Explicit encryption key for disposable databases and controlled recovery.
    pub fn open_with_key(path: &Path, key: &[u8; 32]) -> rusqlite::Result<Self> {
        let _lock = crate::private_fs::lock(&path.with_extension("db.init.lock"))
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        Self::initialize(path, key, Self::plaintext_header(path)?, true)
    }

    fn plaintext_header(path: &Path) -> rusqlite::Result<bool> {
        use std::io::Read;
        let mut file = match crate::private_fs::open_regular(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(rusqlite::Error::ToSqlConversionFailure(e.into())),
        };
        let mut header = [0u8; 16];
        let n = file
            .read(&mut header)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        Ok(n == 16 && &header == b"SQLite format 3\0")
    }

    fn initialize(
        path: &Path,
        key: &[u8; 32],
        plaintext: bool,
        create: bool,
    ) -> rusqlite::Result<Self> {
        if plaintext {
            migrate_plaintext_to_encrypted(path, key)?;
        }
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let metadata = file
            .metadata()
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let file_identity = Some((metadata.dev(), metadata.ino()));
        drop(file);
        let mut flags =
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        if create {
            flags |= rusqlite::OpenFlags::SQLITE_OPEN_CREATE;
        }
        let conn = Connection::open_with_flags(path, flags)?;
        let current = std::fs::symlink_metadata(path)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        if !current.is_file() || Some((current.dev(), current.ino())) != file_identity {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "database changed while opening; retry".into(),
            ));
        }
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(&key_pragma(key))?;
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 8 {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "database schema is newer than this MyWhisper build".into(),
            ));
        }
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA secure_delete=ON; PRAGMA foreign_keys=ON;",
        )?;
        if version == 8 {
            return Ok(Self {
                conn,
                file_identity,
                settings_baseline: std::cell::RefCell::new(None),
                settings_warning: std::cell::Cell::new(None),
            });
        }
        let fresh = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get::<_, i64>(0),
        )? == 0;
        let tx = conn.unchecked_transaction()?;
        if (3..=7).contains(&version) {
            tx.execute(
                "UPDATE history SET text='',raw_text='',words=0 WHERE discarded=1",
                [],
            )?;
            Self::initialize_settings_on(&tx, false)?;
            pages::initialize(&tx)?;
            segments::initialize(&tx)?;
            tx.execute_batch("PRAGMA user_version=8")?;
            tx.commit()?;
            return Ok(Self {
                conn,
                file_identity,
                settings_baseline: std::cell::RefCell::new(None),
                settings_warning: std::cell::Cell::new(None),
            });
        }
        tx.execute_batch(SCHEMA)?;
        // Best-effort migration for pre-existing databases: SQLite has
        // no ADD COLUMN IF NOT EXISTS, so a duplicate-column failure
        // just means the column is already there.
        for stmt in [
            "ALTER TABLE words ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE words ADD COLUMN updated_by TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE words ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE snippets ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE snippets ADD COLUMN updated_by TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE snippets ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE snippets ADD COLUMN replacement TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE history ADD COLUMN duration_secs INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE history ADD COLUMN raw_text TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE history ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE history ADD COLUMN discarded INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE devices ADD COLUMN address TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE devices ADD COLUMN addresses TEXT NOT NULL DEFAULT '[]'",
            "ALTER TABLE devices ADD COLUMN trusted INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE devices ADD COLUMN pending INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE devices ADD COLUMN introducer INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE devices ADD COLUMN last_sync INTEGER NOT NULL DEFAULT 0",
        ] {
            let tokens: Vec<_> = stmt.split_whitespace().collect();
            if !has_column(&tx, tokens[2], tokens[5]) {
                tx.execute(stmt, [])?;
            }
        }
        // History gained a uuid primary key after v1 (which used an
        // integer id). Backfill deterministic uuids for legacy rows so
        // old databases keep working.
        if !has_column(&tx, "history", "uuid") {
            tx.execute("ALTER TABLE history ADD COLUMN uuid TEXT", [])?;
            tx.execute(
                "UPDATE history SET uuid = 'legacy-' || id || '-' || recorded_at
                 WHERE uuid IS NULL",
                [],
            )?;
        }
        tx.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS history_uuid ON history (uuid)",
            [],
        )?;
        // Rows predating raw_text carry the final text as raw (the
        // pipeline was passthrough then, so this is exact).
        tx.execute(
            "UPDATE history SET raw_text = text WHERE raw_text = '' AND deleted = 0",
            [],
        )?;
        tx.execute(
            "UPDATE history SET text='', raw_text='', words=0, duration_secs=0 WHERE deleted=1",
            [],
        )?;
        tx.execute(
            "UPDATE history SET text='',raw_text='',words=0 WHERE discarded=1",
            [],
        )?;
        tx.execute("UPDATE devices SET introducer=0", [])?;
        Self::initialize_settings_on(&tx, fresh)?;
        pages::initialize(&tx)?;
        segments::initialize(&tx)?;
        tx.execute_batch("PRAGMA user_version=8")?;
        tx.commit()?;
        Ok(Self {
            conn,
            file_identity,
            settings_baseline: std::cell::RefCell::new(None),
            settings_warning: std::cell::Cell::new(None),
        })
    }

    /// Checkpoint the WAL back into the database file. Call on clean
    /// shutdown so replicators always see a complete file.
    pub(crate) fn path(&self) -> Option<&Path> {
        self.conn.path().filter(|p| !p.is_empty()).map(Path::new)
    }

    pub(crate) fn sync_epoch(&self) -> i64 {
        self.conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM sync_meta WHERE key='policy_epoch'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    fn invalidate_sync(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute("INSERT INTO sync_meta(key,value) VALUES ('policy_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1",[])?;
        Ok(())
    }

    pub(crate) fn capture_epoch(&self) -> rusqlite::Result<i64> {
        Ok(self
            .conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM sync_meta WHERE key='capture_epoch'",
                [],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// A capture authority keeps a dedicated existing connection for quick
    /// policy checks. It cannot write, migrate or wait behind a busy writer.
    pub(crate) fn file_identity(&self) -> Option<(u64, u64)> {
        self.file_identity
    }

    pub(crate) fn policy_reads_only(&self) -> rusqlite::Result<()> {
        self.conn.busy_timeout(std::time::Duration::ZERO)?;
        self.conn.execute_batch("PRAGMA query_only=ON")
    }

    /// Read the input configuration and its durable authority from one
    /// snapshot. Dictionary/history enumeration belongs to finalization.
    pub(crate) fn capture_settings(&self) -> rusqlite::Result<(AppSettings, i64)> {
        let snapshot = self.conn.unchecked_transaction()?;
        let raw: String =
            snapshot.query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                row.get(0)
            })?;
        let (value, _) = Self::decode_settings_value(&raw).map_err(|error| {
            rusqlite::Error::ToSqlConversionFailure(crate::diagnostics::json(&error).into())
        })?;
        let mut settings: AppSettings = serde_json::from_value(value).map_err(|error| {
            rusqlite::Error::ToSqlConversionFailure(crate::diagnostics::json(&error).into())
        })?;
        settings.stt.ensure_seed();
        if matches!(
            settings.stt.local_provider,
            crate::settings::LocalSttProvider::Nvidia
        ) && settings.stt.parakeet_model == "cohere-transcribe-03-2026"
        {
            settings.stt.local_provider = crate::settings::LocalSttProvider::Cohere;
        }
        if self.reset_pending()? {
            settings.hotkeys.hotkeys_enabled = false;
        }
        let epoch = self.capture_epoch()?;
        snapshot.commit()?;
        Ok((settings, epoch))
    }

    fn invalidate_capture(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute("INSERT INTO sync_meta(key,value) VALUES ('capture_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1", [])?;
        Ok(())
    }

    pub fn checkpoint(&self) -> rusqlite::Result<()> {
        let busy: i64 = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        if busy != 0 {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("checkpoint blocked by an active reader".into()),
            ));
        }
        Ok(())
    }

    // -- settings (JSON blob under kv['settings']) -------------------------

    fn safe_settings() -> AppSettings {
        let mut safe = AppSettings::default();
        safe.sync.enabled = false;
        safe.sync.advertise = false;
        safe.privacy.history_enabled = false;
        safe.privacy.keep_audio_recordings = false;
        safe.privacy.save_discarded_transcriptions = false;
        safe.hotkeys.hotkeys_enabled = false;
        safe
    }

    fn initialize_settings_on(conn: &Connection, fresh: bool) -> rusqlite::Result<()> {
        let settings = if fresh {
            AppSettings::default()
        } else {
            Self::safe_settings()
        };
        let mut value = Self::settings_value(&settings)?;
        value["settings_version"] = serde_json::Value::from(1);
        conn.execute(
            "INSERT OR IGNORE INTO kv(key,value) VALUES ('settings',?1)",
            [value.to_string()],
        )?;
        Ok(())
    }

    fn decode_settings_value(raw: &str) -> Result<(serde_json::Value, bool), serde_json::Error> {
        let mut value: serde_json::Value = serde_json::from_str(raw)?;
        let object = value.as_object_mut().ok_or_else(|| {
            <serde_json::Error as serde::de::Error>::custom("settings must be an object")
        })?;
        if object
            .get("settings_version")
            .is_some_and(|version| !matches!(version.as_u64(), Some(0 | 1)))
        {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "unsupported settings version",
            ));
        }
        let mut incomplete = false;
        for (section, field) in [
            ("sync", "enabled"),
            ("sync", "advertise"),
            ("privacy", "history_enabled"),
            ("privacy", "keep_audio_recordings"),
            ("privacy", "save_discarded_transcriptions"),
            ("hotkeys", "hotkeys_enabled"),
        ] {
            let section = object
                .entry(section)
                .or_insert_with(|| serde_json::json!({}));
            if let Some(section) = section.as_object_mut()
                && !section.contains_key(field)
            {
                section.insert(field.into(), serde_json::Value::Bool(false));
                incomplete = true;
            }
        }
        // Schema 0 entries omit thinking. Null in the current shape is an
        // explicit model-default choice, independent of the schema marker.
        if let Some(entries) = value
            .get_mut("language_models")
            .and_then(|models| models.get_mut("entries"))
            .and_then(serde_json::Value::as_array_mut)
        {
            for entry in entries {
                if entry["kind"] == "Ollama"
                    && let Some(entry) = entry.as_object_mut()
                {
                    entry
                        .entry("thinking")
                        .or_insert(serde_json::Value::Bool(false));
                }
            }
        }
        // Validate typed fields without discarding unrecognized settings.
        serde_json::from_value::<AppSettings>(value.clone())?;
        value["settings_version"] = serde_json::Value::from(1);
        Ok((value, incomplete))
    }

    pub fn settings_warning(&self) -> Option<&'static str> {
        self.settings_warning.get()
    }

    pub fn try_load_settings(&self) -> rusqlite::Result<AppSettings> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                row.get(0)
            })
            .optional()?;
        let raw = raw.ok_or_else(|| {
            rusqlite::Error::ToSqlConversionFailure(
                "stored settings are missing; permissions disabled until recovery".into(),
            )
        })?;
        let (value, incomplete) = Self::decode_settings_value(&raw).map_err(|error| {
            rusqlite::Error::ToSqlConversionFailure(crate::diagnostics::json(&error).into())
        })?;
        let mut settings: AppSettings = serde_json::from_value(value).map_err(|error| {
            rusqlite::Error::ToSqlConversionFailure(crate::diagnostics::json(&error).into())
        })?;
        settings.language_models.ensure_seed();
        settings.stt.ensure_seed();
        settings.dictionary.words = self.load_words()?;
        settings.dictionary.snippets = self.load_snippets()?;
        if matches!(
            settings.stt.local_provider,
            crate::settings::LocalSttProvider::Nvidia
        ) && settings.stt.parakeet_model == "cohere-transcribe-03-2026"
        {
            settings.stt.local_provider = crate::settings::LocalSttProvider::Cohere;
        }
        let reset_pending = self.reset_pending()?;
        if reset_pending {
            settings.hotkeys.hotkeys_enabled = false;
            settings.sync.enabled = false;
        }
        *self.settings_baseline.borrow_mut() = Some(Self::settings_value(&settings)?);
        self.settings_warning.set(if reset_pending {
            Some("settings.reset.incomplete")
        } else {
            incomplete.then_some("settings.recovery.incomplete")
        });
        Ok(settings)
    }

    pub fn load_settings(&self) -> AppSettings {
        match self.try_load_settings() {
            Ok(settings) => settings,
            Err(error) => {
                self.settings_warning
                    .set(Some("settings.recovery.unreadable"));
                *self.settings_baseline.borrow_mut() = None;
                eprintln!(
                    "[store] settings could not be read: {error}; original settings preserved, sync disabled, recording permissions disabled"
                );
                Self::safe_settings()
            }
        }
    }

    /// Persist the settings blob. Versioned tables (words/snippets)
    /// are NOT touched here: they are authoritative and already carry
    /// their own timestamps from CRUD/merge paths. Rewriting them on
    /// every settings save would mint fresh versions and could clobber
    /// a peer's concurrent edit on the next sync.
    pub fn save_settings(&self, settings: &AppSettings) -> rusqlite::Result<()> {
        // Acquire write ownership before reading the blob. A deferred read
        // snapshot cannot upgrade after another process commits, even with
        // busy_timeout; permissions and disjoint edits must save atomically.
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let old: Option<String> = tx
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |r| {
                r.get(0)
            })
            .optional()?;
        let raw = old.ok_or_else(|| {
            rusqlite::Error::ToSqlConversionFailure(
                "stored settings are missing; restore them before saving".into(),
            )
        })?;
        let (mut current, _) = Self::decode_settings_value(&raw).map_err(|error| {
            rusqlite::Error::ToSqlConversionFailure(
                format!(
                    "settings are invalid; repair the preserved original before saving: {}",
                    crate::diagnostics::json(&error)
                )
                .into(),
            )
        })?;
        let previous_privacy = current.get("privacy").cloned();
        let previous_input = current["hotkeys"]["hotkeys_enabled"].clone();
        let next = Self::settings_value(settings)?;
        if let Some(baseline) = self.settings_baseline.borrow().as_ref() {
            Self::patch_changed(baseline, &next, &mut current);
        } else {
            Self::patch_changed(
                &Self::settings_value(&serde_json::from_value(current.clone()).map_err(|e| {
                    rusqlite::Error::ToSqlConversionFailure(crate::diagnostics::json(&e).into())
                })?)?,
                &next,
                &mut current,
            );
        }
        for field in ["words", "snippets"] {
            current["dictionary"][field] = next["dictionary"][field].clone();
        }
        current["settings_version"] = serde_json::Value::from(1);
        let json = serde_json::to_string(&current)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        tx.execute("INSERT INTO kv(key,value) VALUES ('settings',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[json])?;
        if current.get("privacy") != previous_privacy.as_ref()
            || current["hotkeys"]["hotkeys_enabled"] != previous_input
        {
            Self::invalidate_capture(&tx)?;
        }
        Self::invalidate_sync(&tx)?;
        tx.commit()?;
        // Keep the caller's snapshot as the baseline; unobserved changes from
        // another frontend must not become apparent local edits on the next save.
        *self.settings_baseline.borrow_mut() = Some(next);
        Ok(())
    }

    fn settings_value(settings: &AppSettings) -> rusqlite::Result<serde_json::Value> {
        let mut clean = settings.clone();
        clean.dictionary = Default::default();
        serde_json::to_value(clean).map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))
    }

    fn patch_changed(
        base: &serde_json::Value,
        next: &serde_json::Value,
        current: &mut serde_json::Value,
    ) {
        if base == next {
            return;
        }
        if let (Some(base), Some(next), Some(current_array)) =
            (base.as_array(), next.as_array(), current.as_array())
        {
            // Empty IDs identify the single legacy model entry too. Ambiguous
            // legacy lists retain positional matching rather than conflating
            // multiple entries with the same ID.
            fn entry_id(value: &serde_json::Value) -> Option<&str> {
                value.get("id").and_then(serde_json::Value::as_str)
            }
            let mut patched = Vec::with_capacity(next.len());
            for (index, value) in next.iter().enumerate() {
                let id = entry_id(value).filter(|id| {
                    [base, next, current_array].into_iter().all(|items| {
                        items
                            .iter()
                            .filter(|item| entry_id(item) == Some(*id))
                            .count()
                            <= 1
                    })
                });
                let find = |items: &[serde_json::Value]| match id {
                    Some(id) => items.iter().position(|item| entry_id(item) == Some(id)),
                    None => (base.len() == next.len())
                        .then_some(index)
                        .filter(|index| *index < items.len()),
                };
                match (find(base), find(current_array)) {
                    (Some(old), Some(now)) => {
                        let mut item = current_array[now].clone();
                        Self::patch_changed(&base[old], value, &mut item);
                        patched.push(item);
                    }
                    // An unobserved deletion wins over editing a stale entry;
                    // restoring it requires an explicit addition with a new ID.
                    (Some(_), None) if id.is_some() => {}
                    _ => patched.push(value.clone()),
                }
            }
            // Entries added by another frontend are absent from this caller's
            // baseline. Preserve them after the caller's explicit ordering.
            for value in current_array {
                if let Some(id) = entry_id(value)
                    && !base.iter().any(|item| entry_id(item) == Some(id))
                    && !next.iter().any(|item| entry_id(item) == Some(id))
                {
                    patched.push(value.clone());
                }
            }
            *current = serde_json::Value::Array(patched);
            return;
        }
        if let (Some(base), Some(next), Some(current)) =
            (base.as_object(), next.as_object(), current.as_object_mut())
        {
            for (key, value) in next {
                if let Some(old) = base.get(key) {
                    Self::patch_changed(
                        old,
                        value,
                        current
                            .entry(key.clone())
                            .or_insert(serde_json::Value::Null),
                    );
                } else {
                    current.insert(key.clone(), value.clone());
                }
            }
        } else {
            *current = next.clone();
        }
    }

    // -- dictionary ----------------------------------------------------------

    pub fn load_words(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT word FROM words WHERE deleted = 0 ORDER BY word")?;
        stmt.query_map([], |row| row.get(0))?.collect()
    }

    /// Local write: add or resurrect a word.
    pub fn upsert_word(&self, word: &str, node_id: &str) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let version = Self::next_version(&tx)?;
        Self::apply_words_on(
            &tx,
            &[WordRecord {
                word: word.to_string(),
                updated_at: version,
                updated_by: node_id.to_string(),
                deleted: false,
            }],
        )?;
        bump_gen(&tx)?;
        tx.commit()?;
        bump_data_gen();
        Ok(())
    }

    /// Local write: tombstone a word (propagates as a delete).
    pub fn delete_word(&self, word: &str, node_id: &str) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let version = Self::next_version(&tx)?;
        Self::apply_words_on(
            &tx,
            &[WordRecord {
                word: word.to_string(),
                updated_at: version,
                updated_by: node_id.to_string(),
                deleted: true,
            }],
        )?;
        bump_gen(&tx)?;
        tx.commit()?;
        bump_data_gen();
        Ok(())
    }

    pub fn load_snippets(&self) -> rusqlite::Result<Vec<Snippet>> {
        let mut stmt = self.conn.prepare(
            "SELECT trigger, replacement FROM snippets WHERE deleted = 0 ORDER BY trigger",
        )?;
        stmt.query_map([], |row| {
            Ok(Snippet {
                trigger: row.get(0)?,
                replacement: row.get(1)?,
            })
        })?
        .collect()
    }

    /// Local write: add/update a snippet.
    pub fn upsert_snippet(
        &self,
        trigger: &str,
        replacement: &str,
        node_id: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let version = Self::next_version(&tx)?;
        Self::apply_snippets_on(
            &tx,
            &[SnippetRecord {
                trigger: trigger.to_string(),
                replacement: replacement.to_string(),
                updated_at: version,
                updated_by: node_id.to_string(),
                deleted: false,
            }],
        )?;
        bump_gen(&tx)?;
        tx.commit()?;
        bump_data_gen();
        Ok(())
    }

    /// Local write: tombstone a snippet.
    pub fn delete_snippet(&self, trigger: &str, node_id: &str) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let version = Self::next_version(&tx)?;
        Self::apply_snippets_on(
            &tx,
            &[SnippetRecord {
                trigger: trigger.to_string(),
                replacement: String::new(),
                updated_at: version,
                updated_by: node_id.to_string(),
                deleted: true,
            }],
        )?;
        bump_gen(&tx)?;
        tx.commit()?;
        bump_data_gen();
        Ok(())
    }

    // -- history (append-only, union by uuid) ---------------------------------

    pub fn load_history(
        &self,
        limit: usize,
        include_discarded: bool,
    ) -> rusqlite::Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, text, raw_text, datetime(recorded_at, 'unixepoch'), words,
                    duration_secs, recorded_at, discarded
             FROM history WHERE deleted = 0 AND (discarded = 0 OR ?1)
             ORDER BY recorded_at DESC LIMIT ?2",
        )?;
        stmt.query_map([include_discarded as i64, limit as i64], |row| {
            Ok(HistoryEntry {
                uuid: row.get(0)?,
                text: row.get(1)?,
                raw_text: row.get(2)?,
                date_label: row.get(3)?,
                word_count: row.get(4)?,
                duration_secs: row.get(5)?,
                recorded_at: row.get(6)?,
                discarded: row.get(7)?,
            })
        })?
        .collect()
    }

    /// Local delete: tombstone, so the row stays deleted across sync
    /// (union-by-uuid would otherwise resurrect it from a peer).
    pub fn delete_history(&self, uuid: &str) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let pending=tx.execute("INSERT OR IGNORE INTO history(uuid,text,raw_text,words,duration_secs,deleted,discarded,recorded_at) SELECT record_key,'','',0,0,1,0,?2 FROM sync_transfers WHERE direction=1 AND kind=2 AND record_key=?1",(uuid,now_ms()/1000))?;
        let changed=tx.execute("UPDATE history SET deleted=1,text='',raw_text='',words=0,duration_secs=0 WHERE uuid=?1 AND deleted=0",[uuid])? + pending;
        if changed > 0 {
            Self::invalidate_sync(&tx)?;
            bump_gen(&tx)?;
        }
        tx.commit()?;
        if changed > 0 {
            bump_data_gen();
        }
        self.prune_audio_vault_at(now_ms() / 1000).map(|_| ())
    }

    /// Legacy cleanup must retain tombstones so peers cannot resurrect rows.
    pub fn delete_zero_duration_history(&self) -> rusqlite::Result<usize> {
        self.redact_history_where("duration_secs<=0", [])
    }

    /// Clear transcript payloads while preserving replicated deletion identities.
    pub fn clear_history(&self) -> rusqlite::Result<usize> {
        self.redact_history_where("1", [])
    }

    fn redact_history_where(
        &self,
        predicate: &str,
        params: impl rusqlite::Params,
    ) -> rusqlite::Result<usize> {
        let now = now_ms() / 1000;
        let count = self.redact_history_where_at(predicate, params, now)?;
        self.prune_audio_vault_at(now)?;
        Ok(count)
    }

    fn redact_history_where_at(
        &self,
        predicate: &str,
        params: impl rusqlite::Params,
        now: i64,
    ) -> rusqlite::Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let pending = if predicate == "1" {
            tx.execute("INSERT OR IGNORE INTO history(uuid,text,raw_text,words,duration_secs,deleted,discarded,recorded_at) SELECT record_key,'','',0,0,1,0,?1 FROM sync_transfers WHERE direction=1 AND kind=2",[now])?
        } else {
            0
        };
        let count=tx.execute(&format!("UPDATE history SET deleted=1,text='',raw_text='',words=0,duration_secs=0 WHERE deleted=0 AND ({predicate})"),params)? + pending;
        if predicate == "1" {
            Self::invalidate_capture(&tx)?;
        }
        if count > 0 {
            Self::invalidate_sync(&tx)?;
            bump_gen(&tx)?;
        }
        tx.commit()?;
        if count > 0 {
            bump_data_gen();
        }
        Ok(count)
    }

    /// One-shot service commands (GUI -> service): "stop"/"start"
    /// STT servers. Consumed (deleted) on read; stale entries die at
    /// service boot without running.
    pub fn set_server_cmd(&self, cmd: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO kv (key, value) VALUES ('stt_server_cmd', ?1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [cmd],
        )?;
        bump_gen(&self.conn)?;
        Ok(())
    }

    pub fn take_server_cmd(&self) -> rusqlite::Result<Option<String>> {
        let tx = self.conn.unchecked_transaction()?;
        let cmd: Option<String> = tx
            .query_row(
                "SELECT value FROM kv WHERE key='stt_server_cmd'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if cmd.is_some() {
            tx.execute("DELETE FROM kv WHERE key='stt_server_cmd'", [])?;
            bump_gen(&tx)?;
        }
        tx.commit()?;
        Ok(cmd)
    }

    /// Distinct local calendar days with at least one entry,
    /// newest first. Feeds the streak calculation.
    pub fn active_days(&self) -> rusqlite::Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT CAST(strftime('%s', recorded_at, 'unixepoch', 'localtime') AS INTEGER) / 86400 FROM history WHERE deleted = 0 AND discarded = 0 ORDER BY 1 DESC")?;
        stmt.query_map([], |row| row.get(0))?.collect()
    }

    /// Per-day aggregates for the insights heatmap: `(epoch_day,
    /// total_words, dictation_count)`, ascending, trailing `days`.
    /// Single indexed range scan -- no per-day queries.
    pub fn daily_aggregates(&self, days: u32) -> rusqlite::Result<Vec<(i64, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT CAST(strftime('%s', recorded_at, 'unixepoch', 'localtime') AS INTEGER) / 86400 AS day,
                    COALESCE(SUM(words), 0),
                    COUNT(*)
             FROM history
             WHERE deleted = 0 AND discarded = 0 AND recorded_at >= strftime('%s', 'now', ?1)
             GROUP BY day
             ORDER BY day",
        )?;
        stmt.query_map([format!("-{days} days")], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect()
    }

    /// Activity totals use the whole saved history. Zero-duration legacy
    /// entries count toward words/dictations but cannot measure speaking speed.
    pub fn usage_stats(&self) -> rusqlite::Result<crate::settings::UsageStats> {
        self.conn.query_row(
            "SELECT COALESCE(SUM(words), 0), COUNT(*),
                    COALESCE(SUM(CASE WHEN duration_secs > 0 THEN words ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN duration_secs > 0 THEN duration_secs ELSE 0 END), 0)
             FROM history WHERE deleted = 0 AND discarded = 0",
            [],
            |row| {
                let words = row.get::<_, i64>(0)?.max(0) as u64;
                let count = row.get::<_, i64>(1)?.max(0) as u64;
                let measured_words = row.get::<_, i64>(2)?.max(0) as u64;
                let seconds = row.get::<_, i64>(3)?.max(0) as u64;
                Ok(crate::settings::UsageStats {
                    words_spoken: words,
                    dictations: count,
                    words_per_minute: if seconds > 0 {
                        measured_words as f64 * 60.0 / seconds as f64
                    } else {
                        0.0
                    },
                    streak_days: 0,
                })
            },
        )
    }

    /// Local write: append one entry. The uuid embeds the node id so
    /// concurrently recorded entries never collide.
    pub fn add_history(
        &self,
        raw_text: &str,
        text: &str,
        words: u32,
        duration_secs: i64,
        node_id: &str,
    ) -> rusqlite::Result<String> {
        self.add_history_full(raw_text, text, words, duration_secs, false, node_id)
    }

    /// Discarded (cancelled-midway) row: no text, no words, but a real
    /// duration. No audio is ever stored (RAM-only capture posture —
    /// stricter than the reference, which keeps the clip).
    pub fn add_discarded(&self, duration_secs: i64, node_id: &str) -> rusqlite::Result<String> {
        self.add_history_full("", "", 0, duration_secs, true, node_id)
    }

    fn add_history_full(
        &self,
        raw_text: &str,
        text: &str,
        words: u32,
        duration_secs: i64,
        discarded: bool,
        node_id: &str,
    ) -> rusqlite::Result<String> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let uuid = format!(
            "{}-{}",
            crate::device::short_id(node_id),
            hex_encode(&random)
        );
        self.apply_history(&[HistoryRecord {
            uuid: uuid.clone(),
            text: text.to_string(),
            raw_text: raw_text.to_string(),
            words,
            duration_secs,
            deleted: false,
            discarded,
            recorded_at: now_ms() / 1000,
        }])?;
        Ok(uuid)
    }

    pub(crate) fn persist_capture(
        &self,
        expected_epoch: i64,
        capture: CapturedUtterance<'_>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> rusqlite::Result<bool> {
        use std::sync::atomic::Ordering;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if cancel.load(Ordering::Acquire)
            || self.capture_epoch()? != expected_epoch
            || self.reset_pending()?
        {
            return Ok(false);
        }
        let privacy = self.load_settings().privacy;
        if !privacy.history_enabled || (capture.discarded && !privacy.save_discarded_transcriptions)
        {
            return Ok(true);
        }
        if privacy.transcript_auto_delete_days > 0
            && capture.recorded_at
                < (now_ms() / 1000)
                    .saturating_sub(i64::from(privacy.transcript_auto_delete_days) * 86400)
        {
            return Ok(false);
        }
        let mut random = [0; 16];
        getrandom::fill(&mut random)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let uuid = format!("local-{}", hex_encode(&random));
        let record = HistoryRecord {
            uuid: uuid.clone(),
            raw_text: if capture.discarded {
                String::new()
            } else {
                capture.raw.into()
            },
            text: if capture.discarded {
                String::new()
            } else {
                capture.text.into()
            },
            words: if capture.discarded {
                0
            } else {
                capture.text.split_whitespace().count() as u32
            },
            duration_secs: capture.duration_secs,
            recorded_at: capture.recorded_at,
            discarded: capture.discarded,
            deleted: false,
        };
        Self::apply_history_on(&tx, &[record])?;
        bump_gen(&tx)?;
        let audio = if !capture.discarded && privacy.keep_audio_recordings {
            self.path().and_then(Path::parent).zip(capture.wav)
        } else {
            None
        };
        let audio_path = if let Some((data, bytes)) = audio {
            let dir = crate::audio::audio_vault_dir(data);
            crate::private_fs::directory(&dir)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
            let path = dir.join(format!("{uuid}.wav"));
            if let Err(error) = crate::private_fs::replace(&path, bytes) {
                let _ = std::fs::remove_file(&path);
                return Err(rusqlite::Error::ToSqlConversionFailure(error.into()));
            }
            Some(path)
        } else {
            None
        };
        if cancel.load(Ordering::Acquire) {
            if let Some(path) = audio_path {
                let _ = std::fs::remove_file(path);
            }
            return Ok(false);
        }
        if let Err(error) = tx.commit() {
            if let Some(path) = audio_path {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
        bump_data_gen();
        Ok(true)
    }

    /// Redact history and tombstone dictionary records, revoke/remove peers,
    /// advance authorization epochs and restore defaults with sync disabled.
    /// Tombstones and revocations survive to prevent stale-peer resurrection.
    /// Callers separately coordinate capture/installers and filesystem cleanup.
    pub fn reset_all_data(&self) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        Self::reset_on(&tx)?;
        tx.commit()?;
        bump_data_gen();
        Ok(())
    }

    fn reset_on(tx: &Connection) -> rusqlite::Result<()> {
        let version = Self::next_version(tx)?;
        tx.execute("INSERT OR IGNORE INTO history(uuid,text,raw_text,words,duration_secs,deleted,discarded,recorded_at) SELECT record_key,'','',0,0,1,0,recorded_at FROM sync_transfers WHERE direction=1 AND kind=2",[])?;
        tx.execute(
            "UPDATE history SET deleted=1, text='', raw_text='', words=0, duration_secs=0",
            [],
        )?;
        tx.execute("UPDATE words SET deleted=1, updated_at=?1", [version])?;
        tx.execute(
            "UPDATE snippets SET deleted=1, replacement='', updated_at=?1",
            [version],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO revoked_devices SELECT node_id FROM devices",
            [],
        )?;
        tx.execute("DELETE FROM devices", [])?;
        Self::invalidate_sync(tx)?;
        Self::invalidate_capture(tx)?;
        tx.execute("DELETE FROM kv", [])?;
        let mut defaults = AppSettings::default();
        defaults.sync.enabled = false;
        tx.execute(
            "INSERT INTO kv(key,value) VALUES ('settings',?1)",
            [serde_json::to_string(&defaults)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?],
        )?;
        bump_gen(tx)?;
        Ok(())
    }

    pub(crate) fn reset_pending(&self) -> rusqlite::Result<bool> {
        Self::reset_pending_on(&self.conn)
    }

    fn reset_pending_on(conn: &Connection) -> rusqlite::Result<bool> {
        if let Some(data) = conn.path().and_then(|path| Path::new(path).parent())
            && crate::reset::requested(data)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?
        {
            return Ok(true);
        }
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM kv WHERE key='reset_pending')",
            [],
            |row| row.get(0),
        )
    }

    pub(crate) fn pending_reset_nonce(&self) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM kv WHERE key='reset_pending'",
                [],
                |row| row.get(0),
            )
            .optional()
    }

    pub(crate) fn completed_reset(&self, nonce: &str) -> rusqlite::Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sync_meta WHERE key='completed_reset' AND value=?1)",
            [nonce],
            |row| row.get(0),
        )
    }

    /// Persist one reset transaction. Retrying cleanup must not wipe edits
    /// written after this commit, including after an interrupted restart.
    pub(crate) fn begin_reset(&self, nonce: &str) -> rusqlite::Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let pending: Option<String> = tx
            .query_row(
                "SELECT value FROM kv WHERE key='reset_pending'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match pending.as_deref() {
            Some(current) if current == nonce => return Ok(()),
            Some(_) => {
                return Err(rusqlite::Error::ToSqlConversionFailure(
                    "another reset must complete before beginning a new one".into(),
                ));
            }
            None => {}
        }
        if self.completed_reset(nonce)? {
            return Ok(());
        }
        Self::reset_on(&tx)?;
        tx.execute(
            "INSERT INTO kv(key,value) VALUES('reset_pending',?1)",
            [nonce],
        )?;
        tx.commit()?;
        bump_data_gen();
        Ok(())
    }

    pub(crate) fn complete_reset(&self, nonce: &str) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "DELETE FROM kv WHERE key='reset_pending' AND value=?1",
            [nonce],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "reset completion does not own its pending transaction".into(),
            ));
        }
        tx.execute("INSERT INTO sync_meta(key,value) VALUES('completed_reset',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[nonce])?;
        bump_gen(&tx)?;
        tx.commit()?;
        bump_data_gen();
        Ok(())
    }

    /// Adopt a worker-loaded snapshot without doing database IO on GTK.
    pub(crate) fn adopt_settings_baseline(
        &self,
        settings: &AppSettings,
        warning: Option<&'static str>,
    ) -> rusqlite::Result<()> {
        *self.settings_baseline.borrow_mut() = Some(Self::settings_value(settings)?);
        self.settings_warning.set(warning);
        Ok(())
    }

    /// Delete rows older than `days` (auto-delete transcripts).
    /// Tombstones survive: dropping them would resurrect peers' rows.
    pub fn purge_old_history(&self, days: u32) -> rusqlite::Result<usize> {
        self.redact_history_where(
            "recorded_at < strftime('%s','now',?1)",
            [format!("-{days} days")],
        )
    }

    /// Periodic retention also handles idle applications and orphaned audio
    /// left by a crash between writing a clip and committing its history row.
    pub(crate) fn maintain_retention_at(&self, now: i64) -> rusqlite::Result<(usize, usize)> {
        let Some(privacy) = self.retention_policy()? else {
            self.expire_sync_transfers_at(now)?;
            return Ok((0, 0));
        };
        let pending = self.expire_pending_history_at(now, privacy.transcript_auto_delete_days)?;
        self.expire_sync_transfers_at(now)?;
        let count = if privacy.transcript_auto_delete_days > 0 {
            self.redact_history_where_at(
                "recorded_at < ?1",
                [now.saturating_sub(i64::from(privacy.transcript_auto_delete_days) * 86400)],
                now,
            )?
        } else {
            0
        };
        Ok((count + pending, self.prune_audio_vault_at(now)?))
    }

    /// An incomplete or invalid configuration cannot authorize automatic
    /// deletion. Keep valid explicit retention policies working even when
    /// an unrelated permission was conservatively disabled during decoding.
    fn retention_policy(&self) -> rusqlite::Result<Option<crate::settings::PrivacySettings>> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(raw) = raw else {
            self.settings_warning
                .set(Some("settings.recovery.unreadable"));
            return Ok(None);
        };
        let value = match Self::decode_settings_value(&raw) {
            Ok((value, _)) => value,
            Err(error) => {
                self.settings_warning
                    .set(Some("settings.recovery.unreadable"));
                eprintln!(
                    "[store] retention awaits settings recovery: {}",
                    crate::diagnostics::json(&error)
                );
                return Ok(None);
            }
        };
        if ["audio_retention_days", "transcript_auto_delete_days"]
            .iter()
            .any(|field| value["privacy"].get(field).is_none())
        {
            self.settings_warning
                .set(Some("settings.recovery.incomplete"));
            return Ok(None);
        }
        let settings: AppSettings = serde_json::from_value(value).map_err(|error| {
            rusqlite::Error::ToSqlConversionFailure(crate::diagnostics::json(&error).into())
        })?;
        Ok(Some(settings.privacy))
    }

    pub fn clear_audio(&self) -> rusqlite::Result<usize> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        Self::invalidate_capture(&tx)?;
        tx.commit()?;
        let Some(data) = self.path().and_then(Path::parent) else {
            return Ok(0);
        };
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let count = crate::audio::clear_audio(data)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        tx.commit()?;
        Ok(count)
    }

    fn prune_audio_vault_at(&self, now: i64) -> rusqlite::Result<usize> {
        let Some(data) = self.path().and_then(Path::parent) else {
            return Ok(0);
        };
        let dir = crate::audio::audio_vault_dir(data);
        if !dir.exists() {
            return Ok(0);
        }
        let Some(privacy) = self.retention_policy()? else {
            return Ok(0);
        };
        // Delivery and pruning share the database writer lock: no clip can
        // appear between this snapshot and cleanup without its committed row.
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let mut history = tx.prepare(
            "SELECT recorded_at FROM history WHERE uuid=?1 AND deleted=0 AND discarded=0",
        )?;
        crate::private_fs::directory(&dir)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let mut removed = 0;
        for entry in entries {
            let entry = entry.map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
            let name = entry.file_name();
            let Some(uuid) = name.to_str().and_then(|name| name.strip_suffix(".wav")) else {
                continue;
            };
            let recorded: Option<i64> = history.query_row([uuid], |row| row.get(0)).optional()?;
            let expires = recorded.is_some_and(|at| {
                at < now.saturating_sub(i64::from(privacy.audio_retention_days.max(1)) * 86400)
            });
            if recorded.is_none() || expires {
                std::fs::remove_file(entry.path())
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
                removed += 1;
            }
        }
        drop(history);
        tx.commit()?;
        Ok(removed)
    }

    // -- sync: export + LWW apply --------------------------------------------

    pub fn export_words(&self) -> rusqlite::Result<Vec<WordRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT word, updated_at, updated_by, deleted FROM words")?;
        stmt.query_map([], |row| {
            Ok(WordRecord {
                word: row.get(0)?,
                updated_at: row.get(1)?,
                updated_by: row.get(2)?,
                deleted: row.get::<_, i64>(3)? != 0,
            })
        })?
        .collect()
    }

    pub fn export_snippets(&self) -> rusqlite::Result<Vec<SnippetRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT trigger, replacement, updated_at, updated_by, deleted FROM snippets",
        )?;
        stmt.query_map([], |row| {
            Ok(SnippetRecord {
                trigger: row.get(0)?,
                replacement: row.get(1)?,
                updated_at: row.get(2)?,
                updated_by: row.get(3)?,
                deleted: row.get::<_, i64>(4)? != 0,
            })
        })?
        .collect()
    }

    pub fn export_history(&self) -> rusqlite::Result<Vec<HistoryRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, text, raw_text, words, duration_secs, deleted, discarded, recorded_at
             FROM history",
        )?;
        stmt.query_map([], |row| {
            Ok(HistoryRecord {
                uuid: row.get(0)?,
                text: row.get(1)?,
                raw_text: row.get(2)?,
                words: row.get(3)?,
                duration_secs: row.get(4)?,
                deleted: row.get(5)?,
                discarded: row.get(6)?,
                recorded_at: row.get(7)?,
            })
        })?
        .collect()
    }

    fn next_version(conn: &Connection) -> rusqlite::Result<i64> {
        conn.query_row(
            "INSERT INTO sync_meta(key,value) VALUES ('clock',
                MAX(?1, COALESCE((SELECT MAX(updated_at) FROM words),0),
                     COALESCE((SELECT MAX(updated_at) FROM snippets),0)) + 1)
             ON CONFLICT(key) DO UPDATE SET value = MAX(CAST(value AS INTEGER),
                 CAST(excluded.value AS INTEGER) - 1) + 1 RETURNING CAST(value AS INTEGER)",
            [now_ms()],
            |r| r.get(0),
        )
    }

    fn validate_version(version: i64) -> rusqlite::Result<()> {
        if version < 0 || version > now_ms().saturating_add(7 * 86_400_000) {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "remote version outside accepted clock horizon".into(),
            ));
        }
        Ok(())
    }

    fn apply_words_on(conn: &Connection, records: &[WordRecord]) -> rusqlite::Result<usize> {
        let mut changed = 0;
        for r in records {
            Self::validate_version(r.updated_at)?;
            changed += conn.execute(
                "INSERT INTO words(word,updated_at,updated_by,deleted) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(word) DO UPDATE SET updated_at=excluded.updated_at,
                 updated_by=excluded.updated_by, deleted=excluded.deleted
                 WHERE (excluded.updated_at,excluded.updated_by) > (words.updated_at,words.updated_by)",
                (&r.word,r.updated_at,&r.updated_by,r.deleted))?;
        }
        Ok(changed)
    }
    pub fn apply_words(&self, records: &[WordRecord]) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let changed = Self::apply_words_on(&tx, records)?;
        if changed > 0 {
            bump_gen(&tx)?;
        }
        tx.commit()?;
        if changed > 0 {
            bump_data_gen();
        }
        Ok(())
    }

    fn apply_snippets_on(conn: &Connection, records: &[SnippetRecord]) -> rusqlite::Result<usize> {
        let mut changed = 0;
        for r in records {
            Self::validate_version(r.updated_at)?;
            changed += conn.execute(
                "INSERT INTO snippets(trigger,replacement,updated_at,updated_by,deleted) VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(trigger) DO UPDATE SET replacement=excluded.replacement,
                 updated_at=excluded.updated_at, updated_by=excluded.updated_by, deleted=excluded.deleted
                 WHERE (excluded.updated_at,excluded.updated_by) > (snippets.updated_at,snippets.updated_by)",
                (&r.trigger, if r.deleted { "" } else { &r.replacement },r.updated_at,&r.updated_by,r.deleted))?;
        }
        Ok(changed)
    }
    pub fn apply_snippets(&self, records: &[SnippetRecord]) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let changed = Self::apply_snippets_on(&tx, records)?;
        if changed > 0 {
            bump_gen(&tx)?;
        }
        tx.commit()?;
        if changed > 0 {
            bump_data_gen();
        }
        Ok(())
    }

    fn apply_history_on(conn: &Connection, records: &[HistoryRecord]) -> rusqlite::Result<usize> {
        let mut changed = 0;
        for r in records {
            changed += conn.execute(
                "INSERT INTO history(uuid,text,raw_text,words,duration_secs,deleted,discarded,recorded_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(uuid) DO UPDATE SET
                   deleted=history.deleted OR excluded.deleted,
                   discarded=history.discarded OR excluded.discarded,
                   text=CASE WHEN history.deleted OR excluded.deleted OR history.discarded OR excluded.discarded THEN '' ELSE history.text END,
                   raw_text=CASE WHEN history.deleted OR excluded.deleted OR history.discarded OR excluded.discarded THEN '' ELSE history.raw_text END,
                   words=CASE WHEN history.deleted OR excluded.deleted OR history.discarded OR excluded.discarded THEN 0 ELSE history.words END,
                   duration_secs=CASE WHEN history.deleted OR excluded.deleted THEN 0
                     WHEN history.discarded OR excluded.discarded THEN MAX(history.duration_secs,excluded.duration_secs) ELSE history.duration_secs END
                 WHERE (excluded.deleted AND NOT history.deleted) OR (excluded.discarded AND NOT history.discarded)
                   OR (history.discarded AND NOT history.deleted AND (history.text<>'' OR history.raw_text<>'' OR history.words<>0 OR history.duration_secs<excluded.duration_secs))",
                (&r.uuid, if r.deleted || r.discarded { "" } else { &r.text },if r.deleted || r.discarded { "" } else { &r.raw_text },
                 if r.deleted || r.discarded { 0 } else { r.words },if r.deleted { 0 } else { r.duration_secs },r.deleted,r.discarded,r.recorded_at))?;
        }
        Ok(changed)
    }
    pub fn apply_history(&self, records: &[HistoryRecord]) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let changed = Self::apply_history_on(&tx, records)?;
        if changed > 0 {
            bump_gen(&tx)?;
        }
        tx.commit()?;
        if changed > 0 {
            bump_data_gen();
        }
        self.prune_audio_vault_at(now_ms() / 1000).map(|_| ())
    }

    /// One content round either commits all tables or leaves all unchanged.
    pub fn apply_content(
        &self,
        words: &[WordRecord],
        snippets: &[SnippetRecord],
        history: &[HistoryRecord],
    ) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let changed = Self::apply_words_on(&tx, words)?
            + Self::apply_snippets_on(&tx, snippets)?
            + Self::apply_history_on(&tx, history)?;
        if changed > 0 {
            bump_gen(&tx)?;
        }
        tx.commit()?;
        if changed > 0 {
            bump_data_gen();
        }
        self.prune_audio_vault_at(now_ms() / 1000).map(|_| ())
    }

    /// All exported tables share one authorized SQLite snapshot. No mixed
    /// settings/trust/content reads and no defaulting away an export failure.
    pub(crate) fn export_authorized(
        &self,
        peer_id: &str,
        epoch: i64,
    ) -> rusqlite::Result<ContentSnapshot> {
        let tx = self.conn.unchecked_transaction()?;
        self.check_sync_identity()?;
        Self::require_sync_authorized(&tx, peer_id, epoch)?;
        // Legacy peers still require one message. Refuse oversized snapshots
        // before building vectors; capable peers use the paged path instead.
        let estimate: i64 = tx.query_row(
            "SELECT COALESCE((SELECT SUM(length(CAST(word AS BLOB))+length(CAST(updated_by AS BLOB))+256) FROM words),0)
               +COALESCE((SELECT SUM(length(CAST(trigger AS BLOB))+length(CAST(replacement AS BLOB))+length(CAST(updated_by AS BLOB))+256) FROM snippets),0)
               +COALESCE((SELECT SUM(length(CAST(uuid AS BLOB))+length(CAST(text AS BLOB))+length(CAST(raw_text AS BLOB))+512) FROM history),0)
               +COALESCE((SELECT SUM(length(CAST(node_id AS BLOB))+length(CAST(name AS BLOB))+length(CAST(address AS BLOB))+length(CAST(addresses AS BLOB))+256) FROM devices),0)",
            [], |row| row.get(0),
        )?;
        if estimate > 16 * 1024 * 1024 {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "peer needs paged-sync support for this database; upgrade the peer".into(),
            ));
        }
        let snapshot = ContentSnapshot {
            words: self.export_words()?,
            snippets: self.export_snippets()?,
            history: self.export_history()?,
            peers: self.export_gossip()?,
        };
        tx.commit()?;
        Ok(snapshot)
    }

    fn require_sync_authorized(
        conn: &Connection,
        peer_id: &str,
        epoch: i64,
    ) -> rusqlite::Result<()> {
        let current_epoch: Option<i64> = conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM sync_meta WHERE key='policy_epoch'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let settings: Option<String> = conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                row.get(0)
            })
            .optional()?;
        let enabled = settings
            .and_then(|json| Self::decode_settings_value(&json).ok())
            .is_some_and(|(value, _)| value["sync"]["enabled"].as_bool() == Some(true));
        let peer = Self::peer_on(conn, peer_id)?;
        let revoked: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM revoked_devices WHERE node_id=?1)",
            [peer_id],
            |row| row.get(0),
        )?;
        if !enabled
            || Self::reset_pending_on(conn)?
            || revoked
            || current_epoch.unwrap_or(0) != epoch
            || !peer.is_some_and(|peer| peer.trusted)
        {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "sync authorization changed".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn apply_sync_round(
        &self,
        peer_id: &str,
        epoch: i64,
        self_id: &str,
        content: (&[WordRecord], &[SnippetRecord], &[HistoryRecord]),
        gossip: &[PeerGossip],
    ) -> rusqlite::Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        self.check_sync_identity()?;
        Self::require_sync_authorized(&tx, peer_id, epoch)?;
        let peer = Self::peer_on(&tx, peer_id)?;
        let changed = Self::apply_words_on(&tx, content.0)?
            + Self::apply_snippets_on(&tx, content.1)?
            + Self::apply_history_on(&tx, content.2)?;
        let added = Self::apply_gossip_on(
            &tx,
            gossip,
            peer.is_some_and(|peer| peer.introducer),
            self_id,
        )?;
        tx.execute(
            "UPDATE devices SET last_seen=?2,last_sync=?2 WHERE node_id=?1",
            (peer_id, now_ms() / 1000),
        )?;
        if changed > 0 || added > 0 {
            bump_gen(&tx)?;
        }
        tx.commit()?;
        if changed > 0 || added > 0 {
            bump_data_gen();
        }
        self.prune_audio_vault_at(now_ms() / 1000)?;
        Ok(added)
    }

    pub fn open_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON;")?;
        conn.execute_batch(SCHEMA)?;
        Self::initialize_settings_on(&conn, true)?;
        pages::initialize(&conn)?;
        segments::initialize(&conn)?;
        conn.execute_batch("PRAGMA user_version=8")?;
        Ok(Self {
            conn,
            file_identity: None,
            settings_baseline: std::cell::RefCell::new(None),
            settings_warning: std::cell::Cell::new(None),
        })
    }
}

/// Versioned word row exchanged between nodes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WordRecord {
    pub word: String,
    pub updated_at: i64,
    pub updated_by: String,
    pub deleted: bool,
}

/// Versioned snippet row exchanged between nodes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SnippetRecord {
    pub trigger: String,
    pub replacement: String,
    pub updated_at: i64,
    pub updated_by: String,
    pub deleted: bool,
}

/// Append-only history row exchanged between nodes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HistoryRecord {
    pub uuid: String,
    pub text: String,
    pub raw_text: String,
    pub words: u32,
    pub duration_secs: i64,
    pub deleted: bool,
    pub discarded: bool,
    pub recorded_at: i64,
}

/// Trusted peer row.
///
/// `address` is the legacy single last-known address; `addresses` is
/// the JSON list fed by mDNS, gossip and direct contact. Dial tries
/// all of them. `pending` marks an incoming request awaiting a
/// decision; `introducer` peers auto-add the devices they vouch for.
#[derive(Debug, Clone)]
pub struct Peer {
    pub node_id: String,
    pub name: String,
    pub address: String,
    pub addresses: Vec<String>,
    pub trusted: bool,
    pub pending: bool,
    pub introducer: bool,
    pub last_seen: i64,
    pub last_sync: i64,
}

impl Peer {
    /// All known dial targets, most-recent-first guess: explicit list
    /// first, then the legacy single address.
    pub fn dial_targets(&self) -> Vec<String> {
        let mut out = self.addresses.clone();
        if !self.address.is_empty() && !out.contains(&self.address) {
            out.push(self.address.clone());
        }
        out
    }
}

fn parse_addresses(raw: &str, legacy: &str) -> Vec<String> {
    let mut out: Vec<String> = serde_json::from_str(raw).unwrap_or_default();
    if !legacy.is_empty() && !out.contains(&legacy.to_string()) {
        out.push(legacy.to_string());
    }
    out
}

fn dump_addresses(addresses: &[String]) -> String {
    serde_json::to_string(addresses).unwrap_or_else(|_| "[]".to_string())
}

/// Merge new observations into a stored address list (dedup, cap 8).
fn merge_addresses(current: &[String], seen: &[String]) -> Vec<String> {
    let mut out: Vec<String> = seen
        .iter()
        .filter(|address| !address.is_empty() && address.len() <= 256)
        .take(8)
        .cloned()
        .collect();
    for a in current {
        if !out.contains(a) {
            out.push(a.clone());
        }
    }
    out.truncate(8);
    out
}

impl Store {
    fn peer_from_row(row: &rusqlite::Row) -> rusqlite::Result<Peer> {
        let address: String = row.get(2)?;
        let addresses_raw: String = row.get(3)?;
        Ok(Peer {
            node_id: row.get(0)?,
            name: row.get(1)?,
            address: address.clone(),
            addresses: parse_addresses(&addresses_raw, &address),
            trusted: row.get::<_, i64>(4)? != 0,
            pending: row.get::<_, i64>(5)? != 0,
            introducer: row.get::<_, i64>(6)? != 0,
            last_seen: row.get(7)?,
            last_sync: row.get(8)?,
        })
    }

    pub fn list_peers(&self) -> rusqlite::Result<Vec<Peer>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, name, address, addresses, trusted, pending,
                    introducer, last_seen, last_sync
             FROM devices ORDER BY name",
        )?;
        stmt.query_map([], Self::peer_from_row)?.collect()
    }

    pub fn get_peer(&self, node_id: &str) -> rusqlite::Result<Option<Peer>> {
        Self::peer_on(&self.conn, node_id)
    }
    fn peer_on(conn: &Connection, node_id: &str) -> rusqlite::Result<Option<Peer>> {
        conn.query_row("SELECT node_id,name,address,addresses,trusted,pending,introducer,last_seen,last_sync FROM devices WHERE node_id=?1",[node_id],Self::peer_from_row).optional()
    }

    /// Insert or update a peer row (manual add / edits).
    pub fn upsert_peer(
        &self,
        node_id: &str,
        name: &str,
        address: &str,
        trusted: bool,
    ) -> rusqlite::Result<()> {
        if !crate::device::valid_node_id(node_id) {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "invalid peer identity".into(),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        Self::invalidate_sync(&tx)?;
        if trusted {
            tx.execute("DELETE FROM revoked_devices WHERE node_id=?1", [node_id])?;
        }
        let addresses = if address.is_empty() {
            "[]".to_string()
        } else {
            dump_addresses(&[address.to_string()])
        };
        tx.execute(
            "INSERT INTO devices
               (node_id, name, address, addresses, trusted, pending, introducer,
                last_seen, last_sync)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, 0, 0)
             ON CONFLICT (node_id) DO UPDATE SET
               name = excluded.name,
               address = excluded.address,
               addresses = excluded.addresses,
               trusted = excluded.trusted,
               introducer = CASE WHEN excluded.trusted THEN devices.introducer ELSE 0 END,
               pending = 0",
            (node_id, name, address, addresses, trusted as i64),
        )?;
        tx.commit()
    }

    pub fn remove_peer(&self, node_id: &str) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        Self::invalidate_sync(&tx)?;
        tx.execute(
            "INSERT OR IGNORE INTO revoked_devices(node_id) VALUES (?1)",
            [node_id],
        )?;
        tx.execute("DELETE FROM devices WHERE node_id=?1", [node_id])?;
        tx.commit()
    }

    pub fn is_revoked(&self, node_id: &str) -> rusqlite::Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM revoked_devices WHERE node_id=?1)",
            [node_id],
            |r| r.get(0),
        )
    }

    /// Introduction authority is an explicit local choice, independent of trust.
    pub fn set_peer_introducer(&self, node_id: &str, enabled: bool) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let peer = Self::peer_on(&tx, node_id)?
            .ok_or_else(|| rusqlite::Error::ToSqlConversionFailure("peer does not exist".into()))?;
        if enabled && !peer.trusted {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "trust the peer before granting introduction authority".into(),
            ));
        }
        tx.execute(
            "UPDATE devices SET introducer=?2 WHERE node_id=?1",
            (node_id, enabled),
        )?;
        Self::invalidate_sync(&tx)?;
        bump_gen(&tx)?;
        tx.commit()
    }

    /// Rename without touching trust, pending or introducer flags.
    pub fn rename_peer(&self, node_id: &str, name: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE devices SET name = ?2 WHERE node_id = ?1",
            (node_id, name),
        )?;
        Ok(())
    }

    /// Replace the discovered address list for a peer.
    pub fn set_peer_addresses(&self, node_id: &str, addresses: &[String]) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE devices SET addresses = ?2 WHERE node_id = ?1",
            (node_id, dump_addresses(&merge_addresses(&[], addresses))),
        )?;
        Ok(())
    }

    /// Record a successful exchange with a peer.
    pub fn touch_peer_synced(&self, node_id: &str, name: &str) -> rusqlite::Result<()> {
        let now = now_ms() / 1000;
        self.conn.execute(
            "INSERT INTO devices (node_id, name, last_seen, last_sync)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT (node_id) DO UPDATE SET
               name = excluded.name,
               last_seen = excluded.last_seen,
               last_sync = excluded.last_sync",
            (node_id, name, now),
        )?;
        Ok(())
    }

    /// Record inbound contact. Unknown nodes become pending requests
    /// (never auto-trusted); known nodes refresh addresses/last_seen.
    pub fn record_seen(
        &self,
        node_id: &str,
        name: &str,
        addresses: &[String],
    ) -> rusqlite::Result<bool> {
        if !crate::device::valid_node_id(node_id) || self.is_revoked(node_id)? {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "invalid or revoked peer".into(),
            ));
        }
        let now = now_ms() / 1000;
        let existing = self.get_peer(node_id)?;
        match existing {
            None => {
                self.conn.execute(
                    "INSERT INTO devices
                       (node_id, name, addresses, trusted, pending, last_seen)
                     VALUES (?1, ?2, ?3, 0, 1, ?4)",
                    (
                        node_id,
                        name,
                        dump_addresses(&merge_addresses(&[], addresses)),
                        now,
                    ),
                )?;
                Ok(true)
            }
            Some(peer) => {
                let merged = merge_addresses(&peer.addresses, addresses);
                let peer_name = if name.is_empty() {
                    peer.name
                } else {
                    name.to_string()
                };
                self.conn.execute(
                    "UPDATE devices SET name = ?2, addresses = ?3, last_seen = ?4
                     WHERE node_id = ?1",
                    (node_id, peer_name, dump_addresses(&merged), now),
                )?;
                Ok(false)
            }
        }
    }

    /// Decide a pending request. Accepting trusts the peer, marks it
    /// an introducer (its vouched devices join automatically) and
    /// clears the pending flag.
    pub fn decide_request(&self, node_id: &str, accept: bool) -> rusqlite::Result<()> {
        if accept {
            self.conn.execute(
                "UPDATE devices SET trusted = 1, pending = 0, introducer = 0
                 WHERE node_id = ?1",
                [node_id],
            )?;
        } else {
            self.remove_peer(node_id)?;
        }
        Ok(())
    }

    pub fn list_pending(&self) -> rusqlite::Result<Vec<Peer>> {
        Ok(self
            .list_peers()?
            .into_iter()
            .filter(|p| p.pending)
            .collect())
    }

    /// Gossip payload: nodes we vouch for (trusted peers + self is
    /// added by the sender, not stored here).
    pub fn export_gossip(&self) -> rusqlite::Result<Vec<PeerGossip>> {
        Ok(self
            .list_peers()?
            .into_iter()
            .filter(|p| p.trusted)
            .map(|p| PeerGossip {
                node_id: p.node_id,
                name: p.name,
                addresses: p.addresses,
                last_seen: p.last_seen,
            })
            .collect())
    }

    /// Merge gossiped peers: refresh addresses of known nodes;
    /// auto-add strangers vouched by an introducer as trusted.
    pub fn apply_gossip(
        &self,
        gossip: &[PeerGossip],
        introducer: bool,
        self_id: &str,
    ) -> rusqlite::Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let added = Self::apply_gossip_on(&tx, gossip, introducer, self_id)?;
        tx.commit()?;
        Ok(added)
    }
    fn apply_gossip_on(
        conn: &Connection,
        gossip: &[PeerGossip],
        introducer: bool,
        self_id: &str,
    ) -> rusqlite::Result<usize> {
        let mut added = 0;
        for g in gossip {
            if g.node_id == self_id
                || !crate::device::valid_node_id(&g.node_id)
                || conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM revoked_devices WHERE node_id=?1)",
                    [&g.node_id],
                    |r| r.get::<_, bool>(0),
                )?
            {
                continue;
            }
            match Self::peer_on(conn, &g.node_id)? {
                Some(peer) => {
                    let merged = merge_addresses(&peer.addresses, &g.addresses);
                    conn.execute(
                        "UPDATE devices SET addresses = ?2, last_seen = MAX(last_seen, ?3)
                         WHERE node_id = ?1",
                        (g.node_id.clone(), dump_addresses(&merged), g.last_seen),
                    )?;
                }
                None if introducer => {
                    conn.execute(
                        "INSERT INTO devices
                           (node_id, name, addresses, trusted, pending, last_seen)
                         VALUES (?1, ?2, ?3, 1, 0, ?4)",
                        (
                            g.node_id.clone(),
                            g.name.clone(),
                            dump_addresses(&merge_addresses(&[], &g.addresses)),
                            g.last_seen,
                        ),
                    )?;
                    added += 1;
                }
                None => {}
            }
        }
        Ok(added)
    }
}

/// Gossiped peer descriptor exchanged inside sync payloads.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PeerGossip {
    pub node_id: String,
    pub name: String,
    #[serde(default)]
    pub addresses: Vec<String>,
    pub last_seen: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_key_provision_candidates_recover_existing_data_without_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let key = [45; 32];
        {
            let store = Store::open_with_key(&path, &key).unwrap();
            store
                .upsert_word("synthetic retained word", "test")
                .unwrap();
            store.checkpoint().unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        for candidates in [[key, [46; 32]], [[46; 32], key]] {
            assert_eq!(select_database_key(&path, true, &candidates).unwrap(), key);
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(
                select_database_key(&path, false, &candidates).is_err(),
                "ambiguous new/plaintext data must not choose an arbitrary key"
            );
        }
        assert!(select_database_key(&path, true, &[[46; 32], [47; 32]]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            Store::open_with_key(&path, &key)
                .unwrap()
                .load_words()
                .unwrap(),
            ["synthetic retained word"]
        );
    }

    #[test]
    fn activity_counts_all_history_and_only_measured_speed() {
        let store = Store::open_memory().unwrap();
        for _ in 0..205 {
            store.add_history("raw", "text", 10, 5, "node").unwrap();
        }
        store
            .add_history("legacy", "legacy", 100, 0, "node")
            .unwrap();
        store.add_discarded(30, "node").unwrap();
        let deleted = store
            .add_history("deleted", "deleted", 100, 2, "node")
            .unwrap();
        store.delete_history(&deleted).unwrap();
        let stats = store.usage_stats().unwrap();
        assert_eq!(stats.dictations, 206);
        assert_eq!(stats.words_spoken, 2150);
        assert_eq!(stats.words_per_minute, 120.0);
    }

    #[test]
    fn bulk_deletion_survives_sync_with_an_older_peer() {
        let a = Store::open_memory().unwrap();
        let b = Store::open_memory().unwrap();
        a.add_history("raw", "text", 10, 5, "node").unwrap();
        b.apply_history(&a.export_history().unwrap()).unwrap();
        assert_eq!(a.clear_history().unwrap(), 1);
        a.apply_history(&b.export_history().unwrap()).unwrap();
        b.apply_history(&a.export_history().unwrap()).unwrap();
        assert!(a.load_history(10, true).unwrap().is_empty());
        assert!(b.load_history(10, true).unwrap().is_empty());
        assert_eq!(a.usage_stats().unwrap().dictations, 0);
    }

    #[test]
    fn retained_history_deletion_survives_sync() {
        let a = Store::open_memory().unwrap();
        let b = Store::open_memory().unwrap();
        a.add_history("raw", "text", 10, 5, "node").unwrap();
        a.conn
            .execute(
                "UPDATE history SET recorded_at = strftime('%s','now','-60 days')",
                [],
            )
            .unwrap();
        b.apply_history(&a.export_history().unwrap()).unwrap();
        assert_eq!(a.purge_old_history(30).unwrap(), 1);
        a.apply_history(&b.export_history().unwrap()).unwrap();
        b.apply_history(&a.export_history().unwrap()).unwrap();
        assert!(a.load_history(10, true).unwrap().is_empty());
        assert!(b.load_history(10, true).unwrap().is_empty());
    }

    #[test]
    fn server_cmd_is_one_shot() {
        let store = Store::open_memory().unwrap();
        assert_eq!(store.take_server_cmd().unwrap(), None);
        store.set_server_cmd("stop").unwrap();
        assert_eq!(store.take_server_cmd().unwrap().as_deref(), Some("stop"));
        // Consumed: second read finds nothing.
        assert_eq!(store.take_server_cmd().unwrap(), None);
    }

    #[test]
    fn settings_engine_switch_roundtrip() {
        use crate::settings::LocalSttProvider;
        let store = Store::open_memory().unwrap();
        let mut s = store.load_settings();
        // Scoped mutation, then a separate persist read (never nested
        // borrows). Seeded chain survives the round trip.
        {
            s.stt.local_provider = LocalSttProvider::Nvidia;
        }
        store.save_settings(&s).unwrap();
        let reloaded = store.load_settings();
        assert_eq!(reloaded.stt.local_provider, LocalSttProvider::Nvidia);
        assert!(!reloaded.stt.entries.is_empty());
        assert_eq!(reloaded.stt.chain().len(), 1);
    }

    #[test]
    fn discarded_rows_filter_and_sync() {
        let store = Store::open_memory().unwrap();
        let uuid = store.add_discarded(3, "node-x").unwrap();
        // Hidden by default, visible with the toggle, flagged.
        assert_eq!(store.load_history(10, false).unwrap().len(), 0);
        let shown = store.load_history(10, true).unwrap();
        assert_eq!(shown.len(), 1);
        assert!(shown[0].discarded);
        assert_eq!(shown[0].duration_secs, 3);
        assert_eq!(shown[0].word_count, 0);
        // Stats queries never see discards.
        assert!(store.active_days().unwrap().is_empty());
        assert!(store.daily_aggregates(30).unwrap().is_empty());
        // Tombstoning a discard keeps it hidden everywhere.
        store.delete_history(&uuid).unwrap();
        assert_eq!(store.load_history(10, true).unwrap().len(), 0);
    }

    #[test]
    fn auto_delete_keeps_tombstones() {
        let store = Store::open_memory().unwrap();
        let uuid = store.add_history("r", "t", 2, 5, "node-x").unwrap();
        store.delete_history(&uuid).unwrap();
        // Ancient live rows go; tombstones stay (delete-sync safety).
        let n = store.purge_old_history(0).unwrap();
        assert_eq!(n, 0, "tombstone must survive auto-delete");
        store.add_history("r2", "t2", 2, 5, "node-x").unwrap();
        // Backdate the live row 60 days.
        store
            .conn
            .execute(
                "UPDATE history SET recorded_at = strftime('%s','now','-60 days') WHERE deleted = 0",
                [],
            )
            .unwrap();
        let n = store.purge_old_history(30).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn shared_gen_moves_on_write() {
        let store = Store::open_memory().unwrap();
        let v0 = read_gen(&store);
        store.upsert_word("hello", "node-x").unwrap();
        let v1 = read_gen(&store);
        assert!(v1 != v0, "shared gen must move on write ({v0} -> {v1})");
    }

    /// Two connections, same file (two processes in miniature): a
    /// write through one is visible to the other on the next read.
    /// This is the exact property UI refresh polls rely on.
    #[test]
    fn shared_gen_visible_across_connections() {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mywhisper-gen-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        // NOTE: Store::open needs the keyring; the test env has
        // a session keyring, same as production on this machine.
        // If the keyring is unavailable here, skip rather than fail:
        // crypto setup is covered by the app path, not this property.
        let Ok(a) = Store::open_with_key(&dir, &[42u8; 32]) else {
            return;
        };
        let v0 = read_gen(&a);
        {
            let b = Store::open_with_key(&dir, &[42u8; 32]).unwrap();
            b.upsert_word("cross-conn", "node-y").unwrap();
        }
        let v1 = read_gen(&a);
        assert!(v1 != v0, "gen write must be visible cross-connection");
        assert!(a.load_words().unwrap().contains(&"cross-conn".to_string()));
        drop(a);
        let _ = std::fs::remove_file(&dir);
    }

    const A: &str = "node-aaaa";
    const B: &str = "node-bbbb";

    fn snippet(store: &Store, trigger: &str) -> Option<String> {
        store
            .load_snippets()
            .unwrap()
            .into_iter()
            .find(|s| s.trigger == trigger)
            .map(|s| s.replacement)
    }

    fn words(store: &Store) -> Vec<String> {
        let mut w = store.load_words().unwrap();
        w.sort();
        w
    }

    /// Full bidirectional exchange, as one sync round-trip would do.
    fn sync(a: &Store, b: &Store) {
        b.apply_words(&a.export_words().unwrap()).unwrap();
        a.apply_words(&b.export_words().unwrap()).unwrap();
        b.apply_snippets(&a.export_snippets().unwrap()).unwrap();
        a.apply_snippets(&b.export_snippets().unwrap()).unwrap();
        b.apply_history(&a.export_history().unwrap()).unwrap();
        a.apply_history(&b.export_history().unwrap()).unwrap();
    }

    #[test]
    fn divergent_edits_converge_with_newest_winning() {
        let a = Store::open_memory().unwrap();
        let b = Store::open_memory().unwrap();

        // Common base: same snippet on both (older version).
        for store in [&a, &b] {
            store
                .apply_snippets(&[SnippetRecord {
                    trigger: "omw".to_string(),
                    replacement: "On my way".to_string(),
                    updated_at: 1000,
                    updated_by: A.to_string(),
                    deleted: false,
                }])
                .unwrap();
            store
                .apply_words(&[WordRecord {
                    word: "gone".to_string(),
                    updated_at: 1000,
                    updated_by: A.to_string(),
                    deleted: false,
                }])
                .unwrap();
        }

        // Diverge with no contact: A edits the snippet at t=2000,
        // B edits the SAME snippet at t=3000. A adds w1, B adds w2,
        // A deletes "gone" at t=2500.
        a.apply_snippets(&[SnippetRecord {
            trigger: "omw".to_string(),
            replacement: "On my way home".to_string(),
            updated_at: 2000,
            updated_by: A.to_string(),
            deleted: false,
        }])
        .unwrap();
        b.apply_snippets(&[SnippetRecord {
            trigger: "omw".to_string(),
            replacement: "On my way!!".to_string(),
            updated_at: 3000,
            updated_by: B.to_string(),
            deleted: false,
        }])
        .unwrap();
        a.apply_words(&[WordRecord {
            word: "w1".to_string(),
            updated_at: 2000,
            updated_by: A.to_string(),
            deleted: false,
        }])
        .unwrap();
        b.apply_words(&[WordRecord {
            word: "w2".to_string(),
            updated_at: 3000,
            updated_by: B.to_string(),
            deleted: false,
        }])
        .unwrap();
        a.apply_words(&[WordRecord {
            word: "gone".to_string(),
            updated_at: 2500,
            updated_by: A.to_string(),
            deleted: true,
        }])
        .unwrap();
        a.apply_history(&[HistoryRecord {
            uuid: "a-1".to_string(),
            text: "hello from a".to_string(),
            raw_text: "hello from a".to_string(),
            words: 3,
            duration_secs: 5,
            deleted: false,
            discarded: false,
            recorded_at: 2000,
        }])
        .unwrap();
        b.apply_history(&[HistoryRecord {
            uuid: "b-1".to_string(),
            text: "hello from b".to_string(),
            raw_text: "raw from b".to_string(),
            words: 3,
            duration_secs: 5,
            deleted: false,
            discarded: false,
            recorded_at: 3000,
        }])
        .unwrap();

        // Reconnect and sync.
        sync(&a, &b);

        // Newer concurrent edit wins on BOTH sides...
        assert_eq!(snippet(&a, "omw").as_deref(), Some("On my way!!"));
        assert_eq!(snippet(&b, "omw").as_deref(), Some("On my way!!"));
        // ...disjoint adds union...
        assert_eq!(words(&a), ["w1", "w2"]);
        assert_eq!(words(&b), ["w1", "w2"]);
        // ...the newer tombstone deletes everywhere...
        assert!(!words(&a).contains(&"gone".to_string()));
        assert!(!words(&b).contains(&"gone".to_string()));
        // ...history unions by uuid, raw survives the round trip...
        assert_eq!(a.load_history(10, false).unwrap().len(), 2);
        assert_eq!(b.load_history(10, false).unwrap().len(), 2);
        let raws: Vec<String> = a
            .load_history(10, false)
            .unwrap()
            .iter()
            .map(|h| h.raw_text.clone())
            .collect();
        assert!(raws.contains(&"raw from b".to_string()));
        // ...a history tombstone deletes everywhere and never
        // resurrects from the peer that still holds the row...
        a.delete_history("b-1").unwrap();
        assert_eq!(a.load_history(10, false).unwrap().len(), 1);
        sync(&a, &b);
        assert_eq!(a.load_history(10, false).unwrap().len(), 1);
        assert_eq!(b.load_history(10, false).unwrap().len(), 1);

        // Sync is idempotent: a second round changes nothing.
        let before_a = a.export_snippets().unwrap();
        sync(&a, &b);
        assert_eq!(a.export_snippets().unwrap(), before_a);
        assert_eq!(snippet(&a, "omw").as_deref(), Some("On my way!!"));
    }

    #[test]
    fn older_write_does_not_resurrect() {
        let a = Store::open_memory().unwrap();
        a.apply_snippets(&[SnippetRecord {
            trigger: "k".to_string(),
            replacement: "new".to_string(),
            updated_at: 5000,
            updated_by: A.to_string(),
            deleted: false,
        }])
        .unwrap();
        // A delayed/duplicate older record must not win.
        a.apply_snippets(&[SnippetRecord {
            trigger: "k".to_string(),
            replacement: "stale".to_string(),
            updated_at: 4000,
            updated_by: B.to_string(),
            deleted: false,
        }])
        .unwrap();
        assert_eq!(snippet(&a, "k").as_deref(), Some("new"));
    }
}

#[cfg(test)]
mod release_regressions {
    #[test]
    fn simultaneous_settings_writers_preserve_disjoint_changes_without_snapshot_upgrade_failure() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("synthetic-settings.db");
        let store = super::Store::open_with_key(&path, &[53; 32]).unwrap();
        let mut raw = serde_json::to_value(store.load_settings()).unwrap();
        raw["synthetic_future_field"] = serde_json::json!({"preserved":true});
        store
            .conn
            .execute(
                "UPDATE kv SET value=?1 WHERE key='settings'",
                [raw.to_string()],
            )
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writers = [true, false].map(|privacy| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let store = super::Store::open_with_key(&path, &[53; 32]).unwrap();
                let mut failures = Vec::new();
                for index in 0..64 {
                    let mut settings = store.load_settings();
                    if privacy {
                        settings.privacy.history_enabled = index % 2 == 0;
                    } else {
                        settings.sync.enabled = index % 2 != 0;
                    }
                    barrier.wait();
                    if let Err(error) = store.save_settings(&settings) {
                        failures.push(format!("round {index}: {error:?}"));
                    }
                    barrier.wait();
                }
                failures
            })
        });
        let failures = writers
            .into_iter()
            .flat_map(|writer| writer.join().unwrap())
            .collect::<Vec<_>>();
        assert!(
            failures.is_empty(),
            "simultaneous saves failed: {failures:?}"
        );
        let settings = store.load_settings();
        assert!(!settings.privacy.history_enabled);
        assert!(settings.sync.enabled);
        let raw: String = store
            .conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&raw).unwrap()["synthetic_future_field"]["preserved"],
            true
        );
    }

    use super::*;

    #[test]
    fn atomic_sync_authorization_rejects_missing_and_unreadable_permissions() {
        let store = Store::open_memory().unwrap();
        let peer = crate::device::DeviceIdentity::ephemeral().node_id;
        store
            .upsert_peer(&peer, "synthetic trusted peer", "", true)
            .unwrap();
        store
            .add_history(
                "synthetic private raw",
                "synthetic private final",
                3,
                1,
                "synthetic",
            )
            .unwrap();
        let epoch = store.sync_epoch();
        assert_eq!(
            store.export_authorized(&peer, epoch).unwrap().history.len(),
            1
        );
        let original: String = store
            .conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                row.get(0)
            })
            .unwrap();
        for variant in 0..6 {
            let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
            match variant {
                0 => {
                    value.as_object_mut().unwrap().remove("sync");
                }
                1 => {
                    value["sync"].as_object_mut().unwrap().remove("enabled");
                }
                2 => {
                    value["sync"]["enabled"] = serde_json::Value::Null;
                }
                3 => {
                    value["settings_version"] = serde_json::Value::from(99);
                }
                4 => {
                    value["sync"]["enabled"] = serde_json::Value::Bool(false);
                }
                5 => {
                    store
                        .conn
                        .execute("DELETE FROM kv WHERE key='settings'", [])
                        .unwrap();
                }
                _ => unreachable!(),
            }
            if variant != 5 {
                store
                    .conn
                    .execute(
                        "UPDATE kv SET value=?1 WHERE key='settings'",
                        [value.to_string()],
                    )
                    .unwrap();
            }
            assert_eq!(
                store.sync_epoch(),
                epoch,
                "fixture must preserve the earlier round's generation"
            );
            assert!(
                store.export_authorized(&peer, epoch).is_err(),
                "missing or unreadable permission exported private speech: {variant}"
            );
            let incoming = [WordRecord {
                word: format!("unauthorized-{variant}"),
                updated_at: now_ms(),
                updated_by: peer.clone(),
                deleted: false,
            }];
            assert!(
                store
                    .apply_sync_round(&peer, epoch, "synthetic", (&incoming, &[], &[]), &[])
                    .is_err(),
                "missing or unreadable permission committed received speech: {variant}"
            );
            assert!(
                !store.load_words().unwrap().contains(&incoming[0].word),
                "rejected sync round partially wrote records"
            );
            store.conn.execute("INSERT INTO kv(key,value) VALUES ('settings',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[&original]).unwrap();
            assert_eq!(
                store.export_authorized(&peer, epoch).unwrap().history.len(),
                1,
                "explicit repaired permission must remain supported"
            );
        }
    }

    #[test]
    fn stale_authorized_exports_reject_every_privacy_boundary() {
        for change in ["off", "off_on", "revoke", "reset", "delete"] {
            let store = Store::open_memory().unwrap();
            let peer = crate::device::DeviceIdentity::ephemeral().node_id;
            store
                .upsert_peer(&peer, "synthetic peer", "", true)
                .unwrap();
            let uuid = store
                .add_history(
                    "synthetic secret raw",
                    "synthetic secret final",
                    3,
                    1,
                    "synthetic node",
                )
                .unwrap();
            let epoch = store.sync_epoch();
            assert_eq!(
                store.export_authorized(&peer, epoch).unwrap().history.len(),
                1
            );
            match change {
                "off" | "off_on" => {
                    let mut settings = store.load_settings();
                    settings.sync.enabled = false;
                    store.save_settings(&settings).unwrap();
                    if change == "off_on" {
                        settings.sync.enabled = true;
                        store.save_settings(&settings).unwrap();
                    }
                }
                "revoke" => store.remove_peer(&peer).unwrap(),
                "reset" => store.reset_all_data().unwrap(),
                "delete" => store.delete_history(&uuid).unwrap(),
                _ => unreachable!(),
            }
            assert!(
                store.export_authorized(&peer, epoch).is_err(),
                "stale snapshot escaped {change}"
            );
            if change == "delete" {
                let current = store.export_authorized(&peer, store.sync_epoch()).unwrap();
                assert!(
                    current.history[0].deleted
                        && current.history[0].text.is_empty()
                        && current.history[0].raw_text.is_empty()
                );
            }
        }
    }

    #[test]
    fn received_round_authorization_is_checked_inside_the_atomic_commit() {
        let peer = crate::device::DeviceIdentity::ephemeral().node_id;
        let self_id = crate::device::DeviceIdentity::ephemeral().node_id;
        let newcomer = crate::device::DeviceIdentity::ephemeral().node_id;
        let record = WordRecord {
            word: "synthetic pending secret".into(),
            updated_at: now_ms(),
            updated_by: peer.clone(),
            deleted: false,
        };
        let gossip = PeerGossip {
            node_id: newcomer.clone(),
            name: "synthetic newcomer".into(),
            addresses: vec!["127.0.0.1:1".into()],
            last_seen: 0,
        };
        for change in ["off", "revoke", "off_on", "reset"] {
            let store = Store::open_memory().unwrap();
            store.upsert_peer(&peer, "peer", "", true).unwrap();
            let epoch = store.sync_epoch();
            match change {
                "off" => {
                    let mut settings = store.load_settings();
                    settings.sync.enabled = false;
                    store.save_settings(&settings).unwrap();
                }
                "revoke" => store.remove_peer(&peer).unwrap(),
                "off_on" => {
                    let mut settings = store.load_settings();
                    settings.sync.enabled = false;
                    store.save_settings(&settings).unwrap();
                    settings.sync.enabled = true;
                    store.save_settings(&settings).unwrap();
                }
                _ => store.reset_all_data().unwrap(),
            }
            assert!(
                store
                    .apply_sync_round(
                        &peer,
                        epoch,
                        &self_id,
                        (std::slice::from_ref(&record), &[], &[]),
                        std::slice::from_ref(&gossip)
                    )
                    .is_err()
            );
            assert!(store.load_words().unwrap().is_empty());
            assert!(store.get_peer(&newcomer).unwrap().is_none());
        }
        let store = Store::open_memory().unwrap();
        store.upsert_peer(&peer, "peer", "", true).unwrap();
        assert!(!store.get_peer(&peer).unwrap().unwrap().introducer);
        store.set_peer_introducer(&peer, true).unwrap();
        let epoch = store.sync_epoch();
        let generation = read_gen(&store);
        let malformed = SnippetRecord {
            trigger: "bad version".into(),
            replacement: "never committed".into(),
            updated_at: i64::MAX,
            updated_by: peer.clone(),
            deleted: false,
        };
        assert!(
            store
                .apply_sync_round(
                    &peer,
                    epoch,
                    &self_id,
                    (std::slice::from_ref(&record), &[malformed], &[]),
                    std::slice::from_ref(&gossip)
                )
                .is_err()
        );
        assert!(store.load_words().unwrap().is_empty());
        assert_eq!(read_gen(&store), generation);
        assert!(store.get_peer(&newcomer).unwrap().is_none());
        assert_eq!(
            store
                .apply_sync_round(
                    &peer,
                    epoch,
                    &self_id,
                    (std::slice::from_ref(&record), &[], &[]),
                    std::slice::from_ref(&gossip)
                )
                .unwrap(),
            1
        );
        assert!(store.get_peer(&newcomer).unwrap().unwrap().trusted);
        store.remove_peer(&newcomer).unwrap();
        assert_eq!(
            store
                .apply_sync_round(
                    &peer,
                    store.sync_epoch(),
                    &self_id,
                    (&[], &[], &[]),
                    &[gossip]
                )
                .unwrap(),
            0
        );
        assert!(store.get_peer(&newcomer).unwrap().is_none());
    }

    #[test]
    fn deletion_clear_expiry_and_merge_never_export_content_or_resurrect() {
        for deletion in 0..3 {
            let a = Store::open_memory().unwrap();
            let uuid = a
                .add_history("synthetic raw secret", "synthetic final secret", 4, 2, "A")
                .unwrap();
            let normal = a.export_history().unwrap().remove(0);
            let mut discard = normal.clone();
            discard.discarded = true;
            match deletion {
                0 => a.delete_history(&uuid).unwrap(),
                1 => {
                    a.clear_history().unwrap();
                }
                _ => {
                    a.conn
                        .execute("UPDATE history SET recorded_at=0", [])
                        .unwrap();
                    a.purge_old_history(1).unwrap();
                }
            }
            let deleted = a.export_history().unwrap().remove(0);
            assert!(deleted.deleted && deleted.text.is_empty() && deleted.raw_text.is_empty());
            for order in [
                [0, 1, 2],
                [0, 2, 1],
                [1, 0, 2],
                [1, 2, 0],
                [2, 0, 1],
                [2, 1, 0],
            ] {
                let b = Store::open_memory().unwrap();
                let records = [normal.clone(), discard.clone(), deleted.clone()];
                for _ in 0..3 {
                    for i in order {
                        b.apply_history(&[records[i].clone()]).unwrap();
                    }
                }
                assert!(b.load_history(10, true).unwrap().is_empty());
                let row = b.export_history().unwrap().remove(0);
                assert!(row.deleted && row.discarded);
                assert!(row.text.is_empty() && row.raw_text.is_empty());
                let stored: (String, String) = b
                    .conn
                    .query_row("SELECT text,raw_text FROM history", [], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .unwrap();
                assert_eq!(stored, (String::new(), String::new()));
            }
        }
    }

    #[test]
    fn rapid_local_edits_advance_past_future_remote_versions() {
        let store = Store::open_memory().unwrap();
        store
            .apply_words(&[WordRecord {
                word: "word".into(),
                updated_at: now_ms() + 86_400_000,
                updated_by: "Z".into(),
                deleted: false,
            }])
            .unwrap();
        for _ in 0..100 {
            store.delete_word("word", "A").unwrap();
            assert!(store.load_words().unwrap().is_empty());
            store.upsert_word("word", "A").unwrap();
            assert_eq!(store.load_words().unwrap(), ["word"]);
        }
        let before = store.export_words().unwrap();
        assert!(
            store
                .apply_words(&[WordRecord {
                    word: "invalid".into(),
                    updated_at: i64::MAX,
                    updated_by: "B".into(),
                    deleted: false
                }])
                .is_err()
        );
        assert_eq!(store.export_words().unwrap(), before);
    }

    #[test]
    fn content_round_is_atomic_and_repeated_snapshots_do_not_refresh_generation() {
        let store = Store::open_memory().unwrap();
        let word = WordRecord {
            word: "one".into(),
            updated_at: 1,
            updated_by: "A".into(),
            deleted: false,
        };
        let snippet = SnippetRecord {
            trigger: "two".into(),
            replacement: "private".into(),
            updated_at: i64::MAX,
            updated_by: "A".into(),
            deleted: false,
        };
        assert!(
            store
                .apply_content(std::slice::from_ref(&word), &[snippet], &[])
                .is_err()
        );
        assert!(store.load_words().unwrap().is_empty());
        store.apply_words(std::slice::from_ref(&word)).unwrap();
        let generation = read_gen(&store);
        store.apply_words(&[word]).unwrap();
        assert_eq!(read_gen(&store), generation);
    }

    #[test]
    fn concurrent_merges_and_local_edits_preserve_newest_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        Store::open_with_key(&path, &[3u8; 32]).unwrap();
        let gate = std::sync::Arc::new(std::sync::Barrier::new(12));
        let workers: Vec<_> = (1..=12)
            .map(|version| {
                let path = path.clone();
                let gate = gate.clone();
                std::thread::spawn(move || {
                    let store = Store::open_with_key(&path, &[3u8; 32]).unwrap();
                    gate.wait();
                    for _ in 0..30 {
                        store
                            .apply_words(&[WordRecord {
                                word: "race".into(),
                                updated_at: version,
                                updated_by: "A".into(),
                                deleted: false,
                            }])
                            .unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let store = Store::open_with_key(&path, &[3u8; 32]).unwrap();
        assert_eq!(store.export_words().unwrap()[0].updated_at, 12);
        store.delete_word("race", "A").unwrap();
        assert!(store.load_words().unwrap().is_empty());
    }

    #[test]
    fn reset_clears_trust_preserves_tombstones_and_blocks_gossip_repair() {
        let store = Store::open_memory().unwrap();
        let peer = crate::device::DeviceIdentity::ephemeral().node_id;
        store
            .upsert_peer(&peer, "peer", "127.0.0.1:1", true)
            .unwrap();
        let uuid = store.add_history("raw", "final", 1, 1, "A").unwrap();
        let old = store.export_history().unwrap();
        store.reset_all_data().unwrap();
        assert!(!store.load_settings().sync.enabled);
        assert!(store.list_peers().unwrap().is_empty());
        store.apply_history(&old).unwrap();
        assert!(store.load_history(10, true).unwrap().is_empty());
        assert_eq!(store.export_history().unwrap()[0].uuid, uuid);
        assert_eq!(
            store
                .apply_gossip(
                    &[PeerGossip {
                        node_id: peer.clone(),
                        name: "peer".into(),
                        addresses: vec![],
                        last_seen: 0
                    }],
                    true,
                    "A"
                )
                .unwrap(),
            0
        );
        assert!(store.is_revoked(&peer).unwrap());
        store.upsert_peer(&peer, "peer", "", true).unwrap();
        assert!(!store.is_revoked(&peer).unwrap());
        assert!(!store.get_peer(&peer).unwrap().unwrap().introducer);
    }

    #[test]
    fn discarded_only_merges_converge_without_retaining_speech() {
        let normal = HistoryRecord {
            uuid: "synthetic-discard-merge".into(),
            text: "synthetic private speech".into(),
            raw_text: "synthetic private raw speech".into(),
            words: 4,
            duration_secs: 17,
            recorded_at: 123,
            deleted: false,
            discarded: false,
        };
        let discarded = HistoryRecord {
            text: String::new(),
            raw_text: String::new(),
            words: 0,
            discarded: true,
            ..normal.clone()
        };
        let malicious_discarded = HistoryRecord {
            discarded: true,
            ..normal.clone()
        };
        let mut expected = None;
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let store = Store::open_memory().unwrap();
            let snapshots = [
                normal.clone(),
                discarded.clone(),
                malicious_discarded.clone(),
            ];
            for _ in 0..3 {
                for index in order {
                    store.apply_history(&[snapshots[index].clone()]).unwrap();
                }
            }
            let row = store.export_history().unwrap().remove(0);
            assert!(row.discarded && !row.deleted);
            assert!(row.text.is_empty() && row.raw_text.is_empty());
            assert_eq!(row.words, 0);
            assert_eq!(row.duration_secs, 17, "discard markers retain duration");
            if let Some(expected) = &expected {
                assert_eq!(&row, expected);
            } else {
                expected = Some(row);
            }
        }
    }

    #[test]
    fn schema_three_upgrade_redacts_discarded_rows_without_losing_marker_duration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("synthetic.db");
        let store = Store::open_with_key(&path, &[8; 32]).unwrap();
        store.conn.execute("INSERT INTO history(uuid,text,raw_text,words,duration_secs,discarded) VALUES ('marker','synthetic secret','synthetic raw',2,17,1)", []).unwrap();
        store.conn.execute_batch("PRAGMA user_version=3").unwrap();
        drop(store);
        for _ in 0..2 {
            let upgraded = Store::open_with_key(&path, &[8; 32]).unwrap();
            let row = upgraded.export_history().unwrap().remove(0);
            assert!(row.discarded && row.text.is_empty() && row.raw_text.is_empty());
            assert_eq!((row.words, row.duration_secs), (0, 17));
            assert_eq!(
                upgraded
                    .conn
                    .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                8
            );
        }
    }

    #[test]
    fn thinking_model_default_survives_reload_and_missing_legacy_field_migrates_once() {
        let store = Store::open_memory().unwrap();
        for choice in [None, Some(true), Some(false)] {
            let mut settings = store.load_settings();
            settings.language_models.entries[0].thinking = choice;
            store.save_settings(&settings).unwrap();
            assert_eq!(
                store.load_settings().language_models.entries[0].thinking,
                choice
            );
        }
        let mut raw = serde_json::to_value(store.load_settings()).unwrap();
        raw["language_models"]["entries"][0]
            .as_object_mut()
            .unwrap()
            .remove("thinking");
        store
            .conn
            .execute(
                "UPDATE kv SET value=?1 WHERE key='settings'",
                [raw.to_string()],
            )
            .unwrap();
        let mut migrated = store.load_settings();
        assert_eq!(migrated.language_models.entries[0].thinking, Some(false));
        store.save_settings(&migrated).unwrap();
        migrated.language_models.entries[0].thinking = None;
        store.save_settings(&migrated).unwrap();
        assert_eq!(
            store.load_settings().language_models.entries[0].thinking,
            None
        );
        let saved: String = store
            .conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&saved).unwrap()["language_models"]["entries"][0]["thinking"].is_null());
    }

    #[test]
    fn incomplete_existing_settings_never_restore_missing_permissions() {
        for missing in [
            "privacy",
            "sync",
            "hotkeys",
            "privacy.history_enabled",
            "privacy.keep_audio_recordings",
            "privacy.save_discarded_transcriptions",
            "sync.enabled",
            "hotkeys.hotkeys_enabled",
            "sync.advertise",
        ] {
            let store = Store::open_memory().unwrap();
            let mut settings = AppSettings::default();
            settings.sync.enabled = false;
            settings.sync.advertise = false;
            settings.privacy.history_enabled = false;
            settings.hotkeys.hotkeys_enabled = false;
            let mut raw = serde_json::to_value(settings).unwrap();
            if let Some((section, field)) = missing.split_once('.') {
                raw[section].as_object_mut().unwrap().remove(field);
            } else {
                raw.as_object_mut().unwrap().remove(missing);
            }
            store
                .conn
                .execute(
                    "INSERT OR REPLACE INTO kv(key,value) VALUES ('settings',?1)",
                    [raw.to_string()],
                )
                .unwrap();
            let loaded = store.load_settings();
            assert!(!loaded.sync.enabled && !loaded.sync.advertise, "{missing}");
            assert!(
                !loaded.privacy.history_enabled
                    && !loaded.privacy.keep_audio_recordings
                    && !loaded.privacy.save_discarded_transcriptions,
                "{missing}"
            );
            assert!(!loaded.hotkeys.hotkeys_enabled, "{missing}");
            let original: String = store
                .conn
                .query_row("SELECT value FROM kv WHERE key='settings'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(original, raw.to_string());
            store.save_settings(&loaded).unwrap();
            let again = store.load_settings();
            assert!(
                !again.sync.enabled
                    && !again.privacy.history_enabled
                    && !again.hotkeys.hotkeys_enabled
            );
        }
    }

    #[test]
    fn settings_read_failure_is_conservative_and_recoverable() {
        let store = Store::open_memory().unwrap();
        store
            .conn
            .execute_batch("ALTER TABLE kv RENAME TO saved_kv")
            .unwrap();
        let failed = store.load_settings();
        assert!(
            !failed.sync.enabled
                && !failed.privacy.history_enabled
                && !failed.hotkeys.hotkeys_enabled
        );
        assert!(store.save_settings(&failed).is_err());
        assert!(store.try_load_settings().is_err());
        assert!(store.maintain_retention_at(0).is_err());
        assert_eq!(
            store.settings_warning(),
            Some("settings.recovery.unreadable")
        );
        store
            .conn
            .execute_batch("ALTER TABLE saved_kv RENAME TO kv")
            .unwrap();
        assert!(
            store.load_settings().sync.enabled,
            "a genuinely new configuration retains its documented defaults"
        );
    }

    #[test]
    fn missing_retention_policy_preserves_data_until_explicit_recovery() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open_with_key(&root.path().join("synthetic.db"), &[27; 32]).unwrap();
        let now = 1_000_000;
        store
            .apply_history(&[HistoryRecord {
                uuid: "synthetic-old".into(),
                text: "synthetic saved speech".into(),
                raw_text: "synthetic saved raw".into(),
                words: 3,
                duration_secs: 4,
                recorded_at: now - 2 * 86400,
                deleted: false,
                discarded: false,
            }])
            .unwrap();
        let vault = crate::audio::audio_vault_dir(root.path());
        crate::private_fs::directory(&vault).unwrap();
        let clip = vault.join("synthetic-old.wav");
        std::fs::write(&clip, b"synthetic audio").unwrap();
        let mut raw = serde_json::to_value(store.load_settings()).unwrap();
        raw["privacy"]
            .as_object_mut()
            .unwrap()
            .remove("transcript_auto_delete_days");
        store
            .conn
            .execute(
                "UPDATE kv SET value=?1 WHERE key='settings'",
                [raw.to_string()],
            )
            .unwrap();
        assert_eq!(store.maintain_retention_at(now).unwrap(), (0, 0));
        assert!(clip.exists());
        assert_eq!(
            store.export_history().unwrap()[0].text,
            "synthetic saved speech"
        );
        // Missing capture permissions must not disable an otherwise explicit
        // retention policy, or extend retention without the user's consent.
        raw["privacy"]["transcript_auto_delete_days"] = serde_json::json!(1);
        raw["privacy"]["audio_retention_days"] = serde_json::json!(1);
        raw["privacy"]
            .as_object_mut()
            .unwrap()
            .remove("history_enabled");
        store
            .conn
            .execute(
                "UPDATE kv SET value=?1 WHERE key='settings'",
                [raw.to_string()],
            )
            .unwrap();
        assert!(!store.load_settings().privacy.history_enabled);
        assert_eq!(store.maintain_retention_at(now).unwrap(), (1, 1));
        assert!(!clip.exists());
        assert!(store.export_history().unwrap()[0].deleted);
    }

    #[test]
    fn missing_or_future_settings_cannot_be_treated_as_a_new_configuration() {
        let store = Store::open_memory().unwrap();
        for raw in [
            "null",
            "[]",
            "42",
            r#""synthetic invalid settings""#,
            r#"{"settings_version":2}"#,
        ] {
            store
                .conn
                .execute("UPDATE kv SET value=?1 WHERE key='settings'", [raw])
                .unwrap();
            assert!(store.try_load_settings().is_err());
            let safe = store.load_settings();
            assert!(
                !safe.sync.enabled
                    && !safe.privacy.history_enabled
                    && !safe.hotkeys.hotkeys_enabled
            );
            assert!(store.save_settings(&safe).is_err());
            assert_eq!(
                store
                    .conn
                    .query_row("SELECT value FROM kv WHERE key='settings'", [], |r| r
                        .get::<_, String>(0))
                    .unwrap(),
                raw
            );
        }
        store
            .conn
            .execute("DELETE FROM kv WHERE key='settings'", [])
            .unwrap();
        assert!(store.try_load_settings().is_err());
        let safe = store.load_settings();
        assert!(!safe.sync.enabled && !safe.privacy.history_enabled);
        assert!(store.save_settings(&safe).is_err());
    }

    #[test]
    fn old_missing_settings_recover_conservatively_without_losing_user_data() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("synthetic.db");
        let store = Store::open_with_key(&path, &[9; 32]).unwrap();
        store
            .upsert_word("synthetic saved word", "synthetic node")
            .unwrap();
        store
            .conn
            .execute("DELETE FROM kv WHERE key='settings'", [])
            .unwrap();
        store.conn.execute_batch("PRAGMA user_version=4").unwrap();
        drop(store);
        let upgraded = Store::open_with_key(&path, &[9; 32]).unwrap();
        let recovered = upgraded.try_load_settings().unwrap();
        assert!(
            !recovered.sync.enabled
                && !recovered.privacy.history_enabled
                && !recovered.hotkeys.hotkeys_enabled
        );
        assert_eq!(upgraded.load_words().unwrap(), ["synthetic saved word"]);
        drop(upgraded);
        let fresh = Store::open_with_key(&root.path().join("fresh.db"), &[10; 32]).unwrap();
        assert!(fresh.try_load_settings().unwrap().sync.enabled);
        drop(fresh);
        assert!(
            Store::open_with_key(&root.path().join("fresh.db"), &[10; 32])
                .unwrap()
                .try_load_settings()
                .unwrap()
                .sync
                .enabled
        );
    }

    #[test]
    fn settings_edits_preserve_unknown_root_nested_and_entry_fields() {
        let store = Store::open_memory().unwrap();
        let mut raw = serde_json::to_value(store.load_settings()).unwrap();
        raw["future_root"] = serde_json::json!({"value":"synthetic future setting"});
        raw["preferences"]["future_nested"] = serde_json::Value::Bool(true);
        raw["dictionary"]["future_nested"] = serde_json::Value::Bool(true);
        raw["language_models"]["entries"][0]["future_entry"] = serde_json::Value::from(42);
        store
            .conn
            .execute(
                "INSERT OR REPLACE INTO kv(key,value) VALUES ('settings',?1)",
                [raw.to_string()],
            )
            .unwrap();
        let mut loaded = store.load_settings();
        loaded.preferences.theme = crate::settings::Theme::Dark;
        loaded.language_models.entries[0].name = "edited synthetic entry".into();
        store.save_settings(&loaded).unwrap();
        let saved: String = store
            .conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(saved["future_root"], raw["future_root"]);
        assert_eq!(
            saved["preferences"]["future_nested"],
            raw["preferences"]["future_nested"]
        );
        assert_eq!(saved["language_models"]["entries"][0]["future_entry"], 42);
        assert_eq!(saved["dictionary"]["future_nested"], true);
    }

    #[test]
    fn settings_missing_fields_preserve_privacy_and_corrupt_blobs_cannot_be_overwritten() {
        let store = Store::open_memory().unwrap();
        let raw = r#"{"sync":{"enabled":false},"privacy":{"history_enabled":false}}"#;
        store
            .conn
            .execute(
                "INSERT OR REPLACE INTO kv(key,value) VALUES ('settings',?1)",
                [raw],
            )
            .unwrap();
        let settings = store.load_settings();
        assert!(!settings.sync.enabled && !settings.privacy.history_enabled);
        store.save_settings(&settings).unwrap();
        let invalid = r#"{"sync":{"enabled":false},"preferences":{"theme":"unknown"}}"#;
        store
            .conn
            .execute("UPDATE kv SET value=?1 WHERE key='settings'", [invalid])
            .unwrap();
        assert!(!store.load_settings().sync.enabled);
        assert!(store.save_settings(&AppSettings::default()).is_err());
        let kept: String = store
            .conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kept, invalid);
    }

    #[test]
    fn two_frontend_snapshots_patch_changed_preferences_without_copying_dictionary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let a = Store::open_with_key(&path, &[5u8; 32]).unwrap();
        let b = Store::open_with_key(&path, &[5u8; 32]).unwrap();
        let mut sa = a.load_settings();
        let mut sb = b.load_settings();
        sa.sync.enabled = false;
        a.save_settings(&sa).unwrap();
        sb.privacy.history_enabled = false;
        sb.dictionary.snippets.push(Snippet {
            trigger: "x".into(),
            replacement: "synthetic secret".into(),
        });
        b.save_settings(&sb).unwrap();
        let result = a.load_settings();
        assert!(!result.sync.enabled && !result.privacy.history_enabled);
        let json: String = a
            .conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!json.contains("synthetic secret"));
    }

    #[test]
    fn stale_frontend_provider_edits_preserve_concurrent_additions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("synthetic.db");
        let first = Store::open_with_key(&path, &[96; 32]).unwrap();
        let second = Store::open_with_key(&path, &[96; 32]).unwrap();
        let mut stale = first.try_load_settings().unwrap();
        let mut concurrent = second.try_load_settings().unwrap();
        let mut llm = concurrent.language_models.entries[0].clone();
        llm.id = "synthetic-added-llm".into();
        llm.name = "concurrently added LLM 世界".into();
        let mut stt = concurrent.stt.entries[0].clone();
        stt.id = "synthetic-added-stt".into();
        stt.name = "concurrently added STT 世界".into();
        concurrent.language_models.entries.push(llm);
        concurrent.stt.entries.push(stt);
        second.save_settings(&concurrent).unwrap();

        for name in ["first stale edit", "second stale edit"] {
            stale.language_models.entries[0].name = name.into();
            stale.stt.entries[0].name = name.into();
            first.save_settings(&stale).unwrap();
            let fresh = Store::open_with_key(&path, &[96; 32]).unwrap();
            let saved = fresh.try_load_settings().unwrap();
            assert_eq!(saved.language_models.entries.len(), 2, "lost new LLM");
            assert_eq!(saved.stt.entries.len(), 2, "lost new STT");
            assert_eq!(saved.language_models.entries[0].name, name);
            assert_eq!(saved.stt.entries[0].name, name);
            assert_eq!(saved.language_models.entries[1].id, "synthetic-added-llm");
            assert_eq!(
                saved.language_models.entries[1].name,
                "concurrently added LLM 世界"
            );
            assert_eq!(saved.stt.entries[1].id, "synthetic-added-stt");
            assert_eq!(saved.stt.entries[1].name, "concurrently added STT 世界");
        }
    }

    #[test]
    fn stale_frontend_provider_edits_do_not_restore_deleted_credentials() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("synthetic.db");
        let first = Store::open_with_key(&path, &[97; 32]).unwrap();
        let mut seeded = first.try_load_settings().unwrap();
        seeded.language_models.entries[0].api_key = "synthetic-deleted-llm-key".into();
        seeded.stt.entries[0].api_key = "synthetic-deleted-stt-key".into();
        first.save_settings(&seeded).unwrap();
        let second = Store::open_with_key(&path, &[97; 32]).unwrap();
        let mut stale = first.try_load_settings().unwrap();
        let mut concurrent = second.try_load_settings().unwrap();
        concurrent.language_models.entries[0].id = "synthetic-replacement-llm".into();
        concurrent.language_models.entries[0].api_key.clear();
        concurrent.stt.entries[0].id = "synthetic-replacement-stt".into();
        concurrent.stt.entries[0].api_key.clear();
        second.save_settings(&concurrent).unwrap();

        stale.language_models.entries[0].name = "stale edit of deleted LLM".into();
        stale.stt.entries[0].name = "stale edit of deleted STT".into();
        stale.preferences.theme = crate::settings::Theme::Dark;
        first.save_settings(&stale).unwrap();
        let fresh = Store::open_with_key(&path, &[97; 32]).unwrap();
        let saved = fresh.try_load_settings().unwrap();
        assert_eq!(saved.language_models.entries.len(), 1);
        assert_eq!(
            saved.language_models.entries[0].id,
            "synthetic-replacement-llm"
        );
        assert_eq!(saved.stt.entries.len(), 1);
        assert_eq!(saved.stt.entries[0].id, "synthetic-replacement-stt");
        assert_eq!(saved.preferences.theme, crate::settings::Theme::Dark);
        let raw: String = fresh
            .conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(!raw.contains("synthetic-deleted-llm-key"));
        assert!(!raw.contains("synthetic-deleted-stt-key"));
    }

    #[test]
    fn stale_frontend_provider_reordering_preserves_edits_and_explicit_deletion() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("synthetic.db");
        let first = Store::open_with_key(&path, &[98; 32]).unwrap();
        let mut seeded = first.try_load_settings().unwrap();
        let mut entry = seeded.language_models.entries[0].clone();
        entry.id = "synthetic-second".into();
        seeded.language_models.entries.push(entry);
        first.save_settings(&seeded).unwrap();
        let second = Store::open_with_key(&path, &[98; 32]).unwrap();
        let mut stale = first.try_load_settings().unwrap();
        let mut concurrent = second.try_load_settings().unwrap();
        concurrent.language_models.entries[1].thinking = None;
        concurrent.language_models.entries[1].name = "concurrent edit 世界".into();
        let mut added = concurrent.language_models.entries[1].clone();
        added.id = "synthetic-third".into();
        concurrent.language_models.entries.insert(0, added);
        second.save_settings(&concurrent).unwrap();
        let raw: String = second
            .conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut raw: serde_json::Value = serde_json::from_str(&raw).unwrap();
        raw["language_models"]["entries"][2]["future_entry"] =
            serde_json::json!({"synthetic":"preserve"});
        second
            .conn
            .execute(
                "UPDATE kv SET value=?1 WHERE key='settings'",
                [raw.to_string()],
            )
            .unwrap();

        stale.language_models.entries.swap(0, 1);
        first.save_settings(&stale).unwrap();
        let saved = second.try_load_settings().unwrap();
        assert_eq!(
            saved
                .language_models
                .entries
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            ["synthetic-second", "", "synthetic-third"]
        );
        assert_eq!(
            saved.language_models.entries[0].name,
            "concurrent edit 世界"
        );
        assert_eq!(saved.language_models.entries[0].thinking, None);

        stale.language_models.entries.remove(1);
        first.save_settings(&stale).unwrap();
        let saved = second.try_load_settings().unwrap();
        assert_eq!(
            saved
                .language_models
                .entries
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            ["synthetic-second", "synthetic-third"]
        );
        let raw: String = second
            .conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&raw).unwrap()["language_models"]["entries"]
                [0]["future_entry"]["synthetic"],
            "preserve"
        );
    }

    #[test]
    fn migration_wrong_key_newer_schema_and_simultaneous_openers_preserve_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let plain = Connection::open(&path).unwrap();
        plain.execute_batch(SCHEMA).unwrap();
        plain
            .execute("INSERT INTO words(word) VALUES ('legacy')", [])
            .unwrap();
        drop(plain);
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    Store::open_with_key(&path, &[6u8; 32])
                        .unwrap()
                        .load_words()
                        .unwrap()
                })
            })
            .collect();
        for worker in workers {
            assert_eq!(worker.join().unwrap(), ["legacy"]);
        }
        let before = std::fs::read(&path).unwrap();
        assert!(Store::open_with_key(&path, &[7u8; 32]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let store = Store::open_with_key(&path, &[6u8; 32]).unwrap();
        store.conn.execute_batch("PRAGMA user_version=99").unwrap();
        store.checkpoint().unwrap();
        drop(store);
        assert!(Store::open_with_key(&path, &[6u8; 32]).is_err());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&key_pragma(&[6u8; 32])).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            99
        );
    }
}

#[cfg(test)]
#[path = "store_migration_tests.rs"]
mod migration_tests;

#[cfg(test)]
mod capture_privacy_regressions {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn capture<'a>(wav: Option<&'a [u8]>) -> CapturedUtterance<'a> {
        CapturedUtterance {
            raw: "synthetic raw",
            text: "synthetic final",
            duration_secs: 3,
            recorded_at: now_ms() / 1000,
            discarded: false,
            wav,
        }
    }

    #[test]
    fn privacy_reset_clear_and_off_on_boundaries_reject_old_capture_payloads() {
        for operation in [
            "reset",
            "clear-history",
            "clear-audio",
            "history-off-on",
            "audio-off-on",
        ] {
            let data = tempfile::tempdir().unwrap();
            let path = data.path().join("state.db");
            let store = Store::open_with_key(&path, &[37; 32]).unwrap();
            let mut settings = store.load_settings();
            settings.privacy.keep_audio_recordings = true;
            store.save_settings(&settings).unwrap();
            let epoch = store.capture_epoch().unwrap();
            let other = Store::open_with_key(&path, &[37; 32]).unwrap();
            match operation {
                "reset" => other.reset_all_data().unwrap(),
                "clear-history" => {
                    other.clear_history().unwrap();
                }
                "clear-audio" => {
                    other.clear_audio().unwrap();
                }
                "history-off-on" | "audio-off-on" => {
                    let mut settings = other.load_settings();
                    if operation == "history-off-on" {
                        settings.privacy.history_enabled = false;
                    } else {
                        settings.privacy.keep_audio_recordings = false;
                    }
                    other.save_settings(&settings).unwrap();
                    settings.privacy.history_enabled = true;
                    settings.privacy.keep_audio_recordings = true;
                    other.save_settings(&settings).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                !store
                    .persist_capture(
                        epoch,
                        capture(Some(b"synthetic WAV")),
                        &AtomicBool::new(false)
                    )
                    .unwrap(),
                "{operation}"
            );
            assert!(store.export_history().unwrap().is_empty(), "{operation}");
            let vault = crate::audio::audio_vault_dir(data.path());
            assert!(
                !vault.exists() || std::fs::read_dir(vault).unwrap().count() == 0,
                "{operation}"
            );
        }
    }

    #[test]
    fn audio_and_text_commit_under_current_policy_and_cancel_never_keeps_payloads() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_with_key(&data.path().join("state.db"), &[38; 32]).unwrap();
        let mut settings = store.load_settings();
        settings.privacy.keep_audio_recordings = true;
        store.save_settings(&settings).unwrap();
        let epoch = store.capture_epoch().unwrap();
        assert!(
            !store
                .persist_capture(
                    epoch,
                    capture(Some(b"synthetic WAV")),
                    &AtomicBool::new(true)
                )
                .unwrap()
        );
        assert!(
            store
                .persist_capture(
                    epoch,
                    capture(Some(b"synthetic WAV")),
                    &AtomicBool::new(false)
                )
                .unwrap()
        );
        let rows = store.export_history().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "synthetic final");
        let file = crate::audio::audio_vault_dir(data.path()).join(format!("{}.wav", rows[0].uuid));
        assert_eq!(std::fs::read(&file).unwrap(), b"synthetic WAV");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        store.delete_history(&rows[0].uuid).unwrap();
        assert!(
            !file.exists(),
            "transcript deletion must remove its retained audio"
        );
        assert_eq!(store.export_history().unwrap()[0].raw_text, "");
    }

    #[test]
    fn idle_retention_redacts_expired_text_removes_audio_and_recovers_orphaned_writes() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_with_key(&data.path().join("state.db"), &[39; 32]).unwrap();
        let mut settings = store.load_settings();
        settings.privacy.audio_retention_days = 1;
        settings.privacy.transcript_auto_delete_days = 1;
        store.save_settings(&settings).unwrap();
        let now = now_ms() / 1000;
        let old = HistoryRecord {
            uuid: "expired".into(),
            text: "synthetic private".into(),
            raw_text: "synthetic private raw".into(),
            words: 2,
            duration_secs: 3,
            recorded_at: now - 2 * 86400,
            deleted: false,
            discarded: false,
        };
        let mut fresh = old.clone();
        fresh.uuid = "fresh".into();
        fresh.recorded_at = now;
        store.apply_history(&[old, fresh]).unwrap();
        let vault = crate::audio::audio_vault_dir(data.path());
        crate::private_fs::directory(&vault).unwrap();
        for name in ["expired", "fresh", "orphan"] {
            std::fs::write(vault.join(format!("{name}.wav")), b"synthetic WAV").unwrap();
        }
        assert_eq!(store.maintain_retention_at(now).unwrap(), (1, 2));
        assert!(!vault.join("expired.wav").exists());
        assert!(!vault.join("orphan.wav").exists());
        assert!(vault.join("fresh.wav").exists());
        let rows = store.export_history().unwrap();
        let expired = rows.iter().find(|r| r.uuid == "expired").unwrap();
        assert!(expired.deleted && expired.text.is_empty() && expired.raw_text.is_empty());
        assert_eq!(
            store.maintain_retention_at(now - 86400).unwrap(),
            (0, 0),
            "backward clock movement must not erase fresh clips"
        );
        assert_eq!(
            store.maintain_retention_at(now + 2 * 86400).unwrap(),
            (1, 1)
        );
        assert!(std::fs::read_dir(vault).unwrap().next().is_none());
    }

    #[test]
    fn zero_retention_keeps_history_but_bounds_audio_and_preserves_exact_boundary() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("state.db")).unwrap();
        let mut settings = store.load_settings();
        settings.privacy.audio_retention_days = 0;
        settings.privacy.transcript_auto_delete_days = 0;
        settings.privacy.keep_audio_recordings = false;
        settings.privacy.history_enabled = false;
        store.save_settings(&settings).unwrap();
        let now = now_ms() / 1000;
        let vault = crate::audio::audio_vault_dir(root.path());
        crate::private_fs::directory(&vault).unwrap();
        for (uuid, recorded_at) in [("expired", now - 86401), ("boundary", now - 86400)] {
            store
                .apply_history(&[HistoryRecord {
                    uuid: uuid.into(),
                    text: "synthetic retained text".into(),
                    raw_text: "synthetic raw text".into(),
                    words: 3,
                    duration_secs: 1,
                    recorded_at,
                    deleted: false,
                    discarded: false,
                }])
                .unwrap();
        }
        // Import itself performs pruning. Populate the retained-file fixture
        // after both imports so this check exercises the idle maintenance pass.
        for uuid in ["expired", "boundary"] {
            std::fs::write(vault.join(format!("{uuid}.wav")), b"synthetic WAV").unwrap();
        }
        assert_eq!(store.maintain_retention_at(now).unwrap(), (0, 1));
        assert!(!vault.join("expired.wav").exists());
        assert!(vault.join("boundary.wav").exists());
        assert!(
            store
                .export_history()
                .unwrap()
                .iter()
                .all(|row| !row.deleted)
        );
        // A malformed setting is preserved for repair, with capture/sync
        // disabled. It must not silently select a destructive text policy.
        let invalid = r#"{"privacy":{"transcript_auto_delete_days":-1}}"#;
        store
            .conn
            .execute("UPDATE kv SET value=?1 WHERE key='settings'", [invalid])
            .unwrap();
        let safe = store.load_settings();
        assert!(
            !safe.sync.enabled
                && !safe.privacy.history_enabled
                && !safe.privacy.keep_audio_recordings
        );
        assert_eq!(store.maintain_retention_at(now).unwrap().0, 0);
        assert!(
            store
                .export_history()
                .unwrap()
                .iter()
                .all(|row| !row.deleted)
        );
        assert_eq!(
            store
                .conn
                .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            invalid
        );
    }

    #[test]
    fn background_open_does_not_recreate_a_removed_database_or_accept_a_symlink() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        {
            let store = Store::open(&path).unwrap();
            store.upsert_word("synthetic", "test").unwrap();
        }
        let existing = Store::open_existing(&path).unwrap();
        assert_eq!(existing.load_words().unwrap(), vec!["synthetic"]);
        drop(existing);
        let saved = root.path().join("preserved.db");
        std::fs::rename(&path, &saved).unwrap();
        assert!(Store::open_existing(&path).is_err());
        assert!(!path.exists());
        symlink(&saved, &path).unwrap();
        assert!(Store::open_existing(&path).is_err());
        assert_eq!(
            Store::open(&saved).unwrap().load_words().unwrap(),
            vec!["synthetic"]
        );
    }
}

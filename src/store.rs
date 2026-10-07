//! Local state database: the single replicated file.
//!
//! `state.db` holds settings, dictionary, snippets, history and sync
//! metadata. WAL mode with a checkpoint on clean shutdown keeps it
//! safe to replicate with external file-sync tools, and it is the
//! unit the future node-based P2P sync exchanges.
//!
//! Sync model: every mutable record carries `(updated_at_ms,
//! updated_by, deleted)`. Two devices that diverged from a common
//! version exchange record sets; the union converges and concurrent
//! edits to the same key resolve Last-Writer-Wins on
//! `(updated_at_ms, updated_by)`. Deletes are tombstones so they win
//! over concurrent edits only when newer (same LWW rule).

use rusqlite::Connection;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::settings::{AppSettings, HistoryEntry, Snippet};

/// Milliseconds since the Unix epoch (wall clock). Sync conflict
/// resolution compares these; ties break on node id (total order).
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA user_version = 2;
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

fn bump_gen(conn: &Connection) {
    let _ = conn.execute(
        "INSERT INTO sync_meta (key, value) VALUES ('gen', '1')
         ON CONFLICT (key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
        [],
    );
}

pub struct Store {
    conn: Connection,
}

static HISTORY_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    conn.prepare(&format!("SELECT {column} FROM {table} LIMIT 0"))
        .is_ok()
}

/// Database encryption key (32 bytes). Order: explicit env override
/// (headless/testing) first, otherwise the OS keyring, creating and
/// storing a random key on first run.
fn db_key() -> Result<[u8; 32], String> {
    if let Ok(hexkey) = std::env::var("MYWHISPER_DB_KEY") {
        return hex_decode_key(&hexkey)
            .ok_or_else(|| "MYWHISPER_DB_KEY must be 64 hex chars".to_string());
    }
    let entry = keyring::Entry::new("mywhisper", "state-db")
        .map_err(|e| format!("keyring unavailable: {e} (is a Secret Service running?)"))?;
    match entry.get_password() {
        Ok(stored) => {
            hex_decode_key(&stored).ok_or_else(|| "keyring holds a malformed db key".to_string())
        }
        Err(keyring::Error::NoEntry) => {
            let mut raw = [0u8; 32];
            getrandom::fill(&mut raw).map_err(|e| format!("rng: {e}"))?;
            entry
                .set_password(&hex_encode(&raw))
                .map_err(|e| format!("keyring store failed: {e}"))?;
            Ok(raw)
        }
        Err(e) => Err(format!("keyring read failed: {e}")),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

fn hex_decode_key(s: &str) -> Option<[u8; 32]> {
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

fn is_not_a_database(e: &rusqlite::Error) -> bool {
    format!("{e:?}").contains("not a database")
}

/// Encrypt a legacy plaintext database in place via a temp file:
/// attach fresh encrypted db, export, atomically replace.
fn migrate_plaintext_to_encrypted(path: &Path, key: &[u8; 32]) -> rusqlite::Result<()> {
    let tmp = path.with_extension("db.migrating");
    let _ = std::fs::remove_file(&tmp);
    let plain = Connection::open(path)?;
    let attach = format!(
        "ATTACH DATABASE '{}' AS enc KEY \"x'{}'\"",
        tmp.to_string_lossy().replace('\'', "''"),
        hex_encode(key)
    );
    plain.execute_batch(&attach)?;
    plain.query_row("SELECT sqlcipher_export('enc')", [], |_| Ok(()))?;
    plain.execute_batch("DETACH enc")?;
    drop(plain);
    std::fs::rename(&tmp, path)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(format!("replace db: {e}").into()))?;
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let key = db_key()
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(format!("db key: {e}").into()))?;
        let fresh = !path.exists();
        let conn = Connection::open(path)?;
        conn.execute_batch(&key_pragma(&key))?;
        // Probing a wrongly-keyed (or plaintext) file fails here.
        let probe = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        });
        if probe.is_err() && !fresh {
            let err = probe.unwrap_err();
            if is_not_a_database(&err) {
                drop(conn);
                eprintln!("[store] plaintext database found, encrypting in place");
                migrate_plaintext_to_encrypted(path, &key)?;
                return Self::open(path);
            }
            return Err(err);
        }
        conn.execute_batch(SCHEMA)?;
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
            let _ = conn.execute(stmt, []);
        }
        // History gained a uuid primary key after v1 (which used an
        // integer id). Backfill deterministic uuids for legacy rows so
        // old databases keep working.
        if !has_column(&conn, "history", "uuid") {
            conn.execute("ALTER TABLE history ADD COLUMN uuid TEXT", [])?;
            conn.execute(
                "UPDATE history SET uuid = 'legacy-' || id || '-' || recorded_at
                 WHERE uuid IS NULL",
                [],
            )?;
        }
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS history_uuid ON history (uuid)",
            [],
        )?;
        // Rows predating raw_text carry the final text as raw (the
        // pipeline was passthrough then, so this is exact).
        let _ = conn.execute(
            "UPDATE history SET raw_text = text WHERE raw_text = ''",
            [],
        );
        Ok(Self { conn })
    }

    /// Checkpoint the WAL back into the database file. Call on clean
    /// shutdown so replicators always see a complete file.
    pub fn checkpoint(&self) -> rusqlite::Result<()> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    // -- settings (JSON blob under kv['settings']) -------------------------

    pub fn load_settings(&self) -> AppSettings {
        let json: Option<String> = self
            .conn
            .query_row("SELECT value FROM kv WHERE key = 'settings'", [], |row| {
                row.get(0)
            })
            .ok();
        let mut settings: AppSettings = json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        // Typed tables are authoritative for dictionary content.
        settings.dictionary.words = self.load_words().unwrap_or_default();
        settings.dictionary.snippets = self.load_snippets().unwrap_or_default();
        // Pre-per-vendor databases stored Parakeet for everything
        // sherpa; Cohere gets its own vendor now.
        if matches!(
            settings.stt.local_provider,
            crate::settings::LocalSttProvider::Nvidia
        ) && settings.stt.parakeet_model == "cohere-transcribe-03-2026"
        {
            settings.stt.local_provider = crate::settings::LocalSttProvider::Cohere;
        }
        settings
    }

    /// Persist the settings blob. Versioned tables (words/snippets)
    /// are NOT touched here: they are authoritative and already carry
    /// their own timestamps from CRUD/merge paths. Rewriting them on
    /// every settings save would mint fresh versions and could clobber
    /// a peer's concurrent edit on the next sync.
    pub fn save_settings(&self, settings: &AppSettings) -> rusqlite::Result<()> {
        let json = serde_json::to_string(settings).map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(format!("settings serialize: {e}").into())
        })?;
        self.conn.execute(
            "INSERT INTO kv (key, value) VALUES ('settings', ?1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [&json],
        )?;
        Ok(())
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
        self.apply_words(&[WordRecord {
            word: word.to_string(),
            updated_at: now_ms(),
            updated_by: node_id.to_string(),
            deleted: false,
        }])
    }

    /// Local write: tombstone a word (propagates as a delete).
    pub fn delete_word(&self, word: &str, node_id: &str) -> rusqlite::Result<()> {
        self.apply_words(&[WordRecord {
            word: word.to_string(),
            updated_at: now_ms(),
            updated_by: node_id.to_string(),
            deleted: true,
        }])
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
        self.apply_snippets(&[SnippetRecord {
            trigger: trigger.to_string(),
            replacement: replacement.to_string(),
            updated_at: now_ms(),
            updated_by: node_id.to_string(),
            deleted: false,
        }])
    }

    /// Local write: tombstone a snippet.
    pub fn delete_snippet(&self, trigger: &str, node_id: &str) -> rusqlite::Result<()> {
        self.apply_snippets(&[SnippetRecord {
            trigger: trigger.to_string(),
            replacement: String::new(),
            updated_at: now_ms(),
            updated_by: node_id.to_string(),
            deleted: true,
        }])
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
        self.conn.execute(
            "UPDATE history SET deleted = 1 WHERE uuid = ?1",
            [uuid],
        )?;
        bump_data_gen();
        bump_gen(&self.conn);
        Ok(())
    }

    /// Delete history rows with no duration (pre-fix data that pins
    /// words-per-minute at 0). Returns rows removed.
    pub fn delete_zero_duration_history(&self) -> rusqlite::Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM history WHERE duration_secs <= 0", [])?)
    }

    /// Distinct UTC days (epoch day numbers) with at least one entry,
    /// newest first. Feeds the streak calculation.
    pub fn active_days(&self) -> rusqlite::Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT recorded_at / 86400 FROM history WHERE deleted = 0 AND discarded = 0 ORDER BY 1 DESC")?;
        stmt.query_map([], |row| row.get(0))?.collect()
    }

    /// Per-day aggregates for the insights heatmap: `(epoch_day,
    /// total_words, dictation_count)`, ascending, trailing `days`.
    /// Single indexed range scan -- no per-day queries.
    pub fn daily_aggregates(&self, days: u32) -> rusqlite::Result<Vec<(i64, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT recorded_at / 86400 AS day,
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
    pub fn add_discarded(
        &self,
        duration_secs: i64,
        node_id: &str,
    ) -> rusqlite::Result<String> {
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
        let seq = HISTORY_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let uuid = format!("{}-{}-{seq}", &node_id[..node_id.len().min(16)], now_ms());
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

    /// Delete rows older than `days` (auto-delete transcripts).
    /// Tombstones survive: dropping them would resurrect peers' rows.
    pub fn purge_old_history(&self, days: u32) -> rusqlite::Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM history WHERE deleted = 0 AND recorded_at < strftime('%s', 'now', ?1)",
            [format!("-{days} days")],
        )?)
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

    /// Returns true when `(new_at, new_by)` strictly beats the stored
    /// version: the total order behind every conflict decision.
    fn wins(new_at: i64, new_by: &str, old_at: i64, old_by: &str) -> bool {
        (new_at, new_by) > (old_at, old_by)
    }

    /// Merge remote word records. Union converges; concurrent edits to
    /// the same word resolve to the newer `(updated_at, updated_by)`.
    pub fn apply_words(&self, records: &[WordRecord]) -> rusqlite::Result<()> {
        for r in records {
            let existing: Option<(i64, String)> = self
                .conn
                .query_row(
                    "SELECT updated_at, updated_by FROM words WHERE word = ?1",
                    [&r.word],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .ok();
            let take = match existing {
                None => true,
                Some((old_at, old_by)) => Self::wins(r.updated_at, &r.updated_by, old_at, &old_by),
            };
            if take {
                self.conn.execute(
                    "INSERT INTO words (word, updated_at, updated_by, deleted)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (word) DO UPDATE SET
                       updated_at = excluded.updated_at,
                       updated_by = excluded.updated_by,
                       deleted = excluded.deleted",
                    (&r.word, r.updated_at, &r.updated_by, r.deleted as i64),
                )?;
            }
        }
        bump_data_gen();
        bump_gen(&self.conn);
        Ok(())
    }

    pub fn apply_snippets(&self, records: &[SnippetRecord]) -> rusqlite::Result<()> {
        for r in records {
            let existing: Option<(String, i64, String)> = self
                .conn
                .query_row(
                    "SELECT replacement, updated_at, updated_by FROM snippets WHERE trigger = ?1",
                    [&r.trigger],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .ok();
            let take = match existing {
                None => true,
                Some((_, old_at, old_by)) => {
                    Self::wins(r.updated_at, &r.updated_by, old_at, &old_by)
                }
            };
            if take {
                self.conn.execute(
                    "INSERT INTO snippets (trigger, replacement, updated_at, updated_by, deleted)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (trigger) DO UPDATE SET
                       replacement = excluded.replacement,
                       updated_at = excluded.updated_at,
                       updated_by = excluded.updated_by,
                       deleted = excluded.deleted",
                    (
                        &r.trigger,
                        &r.replacement,
                        r.updated_at,
                        &r.updated_by,
                        r.deleted as i64,
                    ),
                )?;
            }
        }
        bump_data_gen();
        bump_gen(&self.conn);
        Ok(())
    }

    /// Merge remote history: pure union on uuid, never conflicts.
    pub fn apply_history(&self, records: &[HistoryRecord]) -> rusqlite::Result<()> {
        for r in records {
            self.conn.execute(
                "INSERT INTO history (uuid, text, raw_text, words, duration_secs, deleted, discarded, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (uuid) DO UPDATE
                 SET deleted = excluded.deleted, discarded = excluded.discarded
                 WHERE excluded.deleted = 1 OR excluded.discarded = 1",
                (
                    &r.uuid,
                    &r.text,
                    &r.raw_text,
                    r.words,
                    r.duration_secs,
                    r.deleted,
                    r.discarded,
                    r.recorded_at,
                ),
            )?;
        }
        bump_data_gen();
        bump_gen(&self.conn);
        Ok(())
    }

    pub fn open_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
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
    let mut out: Vec<String> = seen.to_vec();
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
        let peer = self
            .conn
            .query_row(
                "SELECT node_id, name, address, addresses, trusted, pending,
                        introducer, last_seen, last_sync
                 FROM devices WHERE node_id = ?1",
                [node_id],
                Self::peer_from_row,
            )
            .ok();
        Ok(peer)
    }

    /// Insert or update a peer row (manual add / edits).
    pub fn upsert_peer(
        &self,
        node_id: &str,
        name: &str,
        address: &str,
        trusted: bool,
    ) -> rusqlite::Result<()> {
        let addresses = if address.is_empty() {
            "[]".to_string()
        } else {
            dump_addresses(&[address.to_string()])
        };
        self.conn.execute(
            "INSERT INTO devices
               (node_id, name, address, addresses, trusted, pending, introducer,
                last_seen, last_sync)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?5, 0, 0)
             ON CONFLICT (node_id) DO UPDATE SET
               name = excluded.name,
               address = excluded.address,
               addresses = excluded.addresses,
               trusted = excluded.trusted,
               introducer = excluded.introducer,
               pending = 0",
            (node_id, name, address, addresses, trusted as i64),
        )?;
        Ok(())
    }

    pub fn remove_peer(&self, node_id: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM devices WHERE node_id = ?1", [node_id])?;
        Ok(())
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
            (node_id, dump_addresses(addresses)),
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
        let now = now_ms() / 1000;
        let existing = self.get_peer(node_id)?;
        match existing {
            None => {
                self.conn.execute(
                    "INSERT INTO devices
                       (node_id, name, addresses, trusted, pending, last_seen)
                     VALUES (?1, ?2, ?3, 0, 1, ?4)",
                    (node_id, name, dump_addresses(addresses), now),
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
                "UPDATE devices SET trusted = 1, pending = 0, introducer = 1
                 WHERE node_id = ?1",
                [node_id],
            )?;
        } else {
            self.conn
                .execute("DELETE FROM devices WHERE node_id = ?1", [node_id])?;
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
        let mut added = 0;
        for g in gossip {
            if g.node_id == self_id {
                continue;
            }
            match self.get_peer(&g.node_id)? {
                Some(peer) => {
                    let merged = merge_addresses(&peer.addresses, &g.addresses);
                    self.conn.execute(
                        "UPDATE devices SET addresses = ?2, last_seen = MAX(last_seen, ?3)
                         WHERE node_id = ?1",
                        (g.node_id.clone(), dump_addresses(&merged), g.last_seen),
                    )?;
                    // Names ride along with gossip so renames propagate
                    // without waiting for a full sync round.
                    if !g.name.is_empty() && g.name != peer.name {
                        self.conn.execute(
                            "UPDATE devices SET name = ?2 WHERE node_id = ?1",
                            (g.node_id.clone(), g.name.clone()),
                        )?;
                    }
                }
                None if introducer => {
                    self.conn.execute(
                        "INSERT INTO devices
                           (node_id, name, addresses, trusted, pending, last_seen)
                         VALUES (?1, ?2, ?3, 1, 0, ?4)",
                        (
                            g.node_id.clone(),
                            g.name.clone(),
                            dump_addresses(&g.addresses),
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
    #[test]
    fn settings_engine_switch_roundtrip() {
        use crate::settings::{LocalSttProvider, SttMode};
        let store = Store::open_memory().unwrap();
        let mut s = store.load_settings();
        // Simulate the engine dropdown handler: scoped mutation,
        // then a separate persist read (never nested borrows).
        {
            s.stt.mode = SttMode::Local;
            s.stt.local_provider = LocalSttProvider::Nvidia;
        }
        store.save_settings(&s).unwrap();
        let reloaded = store.load_settings();
        assert_eq!(reloaded.stt.mode, SttMode::Local);
        assert_eq!(reloaded.stt.local_provider, LocalSttProvider::Nvidia);
        {
            s.stt.mode = SttMode::SelfHosted;
        }
        store.save_settings(&s).unwrap();
        assert_eq!(store.load_settings().stt.mode, SttMode::SelfHosted);
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
        let uuid = store
            .add_history("r", "t", 2, 5, "node-x")
            .unwrap();
        store.delete_history(&uuid).unwrap();
        // Ancient live rows go; tombstones stay (delete-sync safety).
        let n = store.purge_old_history(0).unwrap();
        assert_eq!(n, 0, "tombstone must survive auto-delete");
        store
            .add_history("r2", "t2", 2, 5, "node-x")
            .unwrap();
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
        let Ok(a) = Store::open(&dir) else {
            return;
        };
        let v0 = read_gen(&a);
        {
            let b = Store::open(&dir).unwrap();
            b.upsert_word("cross-conn", "node-y").unwrap();
        }
        let v1 = read_gen(&a);
        assert!(v1 != v0, "gen write must be visible cross-connection");
        assert!(a.load_words().unwrap().contains(&"cross-conn".to_string()));
        drop(a);
        let _ = std::fs::remove_file(&dir);
    }

    use super::*;

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

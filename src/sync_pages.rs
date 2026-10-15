//! Durable sequence cursors independent of wall clocks and record conflict versions.
//! Only the newest change for each key is retained. Deletion tombstones stay in
//! the content tables, so peers that were offline need no age-based cutoff.
use super::*;
use rusqlite::params;

const ROWS: usize = 32;
const PAGE_BYTES: usize = 16 * 1024 * 1024 - 512;
// Check SQLite byte lengths before loading a record; count escaped JSON with
// a bounded writer before admitting it to a page. Neither path scales with
// the aggregate history, or assumes that Unicode/escaping uses one byte.
const RECORD_TEXT_BYTES: i64 = PAGE_BYTES as i64 - 4096;

struct EncodedSize(usize);
impl std::io::Write for EncodedSize {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > PAGE_BYTES - 512 - self.0 {
            return Err(std::io::Error::other("individual sync record too large"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct SyncManifest {
    pub stream: String,
    pub upper: i64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct SyncPage {
    pub from: i64,
    pub to: i64,
    pub done: bool,
    pub content: ContentSnapshot,
}

#[derive(Debug)]
struct SegmentationRequired;
impl std::fmt::Display for SegmentationRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("individual record exceeds sync wire size; upgrade the peer for segmented transfer; local data preserved")
    }
}
impl std::error::Error for SegmentationRequired {}
pub(super) fn requires_segmentation(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::ToSqlConversionFailure(inner) if inner.is::<SegmentationRequired>())
}
fn segment_required() -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(SegmentationRequired))
}

pub(super) fn record_on(
    conn: &Connection,
    kind: i64,
    key: &str,
    self_id: &str,
    cap: i64,
) -> rusqlite::Result<ContentSnapshot> {
    let (table, column, lengths) = match kind {
        0 => (
            "words",
            "word",
            "length(CAST(word AS BLOB))+length(CAST(updated_by AS BLOB))",
        ),
        1 => (
            "snippets",
            "trigger",
            "length(CAST(trigger AS BLOB))+length(CAST(replacement AS BLOB))+length(CAST(updated_by AS BLOB))",
        ),
        2 => (
            "history",
            "uuid",
            "length(CAST(uuid AS BLOB))+length(CAST(text AS BLOB))+length(CAST(raw_text AS BLOB))",
        ),
        3 => (
            "devices",
            "node_id",
            "length(CAST(node_id AS BLOB))+length(CAST(name AS BLOB))+length(CAST(addresses AS BLOB))+length(CAST(address AS BLOB))",
        ),
        _ => return Err(invalid("invalid sync change kind")),
    };
    let length: Option<i64> = conn
        .query_row(
            &format!("SELECT {lengths} FROM {table} WHERE {column}=?1"),
            [key],
            |row| row.get(0),
        )
        .optional()?;
    if length.is_some_and(|length| length > cap) {
        return Err(if cap == RECORD_TEXT_BYTES {
            segment_required()
        } else {
            invalid("individual sync record exceeds the 64 MiB text budget; local data preserved")
        });
    }
    let mut content = ContentSnapshot::default();
    match kind {
        0=>if let Some(record)=conn.query_row("SELECT word,updated_at,updated_by,deleted FROM words WHERE word=?1",[key],|row|Ok(WordRecord {word:row.get(0)?,updated_at:row.get(1)?,updated_by:row.get(2)?,deleted:row.get(3)?})).optional()? {content.words.push(record);},
        1=>if let Some(record)=conn.query_row("SELECT trigger,replacement,updated_at,updated_by,deleted FROM snippets WHERE trigger=?1",[key],|row|Ok(SnippetRecord {trigger:row.get(0)?,replacement:row.get(1)?,updated_at:row.get(2)?,updated_by:row.get(3)?,deleted:row.get(4)?})).optional()? {content.snippets.push(record);},
        2=>if let Some(record)=conn.query_row("SELECT uuid,text,raw_text,words,duration_secs,deleted,discarded,recorded_at FROM history WHERE uuid=?1",[key],|row|Ok(HistoryRecord {uuid:row.get(0)?,text:row.get(1)?,raw_text:row.get(2)?,words:row.get(3)?,duration_secs:row.get(4)?,deleted:row.get(5)?,discarded:row.get(6)?,recorded_at:row.get(7)?})).optional()? {content.history.push(record);},
        3=>if key!=self_id && let Some(peer)=Store::peer_on(conn,key)?.filter(|peer|peer.trusted) {content.peers.push(PeerGossip {node_id:peer.node_id,name:peer.name,addresses:peer.addresses,last_seen:peer.last_seen});},
        _=>unreachable!(),
    }
    Ok(content)
}

fn invalid(message: &'static str) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(message.into())
}

pub(super) fn initialize(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sync_changes (
             seq INTEGER PRIMARY KEY AUTOINCREMENT,
             kind INTEGER NOT NULL, record_key TEXT NOT NULL,
             UNIQUE(kind,record_key));
         CREATE TABLE IF NOT EXISTS sync_received (
             peer TEXT PRIMARY KEY, stream TEXT NOT NULL, cursor INTEGER NOT NULL);
         INSERT OR IGNORE INTO sync_meta(key,value) VALUES ('stream_id',lower(hex(randomblob(16))));
         CREATE TRIGGER IF NOT EXISTS sync_reset_cursors AFTER DELETE ON kv
             WHEN OLD.key='settings' BEGIN DELETE FROM sync_received; END;
         CREATE TRIGGER IF NOT EXISTS sync_remove_cursor AFTER DELETE ON devices
             BEGIN DELETE FROM sync_received WHERE peer=OLD.node_id; END;",
    )?;
    for (kind, table, key, changed) in [
        (
            0,
            "words",
            "word",
            "NEW.updated_at<>OLD.updated_at OR NEW.updated_by<>OLD.updated_by OR NEW.deleted<>OLD.deleted",
        ),
        (
            1,
            "snippets",
            "trigger",
            "NEW.replacement<>OLD.replacement OR NEW.updated_at<>OLD.updated_at OR NEW.updated_by<>OLD.updated_by OR NEW.deleted<>OLD.deleted",
        ),
        (
            2,
            "history",
            "uuid",
            "NEW.text<>OLD.text OR NEW.raw_text<>OLD.raw_text OR NEW.words<>OLD.words OR NEW.duration_secs<>OLD.duration_secs OR NEW.deleted<>OLD.deleted OR NEW.discarded<>OLD.discarded OR NEW.recorded_at<>OLD.recorded_at",
        ),
        (
            3,
            "devices",
            "node_id",
            "NEW.name<>OLD.name OR NEW.address<>OLD.address OR NEW.addresses<>OLD.addresses OR NEW.trusted<>OLD.trusted OR NEW.introducer<>OLD.introducer",
        ),
    ] {
        // These identifiers are compile-time constants, never received SQL.
        conn.execute_batch(&format!(
            "INSERT OR IGNORE INTO sync_changes(kind,record_key) SELECT {kind},{key} FROM {table};
             CREATE TRIGGER IF NOT EXISTS sync_{table}_insert AFTER INSERT ON {table}
             BEGIN INSERT INTO sync_changes(kind,record_key) VALUES ({kind},NEW.{key}) ON CONFLICT(kind,record_key) DO UPDATE SET seq=excluded.seq; END;
             CREATE TRIGGER IF NOT EXISTS sync_{table}_update AFTER UPDATE ON {table}
             WHEN {changed}
             BEGIN INSERT INTO sync_changes(kind,record_key) VALUES ({kind},NEW.{key}) ON CONFLICT(kind,record_key) DO UPDATE SET seq=excluded.seq; END;
             CREATE TRIGGER IF NOT EXISTS sync_{table}_delete AFTER DELETE ON {table}
             BEGIN DELETE FROM sync_changes WHERE kind={kind} AND record_key=OLD.{key}; END;"
        ))?;
    }
    Ok(())
}

impl Store {
    pub(crate) fn check_sync_identity(&self) -> rusqlite::Result<()> {
        use std::os::unix::fs::MetadataExt;
        if let Some(identity) = self.file_identity {
            let metadata = self
                .path()
                .and_then(|path| std::fs::symlink_metadata(path).ok());
            if !metadata.is_some_and(|meta| meta.is_file() && (meta.dev(), meta.ino()) == identity)
            {
                return Err(invalid("sync database removed or replaced"));
            }
        }
        Ok(())
    }

    pub(crate) fn sync_manifest(&self, peer: &str, epoch: i64) -> rusqlite::Result<SyncManifest> {
        self.check_sync_identity()?;
        let tx = self.conn.unchecked_transaction()?;
        Self::require_sync_authorized(&tx, peer, epoch)?;
        let manifest = SyncManifest {
            stream: tx.query_row(
                "SELECT value FROM sync_meta WHERE key='stream_id'",
                [],
                |row| row.get(0),
            )?,
            upper: tx.query_row(
                "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='sync_changes'),0)",
                [],
                |row| row.get(0),
            )?,
        };
        tx.commit()?;
        Ok(manifest)
    }

    pub(crate) fn sync_received_cursor(
        &self,
        peer: &str,
        epoch: i64,
        manifest: &SyncManifest,
    ) -> rusqlite::Result<i64> {
        if manifest.stream.len() != 32
            || !manifest.stream.bytes().all(|b| b.is_ascii_hexdigit())
            || manifest.upper < 0
        {
            return Err(invalid("invalid sync manifest"));
        }
        self.check_sync_identity()?;
        let tx = self.conn.unchecked_transaction()?;
        Self::require_sync_authorized(&tx, peer, epoch)?;
        let cursor = tx
            .query_row(
                "SELECT cursor FROM sync_received WHERE peer=?1 AND stream=?2",
                params![peer, manifest.stream],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        if cursor < 0 || cursor > manifest.upper {
            return Err(invalid(
                "sync sequence rolled back; re-pair restored database",
            ));
        }
        tx.commit()?;
        Ok(cursor)
    }

    pub(crate) fn export_sync_page(
        &self,
        peer: &str,
        epoch: i64,
        self_id: &str,
        from: i64,
        upper: i64,
    ) -> rusqlite::Result<SyncPage> {
        if from < 0 || from > upper {
            return Err(invalid("invalid sync page cursor"));
        }
        self.check_sync_identity()?;
        let tx = self.conn.unchecked_transaction()?;
        Self::require_sync_authorized(&tx, peer, epoch)?;
        let mut statement = tx.prepare("SELECT seq,kind,length(CAST(record_key AS BLOB)) FROM sync_changes WHERE seq>?1 AND seq<=?2 ORDER BY seq LIMIT ?3")?;
        let changes = statement
            .query_map(params![from, upper, ROWS as i64 + 1], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut content = ContentSnapshot::default();
        let mut to = from;
        let mut bytes = 512;
        for (seq, kind, key_length) in changes.iter().take(ROWS) {
            if *key_length > RECORD_TEXT_BYTES {
                return Err(invalid(
                    "individual sync key too large; local data preserved",
                ));
            }
            let key: String = tx.query_row(
                "SELECT record_key FROM sync_changes WHERE seq=?1",
                [seq],
                |row| row.get(0),
            )?;
            let key = &key;
            let row_content = record_on(&tx, *kind, key, self_id, RECORD_TEXT_BYTES)?;
            let mut encoded = EncodedSize(0);
            serde_json::to_writer(&mut encoded, &row_content).map_err(|_| segment_required())?;
            let row_bytes = encoded.0;
            if bytes + row_bytes > PAGE_BYTES {
                break;
            }
            bytes += row_bytes;
            content.words.extend(row_content.words);
            content.snippets.extend(row_content.snippets);
            content.history.extend(row_content.history);
            content.peers.extend(row_content.peers);
            to = *seq;
        }
        let done = changes.last().is_none_or(|(seq, _, _)| *seq <= to);
        if done {
            to = upper;
        }
        tx.commit()?;
        Ok(SyncPage {
            from,
            to,
            done,
            content,
        })
    }

    pub(crate) fn apply_sync_page(
        &self,
        peer: &str,
        epoch: i64,
        self_id: &str,
        manifest: &SyncManifest,
        page: &SyncPage,
    ) -> rusqlite::Result<usize> {
        self.check_sync_identity()?;
        let tx = self.conn.unchecked_transaction()?;
        let (added, changed) = Self::merge_sync_page_on(&tx, peer, epoch, self_id, manifest, page)?;
        tx.commit()?;
        if changed {
            bump_data_gen();
        }
        self.prune_audio_best_effort();
        Ok(added)
    }

    pub(super) fn merge_sync_page_on(
        tx: &Connection,
        peer: &str,
        epoch: i64,
        self_id: &str,
        manifest: &SyncManifest,
        page: &SyncPage,
    ) -> rusqlite::Result<(usize, bool)> {
        let content = &page.content;
        if content.words.len()
            + content.snippets.len()
            + content.history.len()
            + content.peers.len()
            > ROWS
        {
            return Err(invalid("too many records in sync page"));
        }
        if page.from < 0
            || page.to < page.from
            || page.to > manifest.upper
            || (page.done && page.to != manifest.upper)
            || (!page.done && page.to == page.from)
        {
            return Err(invalid("invalid sync page progress"));
        }
        Self::require_sync_authorized(tx, peer, epoch)?;
        let current = tx
            .query_row(
                "SELECT cursor FROM sync_received WHERE peer=?1 AND stream=?2",
                params![peer, manifest.stream],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        if current != page.from {
            return Err(invalid("concurrent sync page advanced cursor; retry"));
        }
        let changed = Self::apply_words_on(tx, &content.words)?
            + Self::apply_snippets_on(tx, &content.snippets)?
            + Self::apply_history_on(tx, &content.history)?;
        let introducer = Self::peer_on(tx, peer)?.is_some_and(|peer| peer.introducer);
        let added = Self::apply_gossip_on(tx, &content.peers, introducer, self_id)?;
        tx.execute("INSERT INTO sync_received(peer,stream,cursor) VALUES (?1,?2,?3) ON CONFLICT(peer) DO UPDATE SET stream=excluded.stream,cursor=excluded.cursor", params![peer,manifest.stream,page.to])?;
        tx.execute(
            "UPDATE devices SET last_sync=?2 WHERE node_id=?1",
            (peer, now_ms() / 1000),
        )?;
        if changed > 0 || added > 0 {
            bump_gen(tx)?;
        }
        tx.execute(
            "DELETE FROM sync_transfers WHERE peer=?1 AND direction=1",
            [peer],
        )?;
        Ok((added, changed > 0 || added > 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stores() -> (Store, Store, String, String) {
        let a = crate::device::DeviceIdentity::ephemeral().creds().node_id;
        let b = crate::device::DeviceIdentity::ephemeral().creds().node_id;
        let first = Store::open_memory().unwrap();
        let second = Store::open_memory().unwrap();
        for store in [&first, &second] {
            let mut settings = store.load_settings();
            settings.sync.enabled = true;
            store.save_settings(&settings).unwrap();
        }
        first.upsert_peer(&b, "synthetic b", "", true).unwrap();
        second.upsert_peer(&a, "synthetic a", "", true).unwrap();
        (first, second, a, b)
    }

    #[test]
    fn page_and_cursor_rollback_together_and_parallel_rounds_cannot_skip_records() {
        let (a, b, a_id, b_id) = stores();
        a.upsert_word("synthetic original", &a_id).unwrap();
        let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let mut page = a
            .export_sync_page(&b_id, a.sync_epoch(), &a_id, 0, manifest.upper)
            .unwrap();
        page.content.snippets.push(SnippetRecord {
            trigger: "synthetic malformed".into(),
            replacement: "private".into(),
            updated_at: now_ms() + 8 * 86_400_000,
            updated_by: a_id.clone(),
            deleted: false,
        });
        assert!(
            b.apply_sync_page(&a_id, b.sync_epoch(), &b_id, &manifest, &page)
                .is_err()
        );
        assert!(b.load_words().unwrap().is_empty());
        assert_eq!(
            b.sync_received_cursor(&a_id, b.sync_epoch(), &manifest)
                .unwrap(),
            0
        );
        page.content.snippets.clear();
        b.apply_sync_page(&a_id, b.sync_epoch(), &b_id, &manifest, &page)
            .unwrap();
        assert_eq!(b.load_words().unwrap(), ["synthetic original"]);
        assert!(
            b.apply_sync_page(&a_id, b.sync_epoch(), &b_id, &manifest, &page)
                .is_err()
        );
        assert_eq!(
            b.sync_received_cursor(&a_id, b.sync_epoch(), &manifest)
                .unwrap(),
            page.to
        );
    }

    #[test]
    fn durable_sequences_include_edits_before_cursor_and_ignore_clock_order() {
        let (a, b, a_id, b_id) = stores();
        for n in 0..65 {
            a.upsert_word(&format!("synthetic {n:03}"), &a_id).unwrap();
        }
        let snapshot = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let first = a
            .export_sync_page(&b_id, a.sync_epoch(), &a_id, 0, snapshot.upper)
            .unwrap();
        assert!(!first.done);
        assert!(first.content.words.len() <= ROWS);
        b.apply_sync_page(&a_id, b.sync_epoch(), &b_id, &snapshot, &first)
            .unwrap();
        let received_key = first.content.words[0].word.clone();
        a.delete_word(&received_key, &a_id).unwrap();
        a.upsert_word("synthetic newly inserted earlier key", &a_id)
            .unwrap();
        let mut cursor = first.to;
        loop {
            let page = a
                .export_sync_page(&b_id, a.sync_epoch(), &a_id, cursor, snapshot.upper)
                .unwrap();
            b.apply_sync_page(&a_id, b.sync_epoch(), &b_id, &snapshot, &page)
                .unwrap();
            cursor = page.to;
            if page.done {
                break;
            }
        }
        assert!(
            !b.load_words()
                .unwrap()
                .iter()
                .any(|word| word == "synthetic newly inserted earlier key")
        );
        let next = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let delta = a
            .export_sync_page(&b_id, a.sync_epoch(), &a_id, cursor, next.upper)
            .unwrap();
        assert_eq!(delta.content.words.len(), 2);
        assert!(
            delta
                .content
                .words
                .iter()
                .any(|word| word.word == received_key && word.deleted)
        );
        b.apply_sync_page(&a_id, b.sync_epoch(), &b_id, &next, &delta)
            .unwrap();
        assert_eq!(a.export_words().unwrap().len(), 66);
        assert_eq!(a.load_words().unwrap(), b.load_words().unwrap());
        let unchanged = a
            .export_sync_page(&b_id, a.sync_epoch(), &a_id, delta.to, next.upper)
            .unwrap();
        assert!(unchanged.done && unchanged.content.words.is_empty());
        // Wall clock conflict versions and insertion order can move backwards;
        // replication progress uses only SQLite's durable sequence.
        a.apply_words(&[WordRecord {
            word: "synthetic clock anomaly".into(),
            updated_at: 0,
            updated_by: a_id.clone(),
            deleted: false,
        }])
        .unwrap();
        let next = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let page = a
            .export_sync_page(&b_id, a.sync_epoch(), &a_id, delta.to, next.upper)
            .unwrap();
        assert_eq!(page.content.words[0].word, "synthetic clock anomaly");
        assert!(page.to > delta.to);
    }

    #[test]
    fn pages_recheck_disable_reset_and_revocation_and_validate_progress() {
        for action in 0..3 {
            let (a, b, a_id, b_id) = stores();
            a.upsert_word("synthetic never accepted", &a_id).unwrap();
            let snapshot = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
            let page = a
                .export_sync_page(&b_id, a.sync_epoch(), &a_id, 0, snapshot.upper)
                .unwrap();
            let epoch = b.sync_epoch();
            match action {
                0 => {
                    let mut settings = b.load_settings();
                    settings.sync.enabled = false;
                    b.save_settings(&settings).unwrap();
                }
                1 => b.reset_all_data().unwrap(),
                _ => b.remove_peer(&a_id).unwrap(),
            }
            assert!(
                b.apply_sync_page(&a_id, epoch, &b_id, &snapshot, &page)
                    .is_err()
            );
            assert!(b.load_words().unwrap().is_empty());
        }
        let (a, b, a_id, b_id) = stores();
        a.upsert_word("synthetic never skipped", &a_id).unwrap();
        let snapshot = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let mut page = a
            .export_sync_page(&b_id, a.sync_epoch(), &a_id, 0, snapshot.upper)
            .unwrap();
        page.to = snapshot.upper + 1;
        assert!(
            b.apply_sync_page(&a_id, b.sync_epoch(), &b_id, &snapshot, &page)
                .is_err()
        );
        page.to = 0;
        page.done = false;
        assert!(
            b.apply_sync_page(&a_id, b.sync_epoch(), &b_id, &snapshot, &page)
                .is_err()
        );
        assert!(b.load_words().unwrap().is_empty());
    }

    #[test]
    fn latest_change_ledger_is_bounded_by_keys_and_removed_peer_does_not_roll_back_sequence() {
        let (a, _, a_id, b_id) = stores();
        for _ in 0..100 {
            a.upsert_word("synthetic repeatedly edited", &a_id).unwrap();
        }
        let rows: i64 = a
            .conn
            .query_row(
                "SELECT count(*) FROM sync_changes WHERE kind=0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);
        let upper = a.sync_manifest(&b_id, a.sync_epoch()).unwrap().upper;
        let extra = crate::device::DeviceIdentity::ephemeral().creds().node_id;
        a.upsert_peer(&extra, "synthetic removable", "", true)
            .unwrap();
        a.remove_peer(&extra).unwrap();
        let next = a.sync_manifest(&b_id, a.sync_epoch()).unwrap().upper;
        assert!(next > upper);
        let page = a
            .export_sync_page(&b_id, a.sync_epoch(), &a_id, upper, next)
            .unwrap();
        assert!(page.done && page.to == next && page.content.peers.is_empty());
    }

    #[test]
    fn large_individual_unicode_record_is_preserved_and_escaping_is_bounded() {
        let (a, b, a_id, b_id) = stores();
        let record = HistoryRecord {
            uuid: "synthetic unicode boundary".into(),
            text: "🦀".repeat(768 * 1024),
            raw_text: "é".repeat(512 * 1024),
            words: 1,
            duration_secs: 1,
            recorded_at: 123,
            deleted: false,
            discarded: false,
        };
        a.apply_history(std::slice::from_ref(&record)).unwrap();
        let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let page = a
            .export_sync_page(&b_id, a.sync_epoch(), &a_id, 0, manifest.upper)
            .unwrap();
        assert!(page.done);
        b.apply_sync_page(&a_id, b.sync_epoch(), &b_id, &manifest, &page)
            .unwrap();
        assert_eq!(b.export_history().unwrap(), [record]);
        let previous = page.to;
        let too_large = HistoryRecord {
            uuid: "synthetic escaped input".into(),
            text: "\0".repeat(3 * 1024 * 1024),
            raw_text: String::new(),
            words: 1,
            duration_secs: 1,
            recorded_at: 123,
            deleted: false,
            discarded: false,
        };
        a.apply_history(std::slice::from_ref(&too_large)).unwrap();
        let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        assert!(
            a.export_sync_page(&b_id, a.sync_epoch(), &a_id, previous, manifest.upper)
                .unwrap_err()
                .to_string()
                .contains("wire size")
        );
        assert!(
            a.export_history().unwrap().contains(&too_large),
            "oversized data must remain available locally"
        );
    }
}

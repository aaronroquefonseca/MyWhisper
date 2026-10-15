//! Resumable individual records. Staging stays inside the encrypted database;
//! only complete, authenticated records enter the ordinary conflict merge.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

const PART: usize = 64 * 1024;
// Application providers already bound each transcript to 1 MiB. Imported data
// can be larger. These explicit per-record/aggregate staging budgets prevent a
// trusted or malformed peer from allocating unbounded memory/disk. Rejected
// records remain local; the message states the limitation instead of dropping
// text. The existing frame, worker and round limits are unchanged.
const TEXT: usize = 64 * 1024 * 1024;
const WIRE: usize = TEXT * 6 + 4096;
const STAGING: i64 = 512 * 1024 * 1024;
const TRANSFERS: i64 = 16;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PartialCursor {
    seq: i64,
    digest: String,
    offset: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SyncCursor {
    pub committed: i64,
    partial: Option<PartialCursor>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Fragment {
    from: i64,
    seq: i64,
    kind: i64,
    key: String,
    digest: String,
    total: usize,
    offset: usize,
    bytes: String,
    done: bool,
    recorded_at: Option<i64>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum SyncPacket {
    Page(SyncPage),
    Fragment(Fragment),
}
impl SyncPacket {
    pub fn from(&self) -> i64 {
        match self {
            Self::Page(page) => page.from,
            Self::Fragment(part) => part.from,
        }
    }

    pub fn next_cursor(&self) -> SyncCursor {
        match self {
            Self::Page(page) => SyncCursor {
                committed: page.to,
                partial: None,
            },
            Self::Fragment(part) => {
                let offset = part.offset + part.bytes.len() / 4 * 3;
                // Padding is removed without allocating a second payload.
                let padding = part
                    .bytes
                    .as_bytes()
                    .iter()
                    .rev()
                    .take_while(|byte| **byte == b'=')
                    .count();
                let offset = offset - padding;
                if offset == part.total {
                    SyncCursor {
                        committed: part.seq,
                        partial: None,
                    }
                } else {
                    SyncCursor {
                        committed: part.from,
                        partial: Some(PartialCursor {
                            seq: part.seq,
                            digest: part.digest.clone(),
                            offset,
                        }),
                    }
                }
            }
        }
    }
    pub fn accepts(&self, cursor: &SyncCursor) -> bool {
        *cursor == self.next_cursor()
            || matches!(self,Self::Fragment(part) if cursor.committed==part.seq && cursor.partial.is_none())
    }
    pub fn done(&self) -> bool {
        match self {
            Self::Page(page) => page.done,
            Self::Fragment(part) => part.done,
        }
    }
}
pub(crate) struct PacketResult {
    pub cursor: SyncCursor,
    pub done: bool,
    pub introduced: usize,
    pub words: usize,
    pub snippets: usize,
    pub history: usize,
}

pub(super) fn initialize(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS sync_transfers (
        id INTEGER PRIMARY KEY, peer TEXT NOT NULL REFERENCES devices(node_id) ON DELETE CASCADE,
        direction INTEGER NOT NULL CHECK(direction IN (0,1)), stream TEXT NOT NULL,
        base INTEGER NOT NULL, seq INTEGER NOT NULL, kind INTEGER NOT NULL,
        record_key TEXT NOT NULL, digest TEXT NOT NULL, total INTEGER NOT NULL,
        progress INTEGER NOT NULL, recorded_at INTEGER, updated_at INTEGER NOT NULL, UNIQUE(peer,direction));
        CREATE TABLE IF NOT EXISTS sync_transfer_parts (
        transfer INTEGER NOT NULL REFERENCES sync_transfers(id) ON DELETE CASCADE,
        position INTEGER NOT NULL, data BLOB NOT NULL, PRIMARY KEY(transfer,position));
        CREATE TRIGGER IF NOT EXISTS sync_drop_transfers_settings_update AFTER UPDATE ON kv WHEN NEW.key='settings'
        BEGIN DELETE FROM sync_transfers; END;
        CREATE TRIGGER IF NOT EXISTS sync_drop_transfers_settings_delete AFTER DELETE ON kv WHEN OLD.key='settings'
        BEGIN DELETE FROM sync_transfers; END;
        CREATE TRIGGER IF NOT EXISTS sync_drop_transfers_policy AFTER UPDATE ON sync_meta WHEN NEW.key='policy_epoch'
        BEGIN DELETE FROM sync_transfers; END;
        CREATE TRIGGER IF NOT EXISTS sync_drop_transfers_revoked AFTER INSERT ON revoked_devices
        BEGIN DELETE FROM sync_transfers WHERE peer=NEW.node_id; END;
        CREATE TRIGGER IF NOT EXISTS sync_drop_transfers_untrusted AFTER UPDATE ON devices WHEN NEW.trusted=0
        BEGIN DELETE FROM sync_transfers WHERE peer=NEW.node_id; END;")?;
    for (kind, table, key) in [
        (0, "words", "word"),
        (1, "snippets", "trigger"),
        (2, "history", "uuid"),
        (3, "devices", "node_id"),
    ] {
        conn.execute_batch(&format!(
            "CREATE TRIGGER IF NOT EXISTS sync_retire_parts_{table}_update AFTER UPDATE ON {table}
            BEGIN DELETE FROM sync_transfers WHERE kind={kind} AND record_key=NEW.{key}; END;
            CREATE TRIGGER IF NOT EXISTS sync_retire_parts_{table}_delete AFTER DELETE ON {table}
            BEGIN DELETE FROM sync_transfers WHERE kind={kind} AND record_key=OLD.{key}; END;"
        ))?;
    }
    Ok(())
}
fn invalid(message: &'static str) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(message.into())
}
fn size_budget(conn: &Connection, additional: usize) -> rusqlite::Result<()> {
    let bytes: i64 = conn.query_row(
        "SELECT COALESCE(SUM(length(data)),0) FROM sync_transfer_parts",
        [],
        |row| row.get(0),
    )?;
    if bytes > STAGING - additional as i64 {
        return Err(invalid(
            "sync staging budget reached; retry after active transfers finish; local data preserved",
        ));
    }
    Ok(())
}
fn new_transfer(
    conn: &Connection,
    peer: &str,
    direction: i64,
    manifest: &SyncManifest,
    part: &Fragment,
) -> rusqlite::Result<i64> {
    conn.execute(
        "DELETE FROM sync_transfers WHERE peer=?1 AND direction=?2",
        params![peer, direction],
    )?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM sync_transfers", [], |row| row.get(0))?;
    if count >= TRANSFERS {
        return Err(invalid(
            "sync transfer admission budget reached; retry; local data preserved",
        ));
    }
    conn.execute("INSERT INTO sync_transfers(peer,direction,stream,base,seq,kind,record_key,digest,total,progress,recorded_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10,?11)", params![peer,direction,manifest.stream,part.from,part.seq,part.kind,part.key,part.digest,part.total as i64,part.recorded_at,now_ms()/1000])?;
    Ok(conn.last_insert_rowid())
}

struct ReceivedTransfer {
    id: i64,
    seq: i64,
    digest: String,
    base: i64,
    kind: i64,
    key: String,
    total: usize,
    progress: usize,
    recorded_at: Option<i64>,
}

struct ChunkWriter<'a> {
    conn: &'a Connection,
    id: i64,
    buffer: Vec<u8>,
    offset: usize,
    hash: Sha256,
}
impl ChunkWriter<'_> {
    fn persist(&mut self) -> std::io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        size_budget(self.conn, self.buffer.len()).map_err(std::io::Error::other)?;
        self.conn
            .execute(
                "INSERT INTO sync_transfer_parts(transfer,position,data) VALUES (?1,?2,?3)",
                params![self.id, self.offset as i64, self.buffer],
            )
            .map_err(std::io::Error::other)?;
        self.offset += self.buffer.len();
        self.buffer.clear();
        Ok(())
    }
}
impl Write for ChunkWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > WIRE.saturating_sub(self.offset + self.buffer.len()) {
            return Err(std::io::Error::other(
                "sync record exceeds the 64 MiB text/encoded staging budget; local data preserved",
            ));
        }
        self.hash.update(bytes);
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = rest.len().min(PART - self.buffer.len());
            self.buffer.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if self.buffer.len() == PART {
                self.persist()?;
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.persist()
    }
}

/// SQL-backed reader with one chunk of scratch memory. Count decoded string
/// bytes while reading, before serde can grow its scratch string past the
/// admitted text budget. This includes ignored JSON fields and escape forms.
struct ChunkReader<'a> {
    conn: &'a Connection,
    id: i64,
    buffer: Vec<u8>,
    at: usize,
    offset: usize,
    total: usize,
    quoted: bool,
    escaped: bool,
    unicode: u8,
    code: u16,
    text: usize,
}
impl<'a> ChunkReader<'a> {
    fn new(conn: &'a Connection, id: i64, total: usize) -> Self {
        Self {
            conn,
            id,
            buffer: Vec::new(),
            at: 0,
            offset: 0,
            total,
            quoted: false,
            escaped: false,
            unicode: 0,
            code: 0,
            text: 0,
        }
    }
    fn count(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        for &byte in bytes {
            if self.unicode > 0 {
                self.code = self.code.checked_mul(16).unwrap_or(0)
                    | match byte {
                        b'0'..=b'9' => u16::from(byte - b'0'),
                        b'a'..=b'f' => u16::from(byte - b'a' + 10),
                        b'A'..=b'F' => u16::from(byte - b'A' + 10),
                        _ => 0,
                    };
                self.unicode -= 1;
                if self.unicode == 0 {
                    self.text += match self.code {
                        0..=0x7f => 1,
                        0x80..=0x7ff => 2,
                        0xdc00..=0xdfff => 1,
                        _ => 3,
                    };
                }
            } else if self.escaped {
                self.escaped = false;
                if byte == b'u' {
                    self.unicode = 4;
                    self.code = 0;
                } else {
                    self.text += 1;
                }
            } else if self.quoted {
                match byte {
                    b'"' => self.quoted = false,
                    b'\\' => self.escaped = true,
                    _ => self.text += 1,
                }
            } else if byte == b'"' {
                self.quoted = true;
            }
            if self.text > TEXT + 4096 {
                return Err(std::io::Error::other(
                    "sync record exceeds decoded string budget; local data preserved",
                ));
            }
        }
        Ok(())
    }
}
impl Read for ChunkReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() || self.offset == self.total {
            return Ok(0);
        }
        if self.at == self.buffer.len() {
            self.buffer = self
                .conn
                .query_row(
                    "SELECT data FROM sync_transfer_parts WHERE transfer=?1 AND position=?2",
                    params![self.id, self.offset as i64],
                    |row| row.get(0),
                )
                .map_err(std::io::Error::other)?;
            self.at = 0;
            if self.buffer.is_empty()
                || self.buffer.len() > PART
                || self.buffer.len() > self.total - self.offset
            {
                return Err(std::io::Error::other("invalid staged fragment"));
            }
        }
        let n = out.len().min(self.buffer.len() - self.at);
        out[..n].copy_from_slice(&self.buffer[self.at..self.at + n]);
        self.count(&out[..n])?;
        self.at += n;
        self.offset += n;
        Ok(n)
    }
}

#[derive(serde::Deserialize)]
struct OneRecord {
    #[serde(default, deserialize_with = "one")]
    words: Vec<WordRecord>,
    #[serde(default, deserialize_with = "one")]
    snippets: Vec<SnippetRecord>,
    #[serde(default, deserialize_with = "one")]
    history: Vec<HistoryRecord>,
    #[serde(default, deserialize_with = "one")]
    peers: Vec<PeerGossip>,
}
fn one<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    struct Single<T>(std::marker::PhantomData<T>);
    impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for Single<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most one segmented record")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Vec<T>, A::Error> {
            let first = sequence.next_element::<T>()?;
            if sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
                return Err(serde::de::Error::custom("multiple segmented records"));
            }
            Ok(first.into_iter().collect())
        }
    }
    deserializer.deserialize_seq(Single(std::marker::PhantomData))
}

impl Store {
    pub(super) fn expire_sync_transfers_at(&self, now: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM sync_transfers WHERE updated_at>?1 OR updated_at<?2",
            params![now, now.saturating_sub(3600)],
        )?;
        // Retire orphaned parts left by an interrupted delete or a build
        // without FK enforcement: parent cascade is primary, this is repair.
        self.conn.execute(
            "DELETE FROM sync_transfer_parts WHERE transfer NOT IN (SELECT id FROM sync_transfers)",
            [],
        )?;
        Ok(())
    }
    pub(super) fn expire_pending_history_at(&self, now: i64, days: u32) -> rusqlite::Result<usize> {
        if days == 0 {
            return Ok(0);
        }
        let cutoff = now.saturating_sub(i64::from(days) * 86400);
        let tx = self.conn.unchecked_transaction()?;
        let changed=tx.execute("INSERT OR IGNORE INTO history(uuid,text,raw_text,words,duration_secs,deleted,discarded,recorded_at) SELECT record_key,'','',0,0,1,0,recorded_at FROM sync_transfers WHERE direction=1 AND kind=2 AND recorded_at<?1",[cutoff])?;
        tx.execute(
            "DELETE FROM sync_transfers WHERE direction=1 AND kind=2 AND recorded_at<?1",
            [cutoff],
        )?;
        if changed > 0 {
            Self::invalidate_sync(&tx)?;
            bump_gen(&tx)?;
        }
        tx.commit()?;
        if changed > 0 {
            bump_data_gen();
        }
        Ok(changed)
    }
    pub(crate) fn sync_packet_cursor(
        &self,
        peer: &str,
        epoch: i64,
        manifest: &SyncManifest,
    ) -> rusqlite::Result<SyncCursor> {
        if let Some(privacy) = self.retention_policy()? {
            self.expire_pending_history_at(now_ms() / 1000, privacy.transcript_auto_delete_days)?;
        }
        self.expire_sync_transfers_at(now_ms() / 1000)?;
        let committed = self.sync_received_cursor(peer, epoch, manifest)?;
        let partial=self.conn.query_row("SELECT seq,digest,progress FROM sync_transfers WHERE peer=?1 AND direction=1 AND stream=?2 AND base=?3",params![peer,manifest.stream,committed],|row| Ok(PartialCursor { seq:row.get(0)?,digest:row.get(1)?,offset:row.get::<_,i64>(2)? as usize })).optional()?;
        if partial
            .as_ref()
            .is_some_and(|p| p.seq <= committed || p.seq > manifest.upper || p.offset > WIRE)
        {
            return Err(invalid(
                "invalid staged sync cursor; peer sequence rolled back",
            ));
        }
        Ok(SyncCursor { committed, partial })
    }
    pub(crate) fn export_sync_packet(
        &self,
        peer: &str,
        epoch: i64,
        self_id: &str,
        manifest: &SyncManifest,
        cursor: &SyncCursor,
    ) -> rusqlite::Result<SyncPacket> {
        self.check_sync_identity()?;
        let privacy = self.retention_policy()?;
        let expired = |recorded_at: Option<i64>| {
            recorded_at.is_some_and(|time| {
                privacy.as_ref().is_some_and(|privacy| {
                    privacy.transcript_auto_delete_days > 0
                        && time
                            < (now_ms() / 1000).saturating_sub(
                                i64::from(privacy.transcript_auto_delete_days) * 86400,
                            )
                })
            })
        };
        if cursor.committed < 0 || cursor.committed > manifest.upper {
            return Err(invalid("invalid packet cursor"));
        }
        let cached=self.conn.query_row("SELECT id,seq,kind,record_key,digest,total,recorded_at FROM sync_transfers WHERE peer=?1 AND direction=0 AND stream=?2 AND base=?3",params![peer,manifest.stream,cursor.committed],|row| Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,i64>(5)? as usize,row.get::<_,Option<i64>>(6)?))).optional()?;
        let cached = if let Some(value) = cached {
            value
        } else {
            match self.export_sync_page(peer, epoch, self_id, cursor.committed, manifest.upper) {
                Ok(page) => return Ok(SyncPacket::Page(page)),
                Err(error) if pages::requires_segmentation(&error) => {}
                Err(error) => return Err(error),
            }
            let tx = self.conn.unchecked_transaction()?;
            Self::require_sync_authorized(&tx, peer, epoch)?;
            let mut from = cursor.committed;
            let (seq, kind, key, content) = loop {
                let item:Option<(i64,i64,String)>=tx.query_row("SELECT seq,kind,record_key FROM sync_changes WHERE seq>?1 AND seq<=?2 ORDER BY seq LIMIT 1",params![from,manifest.upper],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
                let Some((seq, kind, key)) = item else {
                    return Ok(SyncPacket::Page(SyncPage {
                        from: cursor.committed,
                        to: manifest.upper,
                        done: true,
                        content: ContentSnapshot::default(),
                    }));
                };
                if key.len() > 1024 {
                    return Err(invalid(
                        "segmented sync record key exceeds 1024 bytes; local data preserved",
                    ));
                }
                let content = pages::record_on(&tx, kind, &key, self_id, TEXT as i64)?;
                if content.words.len()
                    + content.snippets.len()
                    + content.history.len()
                    + content.peers.len()
                    > 0
                {
                    break (seq, kind, key, content);
                }
                from = seq;
            };
            if expired(content.history.first().map(|record| record.recorded_at)) {
                drop(tx);
                self.delete_history(&key)?;
                return Err(invalid(
                    "history expired before segmented export; retry authorized synchronization",
                ));
            }
            let header = Fragment {
                from: cursor.committed,
                seq,
                kind,
                key: key.clone(),
                digest: String::new(),
                total: 0,
                offset: 0,
                bytes: String::new(),
                done: false,
                recorded_at: content.history.first().map(|record| record.recorded_at),
            };
            let id = new_transfer(&tx, peer, 0, manifest, &header)?;
            let mut writer = ChunkWriter {
                conn: &tx,
                id,
                buffer: Vec::with_capacity(PART),
                offset: 0,
                hash: Sha256::new(),
            };
            serde_json::to_writer(&mut writer, &content).map_err(|_| {
                invalid("cannot stage sync record within encoded budget; local data preserved")
            })?;
            writer.flush().map_err(|_| {
                invalid("sync staging budget or storage failure; local data preserved")
            })?;
            let total = writer.offset;
            let digest = writer
                .hash
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            tx.execute(
                "UPDATE sync_transfers SET digest=?2,total=?3 WHERE id=?1",
                params![id, digest, total as i64],
            )?;
            tx.commit()?;
            (id, seq, kind, key, digest, total, header.recorded_at)
        };
        let (id, seq, kind, key, digest, total, recorded_at) = cached;
        if expired(recorded_at) {
            self.delete_history(&key)?;
            return Err(invalid(
                "history expired during segmented export; retry authorized synchronization",
            ));
        }
        let offset = cursor
            .partial
            .as_ref()
            .filter(|p| p.seq == seq && p.digest == digest)
            .map_or(0, |p| p.offset);
        if offset >= total || !offset.is_multiple_of(PART) {
            return Err(invalid("invalid partial receive position"));
        }
        let tx = self.conn.unchecked_transaction()?;
        Self::require_sync_authorized(&tx, peer, epoch)?;
        let bytes: Vec<u8> = tx.query_row(
            "SELECT data FROM sync_transfer_parts WHERE transfer=?1 AND position=?2",
            params![id, offset as i64],
            |row| row.get(0),
        )?;
        if bytes.is_empty() || bytes.len() > PART || bytes.len() > total - offset {
            return Err(invalid("invalid outgoing staged fragment"));
        }
        tx.commit()?;
        Ok(SyncPacket::Fragment(Fragment {
            from: cursor.committed,
            seq,
            kind,
            key,
            digest,
            total,
            offset,
            done: offset + bytes.len() == total && seq == manifest.upper,
            bytes: STANDARD.encode(bytes),
            recorded_at,
        }))
    }
    pub(crate) fn acknowledge_packet(
        &self,
        peer: &str,
        epoch: i64,
        cursor: &SyncCursor,
    ) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        Self::require_sync_authorized(&tx, peer, epoch)?;
        tx.execute(
            "UPDATE sync_transfers SET updated_at=?2 WHERE peer=?1 AND direction=0",
            params![peer, now_ms() / 1000],
        )?;
        tx.execute(
            "DELETE FROM sync_transfers WHERE peer=?1 AND direction=0 AND seq<=?2",
            params![peer, cursor.committed],
        )?;
        tx.commit()
    }
    pub(crate) fn apply_sync_packet(
        &self,
        peer: &str,
        epoch: i64,
        self_id: &str,
        manifest: &SyncManifest,
        packet: &SyncPacket,
    ) -> rusqlite::Result<PacketResult> {
        self.check_sync_identity()?;
        if let SyncPacket::Page(page) = packet {
            let introduced = self.apply_sync_page(peer, epoch, self_id, manifest, page)?;
            return Ok(PacketResult {
                cursor: packet.next_cursor(),
                done: page.done,
                introduced,
                words: page.content.words.len(),
                snippets: page.content.snippets.len(),
                history: page.content.history.len(),
            });
        }
        let SyncPacket::Fragment(part) = packet else {
            unreachable!()
        };
        if part.total == 0
            || part.total > WIRE
            || part.offset >= part.total
            || !part.offset.is_multiple_of(PART)
            || part.bytes.len() > PART.div_ceil(3) * 4
            || part.digest.len() != 64
            || !part.digest.bytes().all(|b| b.is_ascii_hexdigit())
            || !(0..=3).contains(&part.kind)
            || part.key.is_empty()
            || part.key.len() > 1024
            || (part.kind == 2) != part.recorded_at.is_some()
            || part.from < 0
            || part.seq <= part.from
            || part.seq > manifest.upper
        {
            return Err(invalid("invalid sync fragment header"));
        }
        let bytes = STANDARD
            .decode(&part.bytes)
            .map_err(|_| invalid("invalid sync fragment encoding"))?;
        if bytes.is_empty()
            || bytes.len() > PART
            || bytes.len() != PART.min(part.total - part.offset)
            || (part.done
                && (part.offset + bytes.len() != part.total || part.seq != manifest.upper))
        {
            return Err(invalid("invalid sync fragment progress"));
        }
        let privacy = self.retention_policy()?;
        let expired = part.recorded_at.is_some_and(|recorded_at| {
            privacy.is_some_and(|privacy| {
                privacy.transcript_auto_delete_days > 0
                    && recorded_at
                        < (now_ms() / 1000)
                            .saturating_sub(i64::from(privacy.transcript_auto_delete_days) * 86400)
            })
        });
        let tx = self.conn.unchecked_transaction()?;
        Self::require_sync_authorized(&tx, peer, epoch)?;
        let current: i64 = tx
            .query_row(
                "SELECT cursor FROM sync_received WHERE peer=?1 AND stream=?2",
                params![peer, manifest.stream],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if current != part.from {
            return Err(invalid("concurrent packet advanced cursor; retry"));
        }
        if expired {
            tx.execute("INSERT OR IGNORE INTO history(uuid,text,raw_text,words,duration_secs,deleted,discarded,recorded_at) VALUES (?1,'','',0,0,1,0,?2)",params![part.key,part.recorded_at])?;
            bump_gen(&tx)?;
        }
        if part.kind == 2
            && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM history WHERE uuid=?1 AND (deleted=1 OR discarded=1))",
                [&part.key],
                |row| row.get::<_, bool>(0),
            )?
        {
            tx.execute(
                "DELETE FROM sync_transfers WHERE peer=?1 AND direction=1",
                [peer],
            )?;
            tx.execute("INSERT INTO sync_received(peer,stream,cursor) VALUES (?1,?2,?3) ON CONFLICT(peer) DO UPDATE SET stream=excluded.stream,cursor=excluded.cursor",params![peer,manifest.stream,part.seq])?;
            tx.commit()?;
            if expired {
                bump_data_gen();
            }
            return Ok(PacketResult {
                cursor: SyncCursor {
                    committed: part.seq,
                    partial: None,
                },
                done: part.seq == manifest.upper,
                introduced: 0,
                words: 0,
                snippets: 0,
                history: 0,
            });
        }
        let previous:Option<ReceivedTransfer>=tx.query_row("SELECT id,seq,digest,base,kind,record_key,total,progress,recorded_at FROM sync_transfers WHERE peer=?1 AND direction=1 AND stream=?2",params![peer,manifest.stream],|row|Ok(ReceivedTransfer {id:row.get(0)?,seq:row.get(1)?,digest:row.get(2)?,base:row.get(3)?,kind:row.get(4)?,key:row.get(5)?,total:row.get::<_,i64>(6)? as usize,progress:row.get::<_,i64>(7)? as usize,recorded_at:row.get(8)?})).optional()?;
        let id = if let Some(saved) =
            previous.filter(|saved| saved.seq == part.seq && saved.digest == part.digest)
        {
            if saved.base != part.from
                || saved.kind != part.kind
                || saved.key != part.key
                || saved.total != part.total
                || saved.progress != part.offset
                || saved.recorded_at != part.recorded_at
            {
                return Err(invalid("mixed or concurrent sync fragments; retry"));
            }
            saved.id
        } else {
            if part.offset != 0 {
                return Err(invalid(
                    "missing initial sync fragment; resume from acknowledged position",
                ));
            }
            new_transfer(&tx, peer, 1, manifest, part)?
        };
        size_budget(&tx, bytes.len())?;
        tx.execute(
            "INSERT INTO sync_transfer_parts(transfer,position,data) VALUES (?1,?2,?3)",
            params![id, part.offset as i64, bytes],
        )?;
        let progress = part.offset + bytes.len();
        tx.execute(
            "UPDATE sync_transfers SET progress=?2,updated_at=?3 WHERE id=?1",
            params![id, progress as i64, now_ms() / 1000],
        )?;
        let mut result = PacketResult {
            cursor: packet.next_cursor(),
            done: part.done,
            introduced: 0,
            words: 0,
            snippets: 0,
            history: 0,
        };
        if progress == part.total {
            let mut hash = Sha256::new();
            let mut reader = ChunkReader::new(&tx, id, part.total);
            let mut buffer = [0u8; PART];
            loop {
                let n = reader
                    .read(&mut buffer)
                    .map_err(|_| invalid("malformed staged sync content"))?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
            }
            let digest = hash
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            if digest != part.digest {
                return Err(invalid("sync fragment digest mismatch"));
            }
            let decoded: OneRecord = serde_json::from_reader(std::io::BufReader::new(
                ChunkReader::new(&tx, id, part.total),
            ))
            .map_err(|_| invalid("malformed complete sync record"))?;
            let content = ContentSnapshot {
                words: decoded.words,
                snippets: decoded.snippets,
                history: decoded.history,
                peers: decoded.peers,
            };
            let keys = content
                .words
                .iter()
                .map(|row| (0, row.word.as_str()))
                .chain(content.snippets.iter().map(|row| (1, row.trigger.as_str())))
                .chain(content.history.iter().map(|row| (2, row.uuid.as_str())))
                .chain(content.peers.iter().map(|row| (3, row.node_id.as_str())))
                .collect::<Vec<_>>();
            if keys.as_slice() != [(part.kind, part.key.as_str())]
                || content.history.first().map(|record| record.recorded_at) != part.recorded_at
            {
                return Err(invalid("fragment content does not match its record key"));
            }
            result.words = content.words.len();
            result.snippets = content.snippets.len();
            result.history = content.history.len();
            let page = SyncPage {
                from: part.from,
                to: part.seq,
                done: part.done,
                content,
            };
            let (introduced, changed) =
                Self::merge_sync_page_on(&tx, peer, epoch, self_id, manifest, &page)?;
            result.introduced = introduced;
            tx.execute("DELETE FROM sync_transfers WHERE id=?1", [id])?;
            tx.commit()?;
            if changed {
                bump_data_gen();
            }
            self.prune_audio_best_effort();
        } else {
            tx.commit()?;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stores() -> (tempfile::TempDir, Store, Store, String, String) {
        let root = tempfile::tempdir().unwrap();
        let a = crate::device::DeviceIdentity::ephemeral().creds().node_id;
        let b = crate::device::DeviceIdentity::ephemeral().creds().node_id;
        let first = Store::open_with_key(&root.path().join("a.db"), &[51; 32]).unwrap();
        let second = Store::open_with_key(&root.path().join("b.db"), &[52; 32]).unwrap();
        for store in [&first, &second] {
            let mut settings = store.load_settings();
            settings.sync.enabled = true;
            store.save_settings(&settings).unwrap();
        }
        first.upsert_peer(&b, "synthetic b", "", true).unwrap();
        second.upsert_peer(&a, "synthetic a", "", true).unwrap();
        (root, first, second, a, b)
    }
    fn record() -> HistoryRecord {
        HistoryRecord {
            uuid: "synthetic segmented transcript".into(),
            text: "é🙂\\\0\n".repeat(2 * 1024 * 1024),
            raw_text: "synthetic raw".into(),
            words: 7,
            duration_secs: 4,
            deleted: false,
            discarded: false,
            recorded_at: 1_700_000_000,
        }
    }
    fn first_history(
        a: &Store,
        b: &Store,
        a_id: &str,
        b_id: &str,
        manifest: &SyncManifest,
    ) -> (SyncPacket, SyncCursor) {
        let mut cursor = b
            .sync_packet_cursor(a_id, b.sync_epoch(), manifest)
            .unwrap();
        for _ in 0..32 {
            let packet = a
                .export_sync_packet(b_id, a.sync_epoch(), a_id, manifest, &cursor)
                .unwrap();
            if matches!(&packet,SyncPacket::Fragment(part) if part.kind==2) {
                return (packet, cursor);
            }
            cursor = b
                .apply_sync_packet(a_id, b.sync_epoch(), b_id, manifest, &packet)
                .unwrap()
                .cursor;
            a.acknowledge_packet(b_id, a.sync_epoch(), &cursor).unwrap();
        }
        panic!("large record did not select segmentation");
    }
    fn transfer(
        a: &Store,
        b: &Store,
        a_id: &str,
        b_id: &str,
        manifest: &SyncManifest,
        mut cursor: SyncCursor,
    ) -> usize {
        let mut histories = 0;
        for _ in 0..2048 {
            let packet = a
                .export_sync_packet(b_id, a.sync_epoch(), a_id, manifest, &cursor)
                .unwrap();
            assert!(serde_json::to_vec(&packet).unwrap().len() < 16 * 1024 * 1024);
            let result = b
                .apply_sync_packet(a_id, b.sync_epoch(), b_id, manifest, &packet)
                .unwrap();
            assert!(packet.accepts(&result.cursor));
            histories += result.history;
            cursor = result.cursor;
            a.acknowledge_packet(b_id, a.sync_epoch(), &cursor).unwrap();
            if result.done {
                return histories;
            }
        }
        panic!("segmented transfer did not finish");
    }
    #[test]
    fn oversized_unicode_record_resumes_after_reopen_and_commits_once() {
        let (root, a, mut b, a_id, b_id) = stores();
        let original = record();
        a.apply_history(std::slice::from_ref(&original)).unwrap();
        let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let (packet, _) = first_history(&a, &b, &a_id, &b_id, &manifest);
        let received = b
            .apply_sync_packet(&a_id, b.sync_epoch(), &b_id, &manifest, &packet)
            .unwrap();
        assert!(received.cursor.partial.is_some());
        assert_eq!(received.history, 0);
        assert!(b.export_history().unwrap().is_empty());
        assert!(
            b.apply_sync_packet(&a_id, b.sync_epoch(), &b_id, &manifest, &packet)
                .is_err(),
            "a stale competing round skipped progress"
        );
        // Lose the acknowledgement and restart both process-owned databases.
        let saved = received.cursor;
        b.checkpoint().unwrap();
        drop(b);
        b = Store::open_with_key(&root.path().join("b.db"), &[52; 32]).unwrap();
        assert_eq!(
            b.sync_packet_cursor(&a_id, b.sync_epoch(), &manifest)
                .unwrap(),
            saved
        );
        assert_eq!(transfer(&a, &b, &a_id, &b_id, &manifest, saved), 1);
        assert_eq!(b.export_history().unwrap(), [original]);
        let cursor = b
            .sync_packet_cursor(&a_id, b.sync_epoch(), &manifest)
            .unwrap();
        assert_eq!(transfer(&a, &b, &a_id, &b_id, &manifest, cursor), 0);
        for store in [&a, &b] {
            assert_eq!(
                store
                    .conn
                    .query_row("SELECT count(*) FROM sync_transfers", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            assert_eq!(
                store
                    .conn
                    .query_row("SELECT count(*) FROM sync_transfer_parts", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
    #[test]
    fn clear_history_tombstones_partial_identity_and_prevents_restaging_speech() {
        let (_root, a, b, a_id, b_id) = stores();
        let original = record();
        a.apply_history(std::slice::from_ref(&original)).unwrap();
        let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let (packet, _) = first_history(&a, &b, &a_id, &b_id, &manifest);
        b.apply_sync_packet(&a_id, b.sync_epoch(), &b_id, &manifest, &packet)
            .unwrap();
        assert_eq!(b.clear_history().unwrap(), 1);
        let tombstone = b.export_history().unwrap().remove(0);
        assert!(tombstone.deleted && tombstone.text.is_empty() && tombstone.raw_text.is_empty());
        assert_eq!(tombstone.uuid, original.uuid);
        assert_eq!(
            b.conn
                .query_row("SELECT count(*) FROM sync_transfer_parts", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let cursor = b
            .sync_packet_cursor(&a_id, b.sync_epoch(), &manifest)
            .unwrap();
        assert_eq!(transfer(&a, &b, &a_id, &b_id, &manifest, cursor), 0);
        assert_eq!(b.export_history().unwrap(), [tombstone]);
        assert_eq!(
            b.conn
                .query_row("SELECT count(*) FROM sync_transfer_parts", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn corrupt_final_part_rolls_back_and_correct_retry_finishes() {
        let (_root, a, b, a_id, b_id) = stores();
        a.apply_history(&[record()]).unwrap();
        let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let (mut packet, mut cursor) = first_history(&a, &b, &a_id, &b_id, &manifest);
        loop {
            if matches!(&packet,SyncPacket::Fragment(part) if part.offset+STANDARD.decode(&part.bytes).unwrap().len()==part.total)
            {
                break;
            }
            cursor = b
                .apply_sync_packet(&a_id, b.sync_epoch(), &b_id, &manifest, &packet)
                .unwrap()
                .cursor;
            a.acknowledge_packet(&b_id, a.sync_epoch(), &cursor)
                .unwrap();
            packet = a
                .export_sync_packet(&b_id, a.sync_epoch(), &a_id, &manifest, &cursor)
                .unwrap();
        }
        let mut bad: SyncPacket =
            serde_json::from_value(serde_json::to_value(&packet).unwrap()).unwrap();
        if let SyncPacket::Fragment(part) = &mut bad {
            let mut bytes = STANDARD.decode(&part.bytes).unwrap();
            bytes[0] ^= 1;
            part.bytes = STANDARD.encode(bytes);
        }
        assert!(
            b.apply_sync_packet(&a_id, b.sync_epoch(), &b_id, &manifest, &bad)
                .is_err()
        );
        assert_eq!(
            b.sync_packet_cursor(&a_id, b.sync_epoch(), &manifest)
                .unwrap(),
            cursor
        );
        assert!(b.export_history().unwrap().is_empty());
        let result = b
            .apply_sync_packet(&a_id, b.sync_epoch(), &b_id, &manifest, &packet)
            .unwrap();
        assert_eq!(result.history, 1);
        assert_eq!(b.export_history().unwrap(), [record()]);
    }
    #[test]
    fn malformed_progress_and_policy_loss_never_publish_or_keep_partial_speech() {
        let (_root, a, b, a_id, b_id) = stores();
        a.apply_history(&[record()]).unwrap();
        let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let (packet, _) = first_history(&a, &b, &a_id, &b_id, &manifest);
        let epoch = b.sync_epoch();
        for case in ["offset", "total", "encoding", "key", "digest"] {
            let mut bad: SyncPacket =
                serde_json::from_value(serde_json::to_value(&packet).unwrap()).unwrap();
            if let SyncPacket::Fragment(part) = &mut bad {
                match case {
                    "offset" => part.offset = 1,
                    "total" => part.total = WIRE + 1,
                    "encoding" => part.bytes = "!".into(),
                    "key" => part.key.clear(),
                    "digest" => part.digest = "not a digest".into(),
                    _ => unreachable!(),
                }
            }
            assert!(
                b.apply_sync_packet(&a_id, epoch, &b_id, &manifest, &bad)
                    .is_err(),
                "{case}"
            );
            assert_eq!(
                b.conn
                    .query_row(
                        "SELECT count(*) FROM sync_transfers WHERE direction=1",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
        }
        b.apply_sync_packet(&a_id, epoch, &b_id, &manifest, &packet)
            .unwrap();
        let mut settings = b.load_settings();
        settings.sync.enabled = false;
        b.save_settings(&settings).unwrap();
        assert_eq!(
            b.conn
                .query_row("SELECT count(*) FROM sync_transfer_parts", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(
            b.apply_sync_packet(&a_id, epoch, &b_id, &manifest, &packet)
                .is_err()
        );
        assert!(b.export_history().unwrap().is_empty());
    }

    #[test]
    fn reset_revocation_and_untrust_remove_both_directions_and_deny_old_work() {
        // One shared pre-action snapshot: the two staged directions are
        // expensive to build (multi-megabyte staging per direction) and
        // identical for all three actions. Each action restores the
        // snapshot files and reopens, so tails observe byte-identical
        // starting states — including the staged rows, which the
        // navigation below reuses from cache without re-staging.
        let root = tempfile::tempdir().unwrap();
        let a_id = crate::device::DeviceIdentity::ephemeral().creds().node_id;
        let b_id = crate::device::DeviceIdentity::ephemeral().creds().node_id;
        let snap_a = root.path().join("snap_a.db");
        let snap_b = root.path().join("snap_b.db");
        {
            let a =
                crate::store::Store::open_with_key(&root.path().join("a.db"), &[51; 32]).unwrap();
            let b =
                crate::store::Store::open_with_key(&root.path().join("b.db"), &[52; 32]).unwrap();
            for store in [&a, &b] {
                let mut settings = store.load_settings();
                settings.sync.enabled = true;
                store.save_settings(&settings).unwrap();
            }
            a.upsert_peer(&b_id, "synthetic b", "", true).unwrap();
            b.upsert_peer(&a_id, "synthetic a", "", true).unwrap();
            let original = record();
            a.apply_history(std::slice::from_ref(&original)).unwrap();
            let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
            let (packet, _) = first_history(&a, &b, &a_id, &b_id, &manifest);
            let epoch = b.sync_epoch();
            b.apply_sync_packet(&a_id, epoch, &b_id, &manifest, &packet)
                .unwrap();
            // Exercise outgoing staging as well, without publishing incoming text.
            b.apply_history(&[HistoryRecord {
                uuid: "opposite direction".into(),
                ..original.clone()
            }])
            .unwrap();
            let opposite = b.sync_manifest(&a_id, epoch).unwrap();
            let _ = first_history(&b, &a, &b_id, &a_id, &opposite);
            assert_eq!(
                b.conn
                    .query_row("SELECT count(*) FROM sync_transfers", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                2
            );
            for store in [&a, &b] {
                store.checkpoint().unwrap();
            }
            drop(a);
            drop(b);
            for (live, snap) in [
                (root.path().join("a.db"), &snap_a),
                (root.path().join("b.db"), &snap_b),
            ] {
                std::fs::copy(&live, snap).unwrap();
                for suffix in ["-wal", "-journal"] {
                    let mut sidecar = live.as_os_str().to_owned();
                    sidecar.push(suffix);
                    let sidecar = std::path::PathBuf::from(sidecar);
                    if sidecar.exists() {
                        let mut target = snap.as_os_str().to_owned();
                        target.push(suffix);
                        std::fs::copy(&sidecar, std::path::PathBuf::from(target)).unwrap();
                    }
                }
            }
        }
        for action in ["reset", "revoke", "untrust"] {
            for (live, snap) in [
                (root.path().join("a.db"), &snap_a),
                (root.path().join("b.db"), &snap_b),
            ] {
                let _ = std::fs::remove_file(&live);
                std::fs::copy(snap, &live).unwrap();
                for suffix in ["-wal", "-journal"] {
                    let mut sidecar = live.as_os_str().to_owned();
                    sidecar.push(suffix);
                    let sidecar = std::path::PathBuf::from(sidecar);
                    let _ = std::fs::remove_file(&sidecar);
                    let mut source = snap.as_os_str().to_owned();
                    source.push(suffix);
                    let source = std::path::PathBuf::from(source);
                    if source.exists() {
                        std::fs::copy(&source, &sidecar).unwrap();
                    }
                }
            }
            let a =
                crate::store::Store::open_with_key(&root.path().join("a.db"), &[51; 32]).unwrap();
            let b =
                crate::store::Store::open_with_key(&root.path().join("b.db"), &[52; 32]).unwrap();
            let original = record();
            let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
            let (packet, _) = first_history(&a, &b, &a_id, &b_id, &manifest);
            let epoch = b.sync_epoch();
            assert_eq!(
                b.conn
                    .query_row("SELECT count(*) FROM sync_transfers", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                2
            );
            match action {
                "reset" => b.reset_all_data().unwrap(),
                "revoke" => b.remove_peer(&a_id).unwrap(),
                "untrust" => b.upsert_peer(&a_id, "synthetic a", "", false).unwrap(),
                _ => unreachable!(),
            }
            assert_eq!(
                b.conn
                    .query_row("SELECT count(*) FROM sync_transfer_parts", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0,
                "{action}"
            );
            assert!(
                b.apply_sync_packet(&a_id, epoch, &b_id, &manifest, &packet)
                    .is_err(),
                "{action}"
            );
            let received = b
                .export_history()
                .unwrap()
                .into_iter()
                .find(|row| row.uuid == original.uuid);
            if action == "reset" {
                let tombstone = received.expect("reset lost pending deletion identity");
                assert!(
                    tombstone.deleted && tombstone.text.is_empty() && tombstone.raw_text.is_empty()
                );
            } else {
                assert!(received.is_none());
            }
        }
    }

    #[test]
    fn changed_source_abandons_partial_snapshot_and_merges_only_current_data() {
        let (_root, a, b, a_id, b_id) = stores();
        let original = record();
        a.apply_history(std::slice::from_ref(&original)).unwrap();
        let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let (packet, _) = first_history(&a, &b, &a_id, &b_id, &manifest);
        b.apply_sync_packet(&a_id, b.sync_epoch(), &b_id, &manifest, &packet)
            .unwrap();
        a.delete_history(&original.uuid).unwrap();
        let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let cursor = b
            .sync_packet_cursor(&a_id, b.sync_epoch(), &manifest)
            .unwrap();
        assert_eq!(transfer(&a, &b, &a_id, &b_id, &manifest, cursor), 1);
        let row = b.export_history().unwrap().remove(0);
        assert!(row.deleted && row.text.is_empty() && row.raw_text.is_empty());
        for store in [&a, &b] {
            assert_eq!(
                store
                    .conn
                    .query_row("SELECT count(*) FROM sync_transfer_parts", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn staging_and_decoded_string_limits_reject_without_allocating_the_budget() {
        let (_root, _a, b, _a_id, _b_id) = stores();
        assert!(size_budget(&b.conn, STAGING as usize).is_ok());
        assert!(size_budget(&b.conn, STAGING as usize + 1).is_err());
        let mut reader = ChunkReader::new(&b.conn, 0, 0);
        reader.text = TEXT + 4096 - 5;
        // A surrogate pair is four decoded UTF-8 bytes; an escaped control
        // byte is one. Split every escape across independent reads.
        for byte in br#""\uD83D\uDE42\n"# {
            reader.count(&[*byte]).unwrap();
        }
        assert_eq!(reader.text, TEXT + 4096);
        assert!(reader.count(b"x").is_err());
        let fragment = Fragment {
            from: 0,
            seq: 1,
            kind: 2,
            key: "synthetic".into(),
            digest: "a".repeat(64),
            total: PART * 2,
            offset: 0,
            bytes: String::new(),
            done: false,
            recorded_at: Some(1),
        };
        let manifest = SyncManifest {
            stream: "synthetic budget".into(),
            upper: 1,
        };
        let tx = b.conn.unchecked_transaction().unwrap();
        for index in 0..TRANSFERS {
            let peer = format!("synthetic budget peer {index}");
            tx.execute(
                "INSERT INTO devices(node_id,name,trusted) VALUES (?1,'synthetic',1)",
                [&peer],
            )
            .unwrap();
            new_transfer(&tx, &peer, 1, &manifest, &fragment).unwrap();
        }
        tx.execute(
            "INSERT INTO devices(node_id,name,trusted) VALUES ('budget overflow','synthetic',1)",
            [],
        )
        .unwrap();
        assert!(new_transfer(&tx, "budget overflow", 1, &manifest, &fragment).is_err());
        assert_eq!(
            tx.query_row("SELECT count(*) FROM sync_transfers", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            TRANSFERS
        );
        drop(tx);
        assert_eq!(
            b.conn
                .query_row("SELECT count(*) FROM sync_transfers", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn retention_redacts_pending_identity_before_clock_or_idle_cleanup() {
        let (_root, a, b, a_id, b_id) = stores();
        let original = record();
        a.apply_history(std::slice::from_ref(&original)).unwrap();
        let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let (packet, _) = first_history(&a, &b, &a_id, &b_id, &manifest);
        b.apply_sync_packet(&a_id, b.sync_epoch(), &b_id, &manifest, &packet)
            .unwrap();
        assert_eq!(
            b.expire_pending_history_at(original.recorded_at + 2 * 86400, 1)
                .unwrap(),
            1
        );
        let tombstone = b.export_history().unwrap().remove(0);
        assert!(tombstone.deleted && tombstone.text.is_empty() && tombstone.raw_text.is_empty());
        assert_eq!(
            b.conn
                .query_row("SELECT count(*) FROM sync_transfer_parts", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        // Expired metadata cannot disappear before its deletion identity is recorded.
        let cursor = b
            .sync_packet_cursor(&a_id, b.sync_epoch(), &manifest)
            .unwrap();
        assert_eq!(transfer(&a, &b, &a_id, &b_id, &manifest, cursor), 0);
        assert_eq!(b.export_history().unwrap(), [tombstone]);
    }

    #[test]
    fn idle_and_backward_clock_restart_partial_progress_without_losing_source() {
        for backward in [false, true] {
            let (_root, a, b, a_id, b_id) = stores();
            let original = record();
            a.apply_history(std::slice::from_ref(&original)).unwrap();
            let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
            let (packet, _) = first_history(&a, &b, &a_id, &b_id, &manifest);
            b.apply_sync_packet(&a_id, b.sync_epoch(), &b_id, &manifest, &packet)
                .unwrap();
            let updated: i64 = b
                .conn
                .query_row(
                    "SELECT updated_at FROM sync_transfers WHERE direction=1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            b.expire_sync_transfers_at(if backward {
                updated - 1
            } else {
                updated + 3601
            })
            .unwrap();
            let cursor = b
                .sync_packet_cursor(&a_id, b.sync_epoch(), &manifest)
                .unwrap();
            assert!(cursor.partial.is_none());
            assert!(b.export_history().unwrap().is_empty());
            assert_eq!(a.export_history().unwrap(), [original]);
            assert_eq!(transfer(&a, &b, &a_id, &b_id, &manifest, cursor), 1);
            assert_eq!(b.export_history().unwrap(), a.export_history().unwrap());
        }
    }

    #[test]
    fn authenticated_but_malformed_complete_records_cannot_merge_or_advance() {
        let (_root, a, b, a_id, b_id) = stores();
        let manifest = a.sync_manifest(&b_id, a.sync_epoch()).unwrap();
        let first = HistoryRecord {
            text: "synthetic".into(),
            raw_text: String::new(),
            uuid: "first".into(),
            ..record()
        };
        let second = HistoryRecord {
            uuid: "second".into(),
            ..first.clone()
        };
        let body = serde_json::to_vec(&ContentSnapshot {
            history: vec![first, second],
            ..Default::default()
        })
        .unwrap();
        let digest = Sha256::digest(&body)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let packet = SyncPacket::Fragment(Fragment {
            from: 0,
            seq: manifest.upper,
            kind: 2,
            key: "first".into(),
            digest,
            total: body.len(),
            offset: 0,
            bytes: STANDARD.encode(body),
            done: true,
            recorded_at: Some(1_700_000_000),
        });
        assert!(
            b.apply_sync_packet(&a_id, b.sync_epoch(), &b_id, &manifest, &packet)
                .is_err()
        );
        assert!(b.export_history().unwrap().is_empty());
        assert_eq!(
            b.sync_packet_cursor(&a_id, b.sync_epoch(), &manifest)
                .unwrap(),
            SyncCursor {
                committed: 0,
                partial: None
            }
        );
        assert_eq!(
            b.conn
                .query_row("SELECT count(*) FROM sync_transfer_parts", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn orphaned_transfer_parts_are_retired_without_losing_active_work() {
        let (_root, a, _, a_id, b_id) = stores();
        // Peer row required by sync_transfers(peer REFERENCES devices).
        a.upsert_peer(&b_id, "synthetic b", "", true).unwrap();
        let _ = a_id;
        let now = crate::store::now_ms() / 1000;
        // Active transfer with one part.
        a.conn
            .execute(
                "INSERT INTO sync_transfers(peer,direction,stream,base,seq,kind,record_key,digest,total,progress,updated_at) VALUES(?1,0,'s',0,1,0,'k','d',10,5,?2)",
                rusqlite::params![b_id, now],
            )
            .unwrap();
        let id: i64 = a
            .conn
            .query_row("SELECT id FROM sync_transfers", [], |r| r.get(0))
            .unwrap();
        a.conn
            .execute(
                "INSERT INTO sync_transfer_parts(transfer,position,data) VALUES(?1,0,X'00')",
                [id],
            )
            .unwrap();
        // Orphan part from an interrupted delete on a build without FK.
        a.conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        a.conn
            .execute(
                "INSERT INTO sync_transfer_parts(transfer,position,data) VALUES(999999,0,X'00')",
                [],
            )
            .unwrap();
        a.conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        a.expire_sync_transfers_at(now).unwrap();
        let remaining: i64 = a
            .conn
            .query_row("SELECT count(*) FROM sync_transfer_parts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 1, "only the orphan must be retired");
    }
}

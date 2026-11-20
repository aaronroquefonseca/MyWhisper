//! Node-to-node sync transport (v3): Noise-encrypted TCP.
//!
//! Everything on the wire is encrypted with `Noise_XX` (mutual
//! authentication, forward secrecy) and framed. Trust is
//! table-driven with Syncthing-style introduction:
//!
//! * unknown dialers are recorded as pending requests, never merged;
//! * accepting marks the peer trusted + introducer, then syncs;
//! * introducers' vouched devices join automatically as trusted;
//! * everyone exchanges peer lists, so addresses stay fresh without
//!   typing IPs (mDNS covers LAN discovery on top).
//!
//! Identity binding: the Noise static keys prove a fresh encrypted
//! channel, and each hello carries an ed25519 signature over the
//! session's handshake hash made with the node's long-term identity
//! key. A claimed node id only validates against its own key, so
//! spoofing buys nothing and replays die with the session.
//!
//! Message flow inside the encrypted channel (strict ping-pong, the
//! initiator sends first at every step):
//!
//! * hello both ways: `{"v":1,"node_id":"…","name":"…",
//!   "addresses":[],"introducer":false,"sig":[…]}`
//! * verdict from responder: `{"ok":true}` or `{"error":"…"}`
//! * records both ways: `{"words":[…],"snippets":[…],
//!   "history":[…],"peers":[…]}`
//!
//! Incremental rounds ride on optional fields that older builds ignore
//! and never send: the verdict and the dialer's records carry `since`
//! (the mark of the last complete merge from the other side), and both
//! record messages carry `mark` (the position they cover). Records go
//! out only when changed after the peer's `since`; without it, or for a
//! mark from another database, a round sends everything as before.
//!
//! Reachability v1: same LAN (mDNS/discovered/gossiped addresses),
//! VPN, or internet with a known address. Hole punching and relays
//! (libp2p-class) are the later internet phase.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{
    IpAddr, SocketAddr, SocketAddrV6, TcpListener, TcpStream, ToSocketAddrs, UdpSocket,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::device::{DeviceIdentity, SyncCreds, short_id, valid_node_id};
use crate::store::{HistoryRecord, PeerGossip, SnippetRecord, Store, SyncMark, WordRecord};

const PROTOCOL_V: u32 = 1;
const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const NOISE_BUF: usize = 65535;
const CHUNK: usize = 60000;
/// A Noise message never exceeds 65535 bytes, so no valid frame (from
/// any build) is larger; checked before allocating.
const FRAME_CAP: usize = NOISE_BUF;
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const GUARDED_WRITE_TIMEOUT: Duration = Duration::from_millis(50);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Anyone can open a Noise session, so until the peer is proven and
/// trusted the session gets a total deadline and small messages only.
const PREAUTH_DEADLINE: Duration = Duration::from_secs(30);
const PREAUTH_MSG_CAP: usize = 64 * 1024;
const MSG_CAP: usize = 256 * 1024 * 1024;
/// After trust, every message gets its own deadline: this floor plus
/// `POSTAUTH_MIN_RATE`. The 256 MiB cap fits in ~19 minutes, a real
/// LAN/VPN sync in far less, and a stalled peer frees its worker.
const POSTAUTH_BASE: Duration = Duration::from_secs(120);
const POSTAUTH_MIN_RATE: u64 = 256 * 1024;
/// Concurrent inbound connections; extras are closed on accept.
const MAX_INBOUND: usize = 8;
const AUTO_SYNC_SECS: u64 = 60;
const MDNS_SERVICE: &str = "_mywhisper._tcp.local.";
/// A reassigned address can reach another of the user's devices.
const NODE_MISMATCH: &str = "peer node id mismatch";

#[derive(Debug, Serialize, Deserialize)]
struct Hello {
    v: u32,
    node_id: String,
    name: String,
    #[serde(default)]
    addresses: Vec<String>,
    #[serde(default)]
    introducer: bool,
    #[serde(default)]
    sig: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Verdict {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    since: Option<SyncMark>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Payload {
    #[serde(default)]
    words: Vec<WordRecord>,
    #[serde(default)]
    snippets: Vec<SnippetRecord>,
    #[serde(default)]
    history: Vec<HistoryRecord>,
    #[serde(default)]
    peers: Vec<PeerGossip>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    since: Option<SyncMark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mark: Option<SyncMark>,
}

#[derive(Debug)]
pub struct SyncReport {
    pub peer_node: String,
    pub peer_name: String,
    pub words_in: usize,
    pub snippets_in: usize,
    pub history_in: usize,
    pub introduced: usize,
}

/// mDNS-discovered peer (LAN, no configuration).
#[derive(Debug, Clone)]
pub struct Discovered {
    pub node_id: String,
    pub name: String,
    pub addresses: Vec<String>,
}

// -- Noise framing --------------------------------------------------------

/// Time left before `deadline`, per-call capped at `IO_TIMEOUT`.
fn time_left(deadline: Instant, what: &str) -> Result<Duration, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(format!("{what}: deadline passed"));
    }
    Ok(left.min(IO_TIMEOUT))
}

/// `write_all` bounded by a total `deadline`, so a peer reading one
/// byte at a time cannot hold the connection forever.
fn write_frame(
    stream: &mut TcpStream,
    bytes: &[u8],
    deadline: Instant,
    permission: Option<FramePermission<'_>>,
) -> Result<(), String> {
    if bytes.len() > FRAME_CAP {
        return Err("frame too large".to_string());
    }
    let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(bytes);
    let mut sent = 0;
    let mut idle_deadline = Instant::now() + IO_TIMEOUT;
    while sent < out.len() {
        let timeout = time_left(deadline.min(idle_deadline), "write")?;
        stream
            .set_write_timeout(Some(if permission.is_some() {
                timeout.min(GUARDED_WRITE_TIMEOUT)
            } else {
                timeout
            }))
            .map_err(|e| format!("timeout: {e}"))?;
        // Withdrawal and writes serialize across processes via SQLite.
        // Release the writer after each bounded syscall, even on a stall.
        let guard = permission.map(FramePermission::acquire).transpose()?;
        let result = stream.write(&out[sent..]);
        drop(guard);
        match result {
            Ok(0) => return Err("write: connection closed".to_string()),
            Ok(n) => {
                sent += n;
                idle_deadline = Instant::now() + IO_TIMEOUT;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e)
                if permission.is_some()
                    && matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
            {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) => return Err(format!("write: {e}")),
        }
    }
    Ok(())
}

/// `read_exact` that also honours a total `deadline`, so a peer
/// trickling bytes cannot outlast the per-read timeout forever.
fn read_full(stream: &mut TcpStream, buf: &mut [u8], deadline: Instant) -> Result<(), String> {
    let mut filled = 0;
    while filled < buf.len() {
        stream
            .set_read_timeout(Some(time_left(deadline, "read")?))
            .map_err(|e| format!("timeout: {e}"))?;
        match stream.read(&mut buf[filled..]) {
            Ok(0) => return Err("read: connection closed".to_string()),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    Ok(())
}

fn read_frame(stream: &mut TcpStream, deadline: Instant) -> Result<Vec<u8>, String> {
    let mut len = [0u8; 4];
    read_full(stream, &mut len, deadline)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > FRAME_CAP {
        return Err("bad frame length".to_string());
    }
    let mut buf = vec![0u8; len];
    read_full(stream, &mut buf, deadline)?;
    Ok(buf)
}

/// One encrypted session: Noise_XX handshake, then length-framed
/// chunked messages over the transport.
pub struct NoiseStream {
    stream: TcpStream,
    transport: snow::TransportState,
    rbuf: Vec<u8>,
    /// Pre-auth: one deadline for the whole phase. After
    /// `authenticated`: reset per message (`message_deadline`).
    deadline: Instant,
    authed: bool,
}

fn message_deadline(len: usize) -> Instant {
    Instant::now() + POSTAUTH_BASE + Duration::from_secs(len as u64 / POSTAUTH_MIN_RATE)
}

impl NoiseStream {
    fn builder(private_key: &[u8; 32], initiator: bool) -> Result<snow::HandshakeState, String> {
        let params: snow::params::NoiseParams = NOISE_PARAMS
            .parse()
            .map_err(|e| format!("noise params: {e}"))?;
        let builder = snow::Builder::new(params)
            .local_private_key(private_key)
            .map_err(|e| format!("noise key: {e}"))?;
        if initiator {
            builder.build_initiator()
        } else {
            builder.build_responder()
        }
        .map_err(|e| format!("noise build: {e}"))
    }

    /// Run the handshake. Returns the session plus the handshake hash
    /// (signed into hellos) and the peer's proven x25519 static key.
    pub fn handshake(
        mut stream: TcpStream,
        private_key: &[u8; 32],
        initiator: bool,
    ) -> Result<(Self, Vec<u8>, [u8; 32]), String> {
        let deadline = Instant::now() + PREAUTH_DEADLINE;
        let mut hs = Self::builder(private_key, initiator)?;
        let mut buf = vec![0u8; NOISE_BUF];
        let mut payload = vec![0u8; NOISE_BUF];
        if initiator {
            // XX initiator: write, read, write.
            loop {
                let n = hs
                    .write_message(&[], &mut buf)
                    .map_err(|e| format!("handshake write: {e}"))?;
                write_frame(&mut stream, &buf[..n], deadline, None)?;
                if hs.is_handshake_finished() {
                    break;
                }
                let msg = read_frame(&mut stream, deadline)?;
                hs.read_message(&msg, &mut payload)
                    .map_err(|e| format!("handshake read: {e}"))?;
                if hs.is_handshake_finished() {
                    break;
                }
            }
        } else {
            loop {
                let msg = read_frame(&mut stream, deadline)?;
                hs.read_message(&msg, &mut payload)
                    .map_err(|e| format!("handshake read: {e}"))?;
                if hs.is_handshake_finished() {
                    break;
                }
                let n = hs
                    .write_message(&[], &mut buf)
                    .map_err(|e| format!("handshake write: {e}"))?;
                write_frame(&mut stream, &buf[..n], deadline, None)?;
            }
        }
        let hash = hs.get_handshake_hash().to_vec();
        let remote: [u8; 32] = hs
            .get_remote_static()
            .ok_or_else(|| "no remote static key".to_string())?
            .try_into()
            .map_err(|_| "bad remote static length".to_string())?;
        let transport = hs
            .into_transport_mode()
            .map_err(|e| format!("transport mode: {e}"))?;
        Ok((
            Self {
                stream,
                transport,
                rbuf: vec![0u8; NOISE_BUF],
                deadline,
                authed: false,
            },
            hash,
            remote,
        ))
    }

    /// The peer is proven and trusted: lift the pre-auth limits.
    fn authenticated(&mut self) {
        self.authed = true;
        self.deadline = message_deadline(0);
    }

    fn write_chunk(
        &mut self,
        plaintext: &[u8],
        permission: Option<FramePermission<'_>>,
    ) -> Result<(), String> {
        let mut buf = vec![0u8; plaintext.len() + 32];
        let n = self
            .transport
            .write_message(plaintext, &mut buf)
            .map_err(|e| format!("encrypt: {e}"))?;
        #[cfg(test)]
        if permission.is_some() {
            tests::before_send();
        }
        write_frame(&mut self.stream, &buf[..n], self.deadline, permission)
    }

    /// Send one logical message (chunked at 60 KiB plaintext).
    pub fn send_msg(&mut self, data: &[u8]) -> Result<(), String> {
        self.send_msg_checked(data, None)
    }

    /// Permission withdrawal truncates the message so the peer rejects it.
    fn send_msg_checked(
        &mut self,
        data: &[u8],
        permission: Option<FramePermission<'_>>,
    ) -> Result<(), String> {
        if self.authed {
            self.deadline = message_deadline(data.len());
        }
        let total = data.len() as u64;
        self.write_chunk(&total.to_be_bytes(), permission)?;
        for chunk in data.chunks(CHUNK) {
            self.write_chunk(chunk, permission)?;
        }
        Ok(())
    }

    pub fn send_json(&mut self, value: &impl Serialize) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|e| format!("encode: {e}"))?;
        self.send_msg(&bytes)
    }

    fn read_chunk(&mut self) -> Result<Vec<u8>, String> {
        let frame = read_frame(&mut self.stream, self.deadline)?;
        let n = self
            .transport
            .read_message(&frame, &mut self.rbuf)
            .map_err(|e| format!("decrypt: {e}"))?;
        Ok(self.rbuf[..n].to_vec())
    }

    /// Receive one logical message, reassembling chunks.
    pub fn recv_msg(&mut self) -> Result<Vec<u8>, String> {
        if self.authed {
            self.deadline = message_deadline(0);
        }
        let head = self.read_chunk()?;
        if head.len() != 8 {
            return Err("bad message header".to_string());
        }
        let total = u64::from_be_bytes(head[..8].try_into().unwrap()) as usize;
        let cap = if self.authed {
            MSG_CAP
        } else {
            PREAUTH_MSG_CAP
        };
        if total > cap {
            return Err("message too large".to_string());
        }
        if self.authed {
            self.deadline = message_deadline(total);
        }
        let mut out = Vec::with_capacity(total.min(1024 * 1024));
        while out.len() < total {
            let chunk = self.read_chunk()?;
            if chunk.is_empty() {
                return Err("empty chunk".to_string());
            }
            out.extend_from_slice(&chunk);
        }
        if out.len() != total {
            return Err("message truncated".to_string());
        }
        Ok(out)
    }

    pub fn recv_json<T: serde::de::DeserializeOwned>(&mut self) -> Result<T, String> {
        let bytes = self.recv_msg()?;
        serde_json::from_slice(&bytes).map_err(|e| format!("decode: {e}"))
    }
}

// -- exchange ---------------------------------------------------------------

/// Bounds of one merge transaction (rows, then text bytes): it holds
/// the write lock, which local dictation writes wait on.
const MERGE_BATCH_ROWS: usize = 200;
const MERGE_BATCH_BYTES: usize = 256 * 1024;
/// Pause between merge transactions so waiting writers get the lock.
const MERGE_YIELD: Duration = Duration::from_millis(5);

/// Split `rows` into consecutive merge batches within both bounds
/// (`size` is a row's text bytes; a single oversized row still gets a
/// batch of its own).
fn merge_batches<T>(rows: &[T], size: impl Fn(&T) -> usize) -> Vec<&[T]> {
    let mut out = Vec::new();
    let (mut start, mut bytes) = (0, 0);
    for (i, row) in rows.iter().enumerate() {
        let row_bytes = size(row) + 64;
        if i > start && (i - start == MERGE_BATCH_ROWS || bytes + row_bytes > MERGE_BATCH_BYTES) {
            out.push(&rows[start..i]);
            (start, bytes) = (i, 0);
        }
        bytes += row_bytes;
    }
    if start < rows.len() {
        out.push(&rows[start..]);
    }
    out
}

fn gossip_out(store: &Store, self_id: &str) -> Vec<PeerGossip> {
    store
        .export_gossip()
        .unwrap_or_default()
        .into_iter()
        .filter(|g| g.node_id != self_id)
        .collect()
}

/// Authoritative gate, re-read right before records move in either
/// direction: the sync switch is on, we trust the peer, and no
/// permission was withdrawn since `epoch` was captured (before export).
fn may_sync(store: &Store, node_id: &str, epoch: i64) -> Result<(), String> {
    if !store.sync_enabled() {
        return Err("sync is off".to_string());
    }
    let trusted = store
        .get_peer(node_id)
        .map_err(|e| format!("peer lookup: {e}"))?
        .is_some_and(|p| p.trusted);
    if !trusted {
        return Err("peer not trusted".to_string());
    }
    if store.sync_epoch().map_err(|e| format!("epoch: {e}"))? != epoch {
        return Err("sync permission changed during the round".to_string());
    }
    Ok(())
}

/// Off only in tests that play an older build, which neither sends nor
/// uses incremental marks.
fn watermarks() -> bool {
    #[cfg(test)]
    return !tests::playing_old_build();
    #[cfg(not(test))]
    true
}

#[derive(Clone, Copy)]
struct FramePermission<'a> {
    store: &'a Store,
    node_id: &'a str,
    epoch: i64,
}

impl<'a> FramePermission<'a> {
    fn acquire(self) -> Result<rusqlite::Transaction<'a>, String> {
        let guard = self.store.begin_merge().map_err(|e| format!("send: {e}"))?;
        may_sync(self.store, self.node_id, self.epoch)?;
        Ok(guard)
    }
}

/// Run one side of an exchange over an encrypted session.
///
/// * `initiator` -- dialer sends first at every step (strict
///   alternation, so large payloads cannot deadlock).
/// * `expect_node` -- when set, the proven session must belong to
///   exactly this node id (client side).
/// * `remote` -- dial-back address observed for this contact.
///
/// The responder opens `db_path` only after the peer proved its id.
#[allow(clippy::too_many_arguments)]
pub fn exchange(
    session: &mut NoiseStream,
    session_hash: &[u8],
    db_path: &Path,
    creds: &SyncCreds,
    name: &str,
    addresses: &[String],
    initiator: bool,
    expect_node: Option<&str>,
    remote: Option<String>,
) -> Result<SyncReport, String> {
    let open = || Store::open(db_path).map_err(|e| format!("open db: {e}"));
    let dialer_store = if initiator {
        let store = open()?;
        if !store.sync_enabled() {
            return Err("sync is off".to_string());
        }
        Some(store)
    } else {
        None
    };
    let hello = Hello {
        v: PROTOCOL_V,
        node_id: creds.node_id.clone(),
        name: name.to_string(),
        addresses: addresses.to_vec(),
        introducer: true,
        sig: DeviceIdentity::sign_session_static(&creds.signing, session_hash).to_vec(),
    };
    let peer: Hello = if initiator {
        session.send_json(&hello)?;
        session.recv_json()?
    } else {
        let peer: Hello = session.recv_json()?;
        session.send_json(&hello)?;
        peer
    };
    // Bind the claimed id to the proven handshake: the signature must
    // verify against the claimed node id over this session's hash.
    // Spoofing a node id requires its ed25519 private key, and
    // replaying a proof fails because every handshake hashes fresh
    // ephemerals.
    //
    // Rejections on the responder side always send an error verdict
    // first, so the dialer gets a clean rejection instead of a hang.
    // Spoofed ids are never recorded (no pending row for forgeries).
    let mut sig = [0u8; 64];
    let proof_ok = valid_node_id(&peer.node_id) && peer.sig.len() == 64 && {
        sig.copy_from_slice(&peer.sig);
        DeviceIdentity::verify_session(&peer.node_id, session_hash, &sig)
    };
    if initiator {
        if peer.v != PROTOCOL_V {
            return Err(format!("protocol mismatch: v{}", peer.v));
        }
        if !proof_ok {
            return Err("identity proof failed".to_string());
        }
    } else if peer.v != PROTOCOL_V || !proof_ok {
        session.send_json(&Verdict {
            ok: false,
            error: "identity proof failed".to_string(),
            since: None,
        })?;
        return Err("identity proof failed".to_string());
    }
    let store = match dialer_store {
        Some(store) => store,
        None => open()?,
    };
    if !initiator && !store.sync_enabled() {
        session.send_json(&Verdict {
            ok: false,
            error: "sync is off".to_string(),
            since: None,
        })?;
        return Err("sync is off".to_string());
    }
    if let Some(expected) = expect_node
        && peer.node_id != expected
    {
        return Err(NODE_MISMATCH.to_string());
    }

    // Contact bookkeeping: unknown nodes become pending requests,
    // known ones refresh addresses.
    let mut contact_addrs: Vec<String> = peer.addresses.clone();
    if let Some(remote) = remote
        && !contact_addrs.contains(&remote)
    {
        contact_addrs.push(remote);
    }
    let is_new = store
        .record_seen(&peer.node_id, &peer.name, &contact_addrs)
        .map_err(|e| format!("record contact: {e}"))?;
    let trusted = store
        .get_peer(&peer.node_id)
        .map_err(|e| format!("peer lookup: {e}"))?
        .map(|p| p.trusted)
        .unwrap_or(false);

    // What we already merged from this peer: it sends only later changes.
    let our_since = if watermarks() && trusted {
        store
            .peer_mark(&peer.node_id)
            .map_err(|e| format!("sync mark: {e}"))?
    } else {
        None
    };
    let mut their_since = None;
    if initiator {
        let verdict: Verdict = session.recv_json()?;
        if !verdict.ok {
            return Err(if verdict.error.is_empty() {
                "rejected".to_string()
            } else {
                verdict.error
            });
        }
        their_since = verdict.since;
    } else if !trusted {
        session.send_json(&Verdict {
            ok: false,
            error: "not trusted: request recorded, ask them to accept".to_string(),
            since: None,
        })?;
        return Err(if is_new {
            format!("new pairing request from {}", short_id(&peer.node_id))
        } else {
            "peer not trusted".to_string()
        });
    }
    if !initiator {
        session.send_json(&Verdict {
            ok: true,
            error: String::new(),
            since: our_since.clone(),
        })?;
    }
    session.authenticated();
    // Our own trust and the switch gate export too (the responder's
    // verdict only covers their side). The epoch captured here makes
    // any switch-off, revocation or reset after this point abort the
    // round at the next check: before our send and inside the merge.
    let epoch = store.sync_epoch().map_err(|e| format!("epoch: {e}"))?;
    may_sync(&store, &peer.node_id, epoch)?;

    // Records, strict alternation again. The responder learns the
    // dialer's `since` from its records, so it exports after reading them.
    let export = |their_since: Option<SyncMark>| -> Result<Vec<u8>, String> {
        let changes = store
            .export_changes(their_since.as_ref().filter(|_| watermarks()))
            .map_err(|e| format!("export: {e}"))?;
        serde_json::to_vec(&Payload {
            words: changes.words,
            snippets: changes.snippets,
            history: changes.history,
            peers: gossip_out(&store, &creds.node_id),
            since: our_since.clone(),
            mark: watermarks().then_some(changes.mark),
        })
        .map_err(|e| format!("encode: {e}"))
    };
    let permission = FramePermission {
        store: &store,
        node_id: &peer.node_id,
        epoch,
    };
    let mut theirs: Payload = if initiator {
        session.send_msg_checked(&export(their_since)?, Some(permission))?;
        session.recv_json()?
    } else {
        let theirs: Payload = session.recv_json()?;
        session.send_msg_checked(&export(theirs.since.clone())?, Some(permission))?;
        theirs
    };

    // Recheck permission under the write lock for every batch. Already
    // committed records converge on retry via the existing merge rules.
    let mut merged_batch = false;
    let mut in_batch = |apply: &mut dyn FnMut() -> rusqlite::Result<()>| -> Result<(), String> {
        if merged_batch {
            std::thread::sleep(MERGE_YIELD);
            #[cfg(test)]
            tests::between_merge_batches();
        }
        let tx = store.begin_merge().map_err(|e| format!("merge: {e}"))?;
        may_sync(&store, &peer.node_id, epoch)?;
        apply().map_err(|e| format!("merge: {e}"))?;
        tx.commit().map_err(|e| format!("merge commit: {e}"))?;
        merged_batch = true;
        Ok(())
    };
    let mut refused = 0;
    for batch in merge_batches(&theirs.words, |w| w.word.len() + w.updated_by.len()) {
        in_batch(&mut || {
            refused += store.apply_words(batch)?;
            Ok(())
        })?;
    }
    for batch in merge_batches(&theirs.snippets, |s| {
        s.trigger.len() + s.replacement.len() + s.updated_by.len()
    }) {
        in_batch(&mut || {
            refused += store.apply_snippets(batch)?;
            Ok(())
        })?;
    }
    for batch in merge_batches(&theirs.history, |h| {
        h.uuid.len() + h.text.len() + h.raw_text.len()
    }) {
        in_batch(&mut || store.apply_history(batch))?;
    }
    // Merge introductions: strangers they vouch for join only when we
    // marked the sender an introducer.
    let mut introduced = 0;
    for batch in merge_batches(&theirs.peers, |p| {
        p.node_id.len() + p.name.len() + p.addresses.iter().map(|a| a.len() + 3).sum::<usize>()
    }) {
        in_batch(&mut || {
            let sender_introducer = store.get_peer(&peer.node_id)?.is_some_and(|p| p.introducer);
            introduced += store.apply_gossip(batch, sender_introducer, &creds.node_id)?;
            Ok(())
        })?;
    }
    // Advance the mark only after a complete merge: an interrupted or
    // partly refused round is sent again from the previous mark.
    let mark = theirs.mark.take().filter(|_| watermarks() && refused == 0);
    in_batch(&mut || {
        store.touch_peer_synced(&peer.node_id, &peer.name)?;
        match &mark {
            Some(mark) => store.set_peer_mark(&peer.node_id, mark),
            None => Ok(()),
        }
    })?;
    Ok(SyncReport {
        peer_node: peer.node_id,
        peer_name: peer.name,
        words_in: theirs.words.len(),
        snippets_in: theirs.snippets.len(),
        history_in: theirs.history.len(),
        introduced,
    })
}

/// Peers with a dialed round running in this process.
static DIALING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Dial a peer's known addresses (every resolved address of each) in
/// order; first success wins. Runs the Noise handshake, then requires
/// the proven session to belong to `expect_node` before any record
/// moves. A round already running with that peer makes this one a
/// no-op error instead of a duplicate full exchange.
pub fn sync_now(
    addresses: &[String],
    db_path: &Path,
    creds: &SyncCreds,
    name: &str,
    expect_node: &str,
) -> Result<SyncReport, String> {
    if addresses.is_empty() {
        return Err("no known address for peer".to_string());
    }
    struct Dialing(String);
    impl Drop for Dialing {
        fn drop(&mut self) {
            DIALING.lock().unwrap().remove(&self.0);
        }
    }
    if !DIALING.lock().unwrap().insert(expect_node.to_string()) {
        return Err("a round with this peer is already running".to_string());
    }
    let _dialing = Dialing(expect_node.to_string());
    let mut last_err = String::new();
    for address in addresses {
        let resolved = match address.to_socket_addrs() {
            Ok(resolved) => resolved,
            Err(e) => {
                last_err = format!("resolve {address}: {e}");
                continue;
            }
        };
        for addr in resolved {
            let session = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
                .map_err(|e| format!("dial {address}: {e}"))
                .and_then(|stream| NoiseStream::handshake(stream, &creds.noise_static, true));
            let (mut session, hash, _remote_static) = match session {
                Ok(session) => session,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            match exchange(
                &mut session,
                &hash,
                db_path,
                creds,
                name,
                &[],
                true,
                Some(expect_node),
                None,
            ) {
                Err(e) if e == NODE_MISMATCH => last_err = format!("{address}: {e}"),
                result => return result,
            }
        }
    }
    if last_err.is_empty() {
        last_err = "no address for peer".to_string();
    }
    Err(last_err)
}

/// Accept loop: one thread per connection, each with its own DB
/// handle (WAL allows concurrent readers/writers across connections).
pub fn serve(
    listener: TcpListener,
    db_path: PathBuf,
    creds: SyncCreds,
    name: String,
    stop: &AtomicBool,
) {
    let _ = listener.set_nonblocking(true);
    let active = Arc::new(AtomicUsize::new(0));
    /// Frees one inbound slot when its connection thread ends.
    struct Slot(Arc<AtomicUsize>);
    impl Drop for Slot {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    for conn in listener.incoming() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match conn {
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Ok(stream) => {
                let remote = stream
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| String::new());
                if active.fetch_add(1, Ordering::SeqCst) >= MAX_INBOUND {
                    active.fetch_sub(1, Ordering::SeqCst);
                    eprintln!("[sync] {remote}: busy, connection dropped");
                    continue;
                }
                let slot = Slot(active.clone());
                // The address the dialer reached: current after a network change.
                let addresses: Vec<String> = stream
                    .local_addr()
                    .map(|a| a.to_string())
                    .into_iter()
                    .collect();
                let remote = if remote.is_empty() {
                    None
                } else {
                    Some(remote)
                };
                let db_path = db_path.clone();
                let creds = creds.clone();
                let name = name.clone();
                std::thread::spawn(move || {
                    let _slot = slot;
                    let res = (|| -> Result<SyncReport, String> {
                        let (mut session, hash, _remote_static) =
                            NoiseStream::handshake(stream, &creds.noise_static, false)?;
                        exchange(
                            &mut session,
                            &hash,
                            &db_path,
                            &creds,
                            &name,
                            &addresses,
                            false,
                            None,
                            remote.clone(),
                        )
                    })();
                    match res {
                        Ok(report) => eprintln!(
                            "[sync] served {} ({} words in, {} introduced)",
                            report.peer_node, report.words_in, report.introduced
                        ),
                        Err(e) => {
                            eprintln!("[sync] {}: {e}", remote.unwrap_or_else(|| "?".to_string()))
                        }
                    }
                });
            }
            Err(e) => eprintln!("[sync] accept: {e}"),
        }
    }
}

/// Bind the sync listener. Port 0 disables listening.
pub fn listen(port: u16) -> std::io::Result<Option<TcpListener>> {
    if port == 0 {
        return Ok(None);
    }
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    Ok(Some(listener))
}

/// Best-effort LAN address for advertisements (UDP trick: no traffic
/// is actually sent).
pub fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}

/// Dial our own sync port through our LAN address, the way a peer
/// would. Used for the reachability indicator.
pub fn self_reachable(port: u16) -> Option<String> {
    let ip = lan_ip()?;
    let addr = format!("{ip}:{port}");
    let sock = addr.to_socket_addrs().ok()?.next()?;
    match TcpStream::connect_timeout(&sock, CONNECT_TIMEOUT) {
        Ok(_) => Some(addr),
        Err(_) => None,
    }
}

/// Allow-command hint for the installed firewall, if any (pure
/// presence check, no privileges, no queries: nothing here can pop
/// polkit). The Check button prints it next to the dial result so
/// the user opens the port themselves, whatever they run.
pub fn firewall_allow_hint(port: u16) -> Option<String> {
    fn present(bin: &str) -> bool {
        std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file()))
            == Some(true)
    }
    if present("firewall-cmd") {
        Some(format!(
            "sudo firewall-cmd --add-port={port}/tcp --permanent && sudo firewall-cmd --reload"
        ))
    } else if present("ufw") {
        Some(format!("sudo ufw allow {port}/tcp"))
    } else {
        None
    }
}

// -- mDNS ----------------------------------------------------------------

pub struct MdnsHandles {
    _daemon: mdns_sd::ServiceDaemon,
}

impl MdnsHandles {
    /// Unregister + stop the daemon (browser recv ends, threads exit).
    pub fn shutdown(&self) {
        let _ = self._daemon.shutdown();
    }
}

/// Advertise this node on the LAN and browse for peers in the
/// background. Discovered nodes land in `sink` (deduplicated by node
/// id); the caller polls it to refresh the UI.
pub fn start_mdns(
    node_id: &str,
    name: &str,
    port: u16,
    sink: Arc<Mutex<HashMap<String, Discovered>>>,
    advertise: bool,
) -> Result<MdnsHandles, String> {
    let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| format!("mdns: {e}"))?;
    if advertise {
        let short_id = &node_id[..node_id.len().min(8)];
        let mut txt = std::collections::HashMap::new();
        txt.insert("node".to_string(), node_id.to_string());
        txt.insert("name".to_string(), name.to_string());
        let host = format!("mywhisper-{short_id}.local.");
        // The daemon tracks interface addresses, so a DHCP or network
        // change re-advertises the new address.
        let info = mdns_sd::ServiceInfo::new(MDNS_SERVICE, short_id, &host, (), port, Some(txt))
            .map_err(|e| format!("mdns service: {e}"))?
            .enable_addr_auto();
        daemon
            .register(info)
            .map_err(|e| format!("mdns register: {e}"))?;
    }

    let browser = daemon
        .browse(MDNS_SERVICE)
        .map_err(|e| format!("mdns browse: {e}"))?;
    let self_id = node_id.to_string();
    std::thread::spawn(move || {
        // Service instance name -> node id, to forget removed services.
        let mut instances: HashMap<String, String> = HashMap::new();
        while let Ok(event) = browser.recv() {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                    let node = info
                        .get_property_val_str("node")
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    // Unauthenticated LAN input: only canonical ids get in.
                    if node == self_id || !valid_node_id(&node) {
                        continue;
                    }
                    let name = info
                        .get_property_val_str("name")
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    let port = info.get_port();
                    let addresses: Vec<String> = info
                        .get_addresses()
                        .iter()
                        .map(|ip| dial_address(ip, port))
                        .collect();
                    if addresses.is_empty() {
                        continue;
                    }
                    eprintln!("[sync] mdns found {name} {addresses:?}");
                    instances.insert(info.get_fullname().to_string(), node.clone());
                    sink.lock().unwrap().insert(
                        node.clone(),
                        Discovered {
                            node_id: node,
                            name,
                            addresses,
                        },
                    );
                }
                mdns_sd::ServiceEvent::ServiceRemoved(_, full_name) => {
                    eprintln!("[sync] mdns lost {full_name}");
                    // Forget it, so its return counts as a new appearance.
                    if let Some(node) = instances.remove(&full_name) {
                        sink.lock().unwrap().remove(&node);
                    }
                }
                _ => {}
            }
        }
    });
    Ok(MdnsHandles { _daemon: daemon })
}

/// `ip:port` a dial can parse: IPv6 gets brackets and its numeric scope.
fn dial_address(ip: &mdns_sd::ScopedIp, port: u16) -> String {
    match ip {
        mdns_sd::ScopedIp::V6(v6) => {
            SocketAddrV6::new(*v6.addr(), port, 0, v6.scope_id().index).to_string()
        }
        other => SocketAddr::new(other.to_ip_addr(), port).to_string(),
    }
}

// -- periodic and triggered rounds ------------------------------------------

/// One best-effort round with every trusted peer that has a known
/// address. Returns per-peer outcome lines for logging. Unreachable
/// peers are simply skipped; the next round (or the next local
/// change) retries them.
pub fn sync_all_trusted(db_path: &Path, creds: &SyncCreds) -> Vec<String> {
    let store = match Store::open(db_path) {
        Ok(s) => s,
        Err(e) => return vec![format!("open db: {e}")],
    };
    if !store.sync_enabled() {
        return vec!["sync is off".to_string()];
    }
    let settings = store.load_settings();
    let name = if settings.sync.device_name.is_empty() {
        creds.node_id.clone()
    } else {
        settings.sync.device_name.clone()
    };
    let peers: Vec<_> = store
        .list_peers()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.trusted && !p.dial_targets().is_empty())
        .collect();
    let mut out = Vec::new();
    for peer in peers {
        match sync_now(&peer.dial_targets(), db_path, creds, &name, &peer.node_id) {
            Ok(r) => out.push(format!(
                "synced {} ({} words in)",
                short_id(&r.peer_node),
                r.words_in
            )),
            Err(e) => out.push(format!("{} unreachable ({e})", short_id(&peer.node_id))),
        }
    }
    out
}

/// Every minute, sync with each trusted peer that has a known
/// address. Failures just log; the next round retries.
pub fn auto_sync_loop(db_path: PathBuf, creds: SyncCreds, stop: Arc<AtomicBool>) {
    for _ in 0..AUTO_SYNC_SECS {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        for line in sync_all_trusted(&db_path, &creds) {
            eprintln!("[sync] auto: {line}");
        }
        for _ in 0..AUTO_SYNC_SECS {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

/// Fire-and-forget sync round after a local change (dictionary edit,
/// rename, …). Same best-effort semantics as the periodic loop.
pub fn sync_after_change(db_path: PathBuf, creds: SyncCreds) {
    std::thread::spawn(move || {
        for line in sync_all_trusted(&db_path, &creds) {
            eprintln!("[sync] on-change: {line}");
        }
    });
}

/// One catch-up round shortly after launch, so a fresh start converges
/// without waiting for the periodic loop.
pub fn sync_on_startup(db_path: PathBuf, creds: SyncCreds) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        for line in sync_all_trusted(&db_path, &creds) {
            eprintln!("[sync] startup: {line}");
        }
    });
}

/// Watch mDNS discovery: whenever a *trusted* peer newly appears on
/// the LAN, sync immediately instead of waiting for the loop.
pub fn watch_discovered(
    db_path: PathBuf,
    creds: SyncCreds,
    discovered: Arc<Mutex<HashMap<String, Discovered>>>,
    stop: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut present = HashSet::new();
        loop {
            for _ in 0..10 {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            let Ok(store) = Store::open(&db_path) else {
                continue;
            };
            let snapshot = discovered.lock().unwrap().clone();
            if note_discovered(&store, &snapshot, &mut present) {
                eprintln!("[sync] peer appeared, syncing now");
                for line in sync_all_trusted(&db_path, &creds) {
                    eprintln!("[sync] on-appear: {line}");
                }
            }
        }
    });
}

/// One appear-watcher pass: store the addresses discovery found for
/// trusted peers (a DHCP move needs no open GUI), and report whether
/// one appeared since the last pass (`present`).
fn note_discovered(
    store: &Store,
    discovered: &HashMap<String, Discovered>,
    present: &mut HashSet<String>,
) -> bool {
    let mut now_present = HashSet::new();
    for peer in store.list_peers().unwrap_or_default() {
        let Some(found) = discovered.get(&peer.node_id).filter(|_| peer.trusted) else {
            continue;
        };
        let merged = crate::store::merge_addresses(&peer.addresses, &found.addresses);
        if merged != peer.addresses {
            let _ = store.set_peer_addresses(&peer.node_id, &merged);
        }
        now_present.insert(peer.node_id);
    }
    let appeared = now_present.iter().any(|id| !present.contains(id));
    *present = now_present;
    appeared
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceIdentity;
    use crate::store::Store;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    thread_local! {
        /// Runs in `exchange` after export, before each frame of our payload.
        static BEFORE_SEND: std::cell::RefCell<Option<Box<dyn Fn()>>> =
            const { std::cell::RefCell::new(None) };
        static BETWEEN_MERGE_BATCHES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
        /// This thread's side of a round behaves like an older build.
        static OLD_BUILD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    pub(super) fn playing_old_build() -> bool {
        OLD_BUILD.with(|old| old.get())
    }

    pub(super) fn before_send() {
        BEFORE_SEND.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook()
            }
        });
    }

    fn set_before_send(hook: Option<Box<dyn Fn()>>) {
        BEFORE_SEND.with(|slot| *slot.borrow_mut() = hook);
    }

    pub(super) fn between_merge_batches() {
        if let Some(hook) = BETWEEN_MERGE_BATCHES.with(|slot| slot.take()) {
            hook();
        }
    }

    fn temp_db(tag: &str) -> PathBuf {
        let n = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        // Sandboxed runs reuse the same pid, so add a per-run nonce or a
        // later run reopens an earlier run's database.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("mywhisper-sync-test-{pid}-{nonce}-{n}-{tag}.db"))
    }

    /// Full client/server round-trip over real loopback TCP with Noise
    /// encryption: B dials A, both sides converge, untrusted dialers
    /// are rejected.
    #[test]
    fn loopback_sync_converges_and_rejects_untrusted() {
        let id_a = DeviceIdentity::ephemeral();
        let id_b = DeviceIdentity::ephemeral();
        let db_a = temp_db("a");
        let db_b = temp_db("b");
        let store_a = Store::open(&db_a).unwrap();
        let store_b = Store::open(&db_b).unwrap();

        // A trusts B; B trusts A.
        store_a
            .upsert_peer(&id_b.node_id, "b", "unused", true)
            .unwrap();
        store_b
            .upsert_peer(&id_a.node_id, "a", "unused", true)
            .unwrap();

        // Divergent content on both sides.
        store_a.upsert_word("from-a", &id_a.node_id).unwrap();
        store_b.upsert_word("from-b", &id_b.node_id).unwrap();
        store_a
            .upsert_snippet("ta", "A says", &id_a.node_id)
            .unwrap();
        store_b
            .upsert_snippet("tb", "B says", &id_b.node_id)
            .unwrap();

        // A serves on an ephemeral port.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let db_a2 = db_a.clone();
        let creds_a = id_a.creds();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let (mut session, hash, _remote) =
                NoiseStream::handshake(stream, &creds_a.noise_static, false).unwrap();
            exchange(
                &mut session,
                &hash,
                &db_a2,
                &creds_a,
                "a",
                &[],
                false,
                None,
                None,
            )
            .unwrap();
            // Serve exactly one round for the test.
        });

        let report = sync_now(
            &[format!("127.0.0.1:{port}")],
            &db_b,
            &id_b.creds(),
            "b",
            &id_a.node_id,
        )
        .unwrap();
        server.join().unwrap();

        assert_eq!(report.peer_node, id_a.node_id);
        assert!(report.words_in >= 1);

        // Both sides converged.
        let mut wa: Vec<_> = store_a.load_words().unwrap();
        let mut wb: Vec<_> = store_b.load_words().unwrap();
        wa.sort();
        wb.sort();
        assert_eq!(wa, wb);
        assert!(wa.contains(&"from-a".to_string()));
        assert!(wa.contains(&"from-b".to_string()));
        let ta = store_a.load_snippets().unwrap();
        assert_eq!(ta.len(), 2);

        // Untrusted dialer is rejected: fresh node C, trusted nowhere.
        let id_c = DeviceIdentity::ephemeral();
        let db_c = temp_db("c");
        Store::open(&db_c).unwrap();
        let listener2 = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port2 = listener2.local_addr().unwrap().port();
        let db_a3 = db_a.clone();
        let creds_a2 = id_a.creds();
        let server2 = std::thread::spawn(move || {
            let (stream, _) = listener2.accept().unwrap();
            let (mut session, hash, _remote) =
                NoiseStream::handshake(stream, &creds_a2.noise_static, false).unwrap();
            exchange(
                &mut session,
                &hash,
                &db_a3,
                &creds_a2,
                "a",
                &[],
                false,
                None,
                None,
            )
        });
        let err = sync_now(
            &[format!("127.0.0.1:{port2}")],
            &db_c,
            &id_c.creds(),
            "c",
            &id_a.node_id,
        )
        .unwrap_err();
        let server_err = server2.join().unwrap().unwrap_err();
        assert!(
            err.contains("not trusted") || server_err.contains("not trusted"),
            "client err: {err}, server err: {server_err}"
        );
        // The rejected node left a pending request behind.
        let pending = store_a.list_pending().unwrap();
        assert!(pending.iter().any(|p| p.node_id == id_c.node_id));

        std::fs::remove_file(&db_a).ok();
        std::fs::remove_file(&db_b).ok();
        std::fs::remove_file(&db_c).ok();
    }

    fn serve_one(
        listener: TcpListener,
        db: PathBuf,
        creds: SyncCreds,
    ) -> std::thread::JoinHandle<Result<SyncReport, String>> {
        serve_one_hooked(listener, db, creds, || {})
    }

    /// `serve_one` with a `before_send` hook on the serving thread.
    fn serve_one_hooked(
        listener: TcpListener,
        db: PathBuf,
        creds: SyncCreds,
        hook: impl Fn() + Send + 'static,
    ) -> std::thread::JoinHandle<Result<SyncReport, String>> {
        std::thread::spawn(move || {
            set_before_send(Some(Box::new(hook)));
            let (stream, _) = listener.accept().unwrap();
            let (mut session, hash, _remote) =
                NoiseStream::handshake(stream, &creds.noise_static, false)?;
            exchange(
                &mut session,
                &hash,
                &db,
                &creds,
                "srv",
                &[],
                false,
                None,
                None,
            )
        })
    }

    fn disable_sync(store: &Store) {
        let mut settings = store.load_settings();
        settings.sync.enabled = false;
        store.save_settings(&settings).unwrap();
    }

    fn history_texts(store: &Store) -> Vec<String> {
        store
            .load_history(10, true)
            .unwrap()
            .into_iter()
            .map(|h| h.text)
            .collect()
    }

    /// Two mutually trusting nodes with one transcript each.
    fn paired() -> (DeviceIdentity, DeviceIdentity, PathBuf, PathBuf) {
        let id_a = DeviceIdentity::ephemeral();
        let id_b = DeviceIdentity::ephemeral();
        let db_a = temp_db("pa");
        let db_b = temp_db("pb");
        let store_a = Store::open(&db_a).unwrap();
        let store_b = Store::open(&db_b).unwrap();
        store_a.upsert_peer(&id_b.node_id, "b", "", true).unwrap();
        store_b.upsert_peer(&id_a.node_id, "a", "", true).unwrap();
        store_a
            .add_history("a", "from-a", 1, 1, &id_a.node_id)
            .unwrap();
        store_b
            .add_history("b", "from-b", 1, 1, &id_b.node_id)
            .unwrap();
        (id_a, id_b, db_a, db_b)
    }

    #[test]
    fn sync_off_moves_no_records_in_either_direction() {
        let (id_a, id_b, db_a, db_b) = paired();
        let store_a = Store::open(&db_a).unwrap();
        let store_b = Store::open(&db_b).unwrap();
        disable_sync(&store_a);

        // Incoming: B dials A while A has sync off.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let server = serve_one(listener, db_a.clone(), id_a.creds());
        let err = sync_now(&[addr], &db_b, &id_b.creds(), "b", &id_a.node_id).unwrap_err();
        assert!(err.contains("sync is off"), "client err: {err}");
        assert!(server.join().unwrap().is_err());

        // Outgoing: A dials B while A has sync off.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let server = serve_one(listener, db_b.clone(), id_b.creds());
        let err = sync_now(&[addr], &db_a, &id_a.creds(), "a", &id_b.node_id).unwrap_err();
        assert!(err.contains("sync is off"), "client err: {err}");
        assert!(server.join().unwrap().is_err());

        // Background rounds do not even dial.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        store_a.set_peer_addresses(&id_b.node_id, &[addr]).unwrap();
        sync_all_trusted(&db_a, &id_a.creds());
        assert!(listener.accept().is_err());

        assert_eq!(history_texts(&store_a), vec!["from-a".to_string()]);
        assert_eq!(history_texts(&store_b), vec!["from-b".to_string()]);
        std::fs::remove_file(&db_a).ok();
        std::fs::remove_file(&db_b).ok();
    }

    #[test]
    fn dialer_sends_nothing_to_a_peer_it_does_not_trust() {
        let (id_a, id_b, db_a, db_b) = paired();
        let store_a = Store::open(&db_a).unwrap();
        let store_b = Store::open(&db_b).unwrap();
        // B still trusts A, but A revoked B.
        store_a.upsert_peer(&id_b.node_id, "b", "", false).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let server = serve_one(listener, db_b.clone(), id_b.creds());
        let err = sync_now(&[addr], &db_a, &id_a.creds(), "a", &id_b.node_id).unwrap_err();
        assert!(err.contains("not trusted"), "client err: {err}");
        assert!(server.join().unwrap().is_err());
        assert_eq!(history_texts(&store_a), vec!["from-a".to_string()]);
        assert_eq!(history_texts(&store_b), vec!["from-b".to_string()]);
        std::fs::remove_file(&db_a).ok();
        std::fs::remove_file(&db_b).ok();
    }

    fn reset_then_reenable(store: &Store) {
        let mut fresh = crate::settings::AppSettings::default();
        fresh.sync.enabled = false;
        store.reset_all_data(&fresh).unwrap();
        fresh.sync.enabled = true;
        store.save_settings(&fresh).unwrap();
    }

    #[test]
    fn permission_withdrawn_mid_round_sends_and_merges_nothing() {
        // Responder A turns sync off between its export and its send.
        let (id_a, id_b, db_a, db_b) = paired();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let hook_db = db_a.clone();
        let server = serve_one_hooked(listener, db_a.clone(), id_a.creds(), move || {
            disable_sync(&Store::open(&hook_db).unwrap())
        });
        assert!(sync_now(&[addr], &db_b, &id_b.creds(), "b", &id_a.node_id).is_err());
        let err = server.join().unwrap().unwrap_err();
        assert!(err.contains("sync is off"), "server err: {err}");
        assert_eq!(history_texts(&Store::open(&db_a).unwrap()), vec!["from-a"]);
        assert_eq!(history_texts(&Store::open(&db_b).unwrap()), vec!["from-b"]);

        // Dialer A revokes B between its export and its send.
        let (id_a, id_b, db_a, db_b) = paired();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let server = serve_one(listener, db_b.clone(), id_b.creds());
        let (hook_db, b_node) = (db_a.clone(), id_b.node_id.clone());
        set_before_send(Some(Box::new(move || {
            Store::open(&hook_db)
                .unwrap()
                .upsert_peer(&b_node, "b", "", false)
                .unwrap()
        })));
        let err = sync_now(&[addr], &db_a, &id_a.creds(), "a", &id_b.node_id).unwrap_err();
        set_before_send(None);
        assert!(err.contains("not trusted"), "client err: {err}");
        assert!(server.join().unwrap().is_err());
        assert_eq!(history_texts(&Store::open(&db_a).unwrap()), vec!["from-a"]);
        assert_eq!(history_texts(&Store::open(&db_b).unwrap()), vec!["from-b"]);

        // Responder A resets and turns sync back on before its send:
        // permission holds again, but the pre-reset export must not go.
        let (id_a, id_b, db_a, db_b) = paired();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let hook_db = db_a.clone();
        let server = serve_one_hooked(listener, db_a.clone(), id_a.creds(), move || {
            reset_then_reenable(&Store::open(&hook_db).unwrap())
        });
        assert!(sync_now(&[addr], &db_b, &id_b.creds(), "b", &id_a.node_id).is_err());
        let err = server.join().unwrap().unwrap_err();
        assert!(
            err.contains("changed during the round"),
            "server err: {err}"
        );
        assert!(history_texts(&Store::open(&db_a).unwrap()).is_empty());
        assert_eq!(history_texts(&Store::open(&db_b).unwrap()), vec!["from-b"]);
    }

    fn add_bulk_history(db: &Path, count: usize, bytes: usize) {
        let store = Store::open(db).unwrap();
        let rows: Vec<HistoryRecord> = (0..count)
            .map(|i| HistoryRecord {
                uuid: format!("bulk-{i}"),
                text: "x".repeat(bytes),
                raw_text: String::new(),
                words: 1,
                duration_secs: 1,
                deleted: false,
                discarded: false,
                recorded_at: 0,
            })
            .collect();
        let tx = store.begin_merge().unwrap();
        store.apply_history(&rows).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn permission_withdrawn_mid_payload_stops_the_send() {
        // A turns sync off after encryption of the final frame, before
        // its bytes reach the socket. B must reject the partial payload.
        let (id_a, id_b, db_a, db_b) = paired();
        add_bulk_history(&db_a, 1, CHUNK);
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (hook_db, frames) = (db_a.clone(), Arc::new(AtomicUsize::new(0)));
        let server = serve_one_hooked(listener, db_a.clone(), id_a.creds(), move || {
            if frames.fetch_add(1, Ordering::SeqCst) == 2 {
                disable_sync(&Store::open(&hook_db).unwrap())
            }
        });
        assert!(sync_now(&[addr], &db_b, &id_b.creds(), "b", &id_a.node_id).is_err());
        let err = server.join().unwrap().unwrap_err();
        assert!(err.contains("sync is off"), "server err: {err}");
        assert_eq!(history_texts(&Store::open(&db_b).unwrap()), vec!["from-b"]);
        let a = Store::open(&db_a).unwrap().load_history(100, true).unwrap();
        assert!(a.iter().all(|h| h.text != "from-b"));
        std::fs::remove_file(&db_a).ok();
        std::fs::remove_file(&db_b).ok();
    }

    #[test]
    fn merge_rechecks_permission_after_releasing_the_write_lock() {
        let (id_a, id_b, db_a, db_b) = paired();
        add_bulk_history(&db_a, MERGE_BATCH_ROWS + 1, 1);
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let server = serve_one(listener, db_a.clone(), id_a.creds());
        let (local_db, local_id) = (db_b.clone(), id_b.node_id.clone());
        BETWEEN_MERGE_BATCHES.with(|slot| {
            slot.replace(Some(Box::new(move || {
                let writer = Store::open(&local_db).unwrap();
                writer
                    .add_history("local", "local", 1, 1, &local_id)
                    .unwrap();
                disable_sync(&writer);
                writer
                    .save_settings(&crate::settings::AppSettings::default())
                    .unwrap();
            })));
        });
        let err = sync_now(&[addr], &db_b, &id_b.creds(), "b", &id_a.node_id).unwrap_err();
        assert!(err.contains("changed during the round"), "{err}");
        server.join().unwrap().unwrap();
        let store = Store::open(&db_b).unwrap();
        let history = store.load_history(MERGE_BATCH_ROWS + 4, true).unwrap();
        assert_eq!(history.len(), MERGE_BATCH_ROWS + 2);
        assert!(history.iter().any(|h| h.text == "local"));
        assert_eq!(store.get_peer(&id_a.node_id).unwrap().unwrap().last_sync, 0);
        std::fs::remove_file(&db_a).ok();
        std::fs::remove_file(&db_b).ok();
    }

    #[test]
    fn merge_batches_respect_row_and_byte_bounds() {
        let sizes = |rows: &[usize]| {
            merge_batches(rows, |r| *r)
                .into_iter()
                .map(<[_]>::len)
                .collect::<Vec<_>>()
        };
        assert_eq!(sizes(&[10; 450]), [200, 200, 50]);
        assert_eq!(sizes(&[200 * 1024, 200 * 1024, 1024 * 1024, 1]), [1; 4]);
        assert!(sizes(&[]).is_empty());
    }

    /// Dial `listener`'s server as an unknown node and finish the Noise
    /// handshake, leaving the session pre-authentication.
    fn unknown_session(port: u16) -> NoiseStream {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let stranger = DeviceIdentity::ephemeral();
        NoiseStream::handshake(stream, &stranger.creds().noise_static, true)
            .unwrap()
            .0
    }

    #[test]
    fn unauthenticated_peers_cannot_send_large_or_empty_messages() {
        let id_a = DeviceIdentity::ephemeral();
        // Never created: the responder must not open its DB before proof.
        let db_a = temp_db("budget");

        // A raw frame header over the Noise limit is refused before
        // the server allocates or reads the body.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = serve_one(listener, db_a.clone(), id_a.creds());
        let mut session = unknown_session(port);
        session
            .stream
            .write_all(&(1024u32 * 1024).to_be_bytes())
            .unwrap();
        let err = server.join().unwrap().unwrap_err();
        assert!(err.contains("bad frame length"), "server err: {err}");

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = serve_one(listener, db_a.clone(), id_a.creds());
        let mut session = unknown_session(port);
        // Header announces 1 MiB; the server refuses before reading it.
        session
            .write_chunk(&(1024u64 * 1024).to_be_bytes(), None)
            .unwrap();
        let err = server.join().unwrap().unwrap_err();
        assert!(err.contains("too large"), "server err: {err}");

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = serve_one(listener, db_a.clone(), id_a.creds());
        let mut session = unknown_session(port);
        session.write_chunk(&10u64.to_be_bytes(), None).unwrap();
        for _ in 0..3 {
            let _ = session.write_chunk(&[], None);
        }
        let err = server.join().unwrap().unwrap_err();
        assert!(err.contains("empty chunk"), "server err: {err}");
        assert!(!db_a.exists(), "DB opened before identity proof");
    }

    #[test]
    fn trusted_sessions_keep_a_deadline_sized_to_each_message() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let key = DeviceIdentity::ephemeral().creds().noise_static;
            let mut session = NoiseStream::handshake(stream, &key, false).unwrap().0;
            session.authenticated();
            session.recv_msg().unwrap().len()
        });
        let mut session = unknown_session(port);
        session.authenticated();
        let floor = Instant::now() + POSTAUTH_BASE;
        assert!(session.deadline >= floor - Duration::from_secs(1));
        assert!(session.deadline <= floor + Duration::from_secs(1));
        let big = vec![7u8; 4 * 1024 * 1024];
        let sending = Instant::now();
        session.send_msg(&big).unwrap();
        // 4 MiB at the minimum rate adds 16 s to the floor.
        let sized = sending + POSTAUTH_BASE + Duration::from_secs(16);
        assert!(session.deadline >= sized - Duration::from_secs(1));
        assert!(session.deadline <= sized + Duration::from_secs(5));
        assert_eq!(server.join().unwrap(), big.len());
    }

    #[test]
    fn listener_caps_concurrent_connections() {
        let id_a = DeviceIdentity::ephemeral();
        let db_a = temp_db("cap");
        Store::open(&db_a).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let server = {
            let stop = stop.clone();
            let creds = id_a.creds();
            let db_a = db_a.clone();
            std::thread::spawn(move || serve(listener, db_a, creds, "a".to_string(), &stop))
        };
        // Idle sockets that never finish a handshake hold every slot.
        let idle: Vec<TcpStream> = (0..MAX_INBOUND)
            .map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap())
            .collect();
        // Accepts are FIFO, so the extra one arrives with every slot taken.
        let mut extra = TcpStream::connect(("127.0.0.1", port)).unwrap();
        extra
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut byte = [0u8; 1];
        assert_eq!(extra.read(&mut byte).unwrap(), 0, "over-budget conn closed");
        stop.store(true, Ordering::Relaxed);
        drop(idle);
        server.join().unwrap();
        std::fs::remove_file(&db_a).ok();
    }

    /// An attacker with their own valid keypair who claims someone
    /// else's node id in hello is rejected: the session proof cannot
    /// verify against the victim's key.
    #[test]
    fn spoofed_node_id_rejected() {
        let id_victim = DeviceIdentity::ephemeral();
        let id_attacker = DeviceIdentity::ephemeral();
        let db_v = temp_db("v");
        let store_v = Store::open(&db_v).unwrap();
        // Victim trusts... nobody relevant; attacker dials claiming
        // to BE the victim's trusted peer would need that peer's key.
        // Here the attacker claims victim's own id to a fresh server
        // that trusts the victim id: handshake succeeds (valid keys
        // on both sides) but the hello proof fails.
        store_v
            .upsert_peer(&id_victim.node_id, "victim", "unused", true)
            .unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let db_v2 = db_v.clone();
        let _ = id_victim.node_id.clone();
        let creds_vk = id_victim.creds();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let (mut session, hash, _remote) =
                NoiseStream::handshake(stream, &creds_vk.noise_static, false).unwrap();
            exchange(
                &mut session,
                &hash,
                &db_v2,
                &creds_vk,
                "v",
                &[],
                false,
                None,
                None,
            )
        });

        // Attacker crafts a hello claiming the victim id, but can only
        // sign with their own key: build the exchange manually with a
        // forged hello over a valid attacker handshake.
        let db_x = temp_db("x");
        Store::open(&db_x).unwrap();
        let stream = TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}")
                .to_socket_addrs()
                .unwrap()
                .next()
                .unwrap(),
            Duration::from_secs(10),
        )
        .unwrap();
        let (mut session, hash, _remote) =
            NoiseStream::handshake(stream, &id_attacker.creds().noise_static, true).unwrap();
        // Forge: valid framing, victim's id, attacker's signature.
        let forged = Hello {
            v: PROTOCOL_V,
            node_id: id_victim.node_id.clone(),
            name: "attacker".to_string(),
            addresses: vec![],
            introducer: false,
            sig: DeviceIdentity::sign_session_static(&id_attacker.creds().signing, &hash).to_vec(),
        };
        session.send_json(&forged).unwrap();
        let _peer_hello: Hello = session.recv_json().unwrap();
        let verdict: Verdict = session.recv_json().unwrap();
        assert!(!verdict.ok, "spoofed hello must be rejected");
        drop(session);
        let server_err = server.join().unwrap().unwrap_err();
        assert!(
            server_err.contains("identity proof failed"),
            "server err: {server_err}"
        );

        std::fs::remove_file(&db_v).ok();
        std::fs::remove_file(&db_x).ok();
    }

    /// One loopback round: B dials A. Returns (B's report, A's report).
    fn round(
        id_a: &DeviceIdentity,
        id_b: &DeviceIdentity,
        db_a: &Path,
        db_b: &Path,
    ) -> (SyncReport, SyncReport) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let server = serve_one(listener, db_a.to_path_buf(), id_a.creds());
        let client = sync_now(&[addr], db_b, &id_b.creds(), "b", &id_a.node_id).unwrap();
        (client, server.join().unwrap().unwrap())
    }

    /// R44: once two peers converged, a round carries only what changed.
    #[test]
    fn later_rounds_send_only_changes() {
        let (id_a, id_b, db_a, db_b) = paired();
        add_bulk_history(&db_a, 50, 10);
        let (b, a) = round(&id_a, &id_b, &db_a, &db_b);
        assert_eq!((b.history_in, a.history_in), (51, 1));
        // The next round still echoes what each side just merged.
        round(&id_a, &id_b, &db_a, &db_b);
        let (b, a) = round(&id_a, &id_b, &db_a, &db_b);
        assert_eq!(
            (b.history_in, a.history_in, b.words_in, a.words_in),
            (0, 0, 0, 0)
        );
        Store::open(&db_a)
            .unwrap()
            .upsert_word("fresh", &id_a.node_id)
            .unwrap();
        Store::open(&db_a)
            .unwrap()
            .delete_history("bulk-3")
            .unwrap();
        let (b, _) = round(&id_a, &id_b, &db_a, &db_b);
        assert_eq!((b.words_in, b.history_in), (1, 1));
        let store_b = Store::open(&db_b).unwrap();
        assert_eq!(store_b.load_words().unwrap(), vec!["fresh".to_string()]);
        assert_eq!(store_b.load_history(100, true).unwrap().len(), 51);
        std::fs::remove_file(&db_a).ok();
        std::fs::remove_file(&db_b).ok();
    }

    /// R42: a stale or unparsable first address, or one that now
    /// belongs to another device, must not stop the dial.
    #[test]
    fn dialing_falls_through_bad_addresses() {
        let (id_a, id_b, db_a, db_b) = paired();
        let hang_up = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let hang_up_addr = hang_up.local_addr().unwrap().to_string();
        let closer = std::thread::spawn(move || drop(hang_up.accept().unwrap()));
        let id_c = DeviceIdentity::ephemeral();
        let db_c = temp_db("other");
        Store::open(&db_c).unwrap();
        let other = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let other_addr = other.local_addr().unwrap().to_string();
        let other = serve_one(other, db_c.clone(), id_c.creds());
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = serve_one(listener, db_a.clone(), id_a.creds());
        let addresses = ["bogus".to_string(), hang_up_addr, other_addr, addr];
        sync_now(&addresses, &db_b, &id_b.creds(), "b", &id_a.node_id).unwrap();
        server.join().unwrap().unwrap();
        closer.join().unwrap();
        assert!(other.join().unwrap().is_err());
        assert!(history_texts(&Store::open(&db_b).unwrap()).contains(&"from-a".to_string()));
        for db in [db_a, db_b, db_c] {
            std::fs::remove_file(db).ok();
        }
    }

    /// `serve_one` for a responder that plays an older build.
    fn serve_one_as_old(
        listener: TcpListener,
        db: PathBuf,
        creds: SyncCreds,
    ) -> std::thread::JoinHandle<Result<SyncReport, String>> {
        std::thread::spawn(move || {
            OLD_BUILD.with(|old| old.set(true));
            let (stream, _) = listener.accept().unwrap();
            let (mut session, hash, _remote) =
                NoiseStream::handshake(stream, &creds.noise_static, false)?;
            exchange(
                &mut session,
                &hash,
                &db,
                &creds,
                "old",
                &[],
                false,
                None,
                None,
            )
        })
    }

    /// Older builds neither send nor use marks: both directions still
    /// converge, and this build keeps sending them everything.
    #[test]
    fn older_builds_still_sync_both_ways() {
        let (id_a, id_b, db_a, db_b) = paired();
        // New dialer B, old responder A.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = serve_one_as_old(listener, db_a.clone(), id_a.creds());
        sync_now(&[addr], &db_b, &id_b.creds(), "b", &id_a.node_id).unwrap();
        assert_eq!(server.join().unwrap().unwrap().history_in, 1);
        // Old dialer A, new responder B, twice: still full state.
        OLD_BUILD.with(|old| old.set(true));
        for _ in 0..2 {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let server = serve_one(listener, db_b.clone(), id_b.creds());
            let report = sync_now(&[addr], &db_a, &id_a.creds(), "a", &id_b.node_id).unwrap();
            assert_eq!(report.history_in, 2);
            server.join().unwrap().unwrap();
        }
        OLD_BUILD.with(|old| old.set(false));
        let (store_a, store_b) = (Store::open(&db_a).unwrap(), Store::open(&db_b).unwrap());
        assert_eq!(store_b.peer_mark(&id_a.node_id).unwrap(), None);
        for store in [&store_a, &store_b] {
            let mut texts = history_texts(store);
            texts.sort();
            assert_eq!(texts, vec!["from-a".to_string(), "from-b".to_string()]);
        }
        std::fs::remove_file(&db_a).ok();
        std::fs::remove_file(&db_b).ok();
    }

    /// Messages from this build decode with the structs of the previous
    /// release (copied here), which ignore unknown fields.
    #[test]
    fn new_messages_decode_as_old_ones() {
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldVerdict {
            #[serde(default)]
            ok: bool,
            #[serde(default)]
            error: String,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldPayload {
            #[serde(default)]
            words: Vec<WordRecord>,
            #[serde(default)]
            snippets: Vec<SnippetRecord>,
            #[serde(default)]
            history: Vec<HistoryRecord>,
            #[serde(default)]
            peers: Vec<PeerGossip>,
        }
        let mark = SyncMark {
            store: "ab".into(),
            seq: 3,
        };
        let verdict = serde_json::to_vec(&Verdict {
            ok: true,
            error: String::new(),
            since: Some(mark.clone()),
        })
        .unwrap();
        assert!(serde_json::from_slice::<OldVerdict>(&verdict).unwrap().ok);
        let payload = serde_json::to_vec(&Payload {
            since: Some(mark.clone()),
            mark: Some(mark),
            ..Payload::default()
        })
        .unwrap();
        serde_json::from_slice::<OldPayload>(&payload).unwrap();
        // Without marks, the wire is byte-for-byte the old shape.
        let plain = serde_json::to_string(&Payload::default()).unwrap();
        assert_eq!(
            plain,
            r#"{"words":[],"snippets":[],"history":[],"peers":[]}"#
        );
    }

    /// R44: a round that fails partway does not advance the mark, so
    /// the next round sends the rest again.
    #[test]
    fn an_interrupted_round_is_sent_again() {
        let (id_a, id_b, db_a, db_b) = paired();
        round(&id_a, &id_b, &db_a, &db_b);
        add_bulk_history(&db_a, MERGE_BATCH_ROWS + 1, 1);
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = serve_one(listener, db_a.clone(), id_a.creds());
        let local_db = db_b.clone();
        BETWEEN_MERGE_BATCHES.with(|slot| {
            slot.replace(Some(Box::new(move || {
                let writer = Store::open(&local_db).unwrap();
                disable_sync(&writer);
                writer
                    .save_settings(&crate::settings::AppSettings::default())
                    .unwrap();
            })));
        });
        sync_now(&[addr], &db_b, &id_b.creds(), "b", &id_a.node_id).unwrap_err();
        server.join().unwrap().unwrap();
        let partial = Store::open(&db_b)
            .unwrap()
            .load_history(500, true)
            .unwrap()
            .len();
        assert!(partial < MERGE_BATCH_ROWS + 3, "{partial}");
        let (b, _) = round(&id_a, &id_b, &db_a, &db_b);
        assert!(b.history_in > MERGE_BATCH_ROWS, "{}", b.history_in);
        let store_b = Store::open(&db_b).unwrap();
        assert_eq!(
            store_b.load_history(500, true).unwrap().len(),
            MERGE_BATCH_ROWS + 3
        );
        std::fs::remove_file(&db_a).ok();
        std::fs::remove_file(&db_b).ok();
    }

    /// R42: discovery refreshes a trusted peer's stored addresses, and a
    /// peer that left and came back counts as a new appearance.
    #[test]
    fn discovery_refreshes_addresses_and_notices_returns() {
        let store = Store::open_memory().unwrap();
        let trusted = DeviceIdentity::ephemeral().node_id;
        let stranger = DeviceIdentity::ephemeral().node_id;
        store
            .upsert_peer(&trusted, "t", "10.0.0.5:7000", true)
            .unwrap();
        store
            .upsert_peer(&stranger, "s", "10.0.0.6:7000", false)
            .unwrap();
        let found = |id: &str, address: &str| Discovered {
            node_id: id.to_string(),
            name: String::new(),
            addresses: vec![address.to_string()],
        };
        let lan: HashMap<String, Discovered> = [
            (trusted.clone(), found(&trusted, "10.0.0.9:7000")),
            (stranger.clone(), found(&stranger, "10.0.0.8:7000")),
        ]
        .into();
        let mut present = HashSet::new();
        assert!(note_discovered(&store, &lan, &mut present));
        let targets = store.get_peer(&trusted).unwrap().unwrap().dial_targets();
        assert_eq!(targets, vec!["10.0.0.9:7000", "10.0.0.5:7000"]);
        let stranger_targets = store.get_peer(&stranger).unwrap().unwrap().dial_targets();
        assert_eq!(stranger_targets, vec!["10.0.0.6:7000"]);
        assert!(!note_discovered(&store, &lan, &mut present));
        assert!(!note_discovered(&store, &HashMap::new(), &mut present));
        assert!(note_discovered(&store, &lan, &mut present));
    }

    #[test]
    fn mdns_addresses_are_dialable() {
        let v6 = mdns_sd::ScopedIp::from("2001:db8::1".parse::<IpAddr>().unwrap());
        let v6 = dial_address(&v6, 7000);
        assert_eq!(v6, "[2001:db8::1]:7000");
        assert!(v6.parse::<SocketAddr>().is_ok());
        let v4 = mdns_sd::ScopedIp::from("10.0.0.9".parse::<IpAddr>().unwrap());
        assert_eq!(dial_address(&v4, 7000), "10.0.0.9:7000");
    }
}

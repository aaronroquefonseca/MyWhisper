//! Node-to-node sync transport: authenticated Noise-encrypted TCP.
//!
//! Everything on the wire is encrypted with `Noise_XX` (mutual
//! authentication, forward secrecy) and framed. Trust is
//! table-driven with Syncthing-style introduction:
//!
//! * unknown dialers are recorded as pending requests, never merged;
//! * accepting explicitly grants trust; introduction authority is separate;
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
//! * capable peers exchange bounded pages with durable cursor acknowledgements;
//!   peers without that capability retain the small-database records flow;
//! * legacy records both ways: `{"words":[…],"snippets":[…],
//!   "history":[…],"peers":[…]}`
//!
//! Reachability v1: same LAN (mDNS/discovered/gossiped addresses),
//! VPN, or internet with a known address. Hole punching and relays
//! (libp2p-class) are the later internet phase.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::device::{DeviceIdentity, SyncCreds};
use crate::store::Store;

const PROTOCOL_V: u32 = 1;
const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const NOISE_BUF: usize = 65535;
const CHUNK: usize = 60000;
const FRAME_CAP: usize = NOISE_BUF;
const HELLO_CAP: usize = 16 * 1024;
const CONTENT_CAP: usize = 16 * 1024 * 1024;
const ROUND_TIMEOUT: Duration = Duration::from_secs(60);
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const AUTO_SYNC_SECS: u64 = 60;
const MDNS_SERVICE: &str = "_mywhisper._tcp.local.";

#[derive(Debug, Serialize, Deserialize)]
struct Hello {
    v: u32,
    #[serde(default)]
    pages: bool,
    #[serde(default)]
    segments: bool,
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
}

use crate::store::ContentSnapshot as Payload;

#[derive(Debug)]
pub struct SyncReport {
    pub peer_node: String,
    pub peer_name: String,
    pub words_in: usize,
    pub snippets_in: usize,
    pub history_in: usize,
    pub introduced: usize,
    /// This bounded session committed progress; further pages remain.
    pub has_more: bool,
}

/// mDNS-discovered peer (LAN, no configuration).
#[derive(Debug, Clone)]
pub struct Discovered {
    pub node_id: String,
    pub name: String,
    pub addresses: Vec<String>,
}

// -- Noise framing --------------------------------------------------------

fn transfer_timeout(deadline: std::time::Instant) -> Result<Duration, String> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err("network progress deadline exceeded".into());
    }
    Ok(remaining.min(IO_TIMEOUT))
}
fn write_until(
    stream: &mut TcpStream,
    mut bytes: &[u8],
    deadline: std::time::Instant,
    allowed: &impl Fn() -> bool,
) -> Result<(), String> {
    while !bytes.is_empty() {
        if crate::runtime::closing() || !allowed() {
            return Err("sync session cancelled".into());
        }
        stream
            .set_write_timeout(Some(
                transfer_timeout(deadline)?.min(Duration::from_millis(100)),
            ))
            .map_err(|e| e.to_string())?;
        match stream.write(bytes) {
            Ok(0) => return Err("connection closed during write".into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(e) => return Err(format!("write: {e}")),
        }
    }
    Ok(())
}
fn read_until(
    stream: &mut TcpStream,
    mut bytes: &mut [u8],
    deadline: std::time::Instant,
    allowed: &impl Fn() -> bool,
) -> Result<(), String> {
    while !bytes.is_empty() {
        if crate::runtime::closing() || !allowed() {
            return Err("sync session cancelled".into());
        }
        stream
            .set_read_timeout(Some(
                transfer_timeout(deadline)?.min(Duration::from_millis(100)),
            ))
            .map_err(|e| e.to_string())?;
        match stream.read(bytes) {
            Ok(0) => return Err("connection closed during read".into()),
            Ok(n) => bytes = &mut bytes[n..],
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    Ok(())
}
fn write_frame_checked(
    stream: &mut TcpStream,
    bytes: &[u8],
    deadline: std::time::Instant,
    allowed: &impl Fn() -> bool,
) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > FRAME_CAP {
        return Err("bad frame length".into());
    }
    write_until(
        stream,
        &(bytes.len() as u32).to_be_bytes(),
        deadline,
        allowed,
    )?;
    write_until(stream, bytes, deadline, allowed)
}
fn read_frame_checked(
    stream: &mut TcpStream,
    deadline: std::time::Instant,
    allowed: &impl Fn() -> bool,
) -> Result<Vec<u8>, String> {
    let mut len = [0; 4];
    read_until(stream, &mut len, deadline, allowed)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > FRAME_CAP {
        return Err("bad frame length".into());
    }
    let mut bytes = vec![0; len];
    read_until(stream, &mut bytes, deadline, allowed)?;
    Ok(bytes)
}

fn write_frame(
    stream: &mut TcpStream,
    bytes: &[u8],
    deadline: std::time::Instant,
) -> Result<(), String> {
    write_frame_checked(stream, bytes, deadline, &|| true)
}
fn read_frame(stream: &mut TcpStream, deadline: std::time::Instant) -> Result<Vec<u8>, String> {
    read_frame_checked(stream, deadline, &|| true)
}

/// One encrypted session: Noise_XX handshake, then length-framed
/// chunked messages over the transport.
pub struct NoiseStream {
    stream: TcpStream,
    transport: snow::TransportState,
    rbuf: Vec<u8>,
    cap: usize,
    deadline: std::time::Instant,
    policy: Option<SyncPolicy>,
    authorization_epoch: Option<i64>,
    owner_stop: Option<Arc<AtomicBool>>,
}

struct JsonBuffer {
    bytes: Vec<u8>,
    cap: usize,
}
impl Write for JsonBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.cap - self.bytes.len() {
            return Err(std::io::Error::other("sync JSON exceeds message limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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
        stream: TcpStream,
        private_key: &[u8; 32],
        initiator: bool,
    ) -> Result<(Self, Vec<u8>, [u8; 32]), String> {
        Self::handshake_until(
            stream,
            private_key,
            initiator,
            std::time::Instant::now() + ROUND_TIMEOUT,
        )
    }
    fn handshake_until(
        stream: TcpStream,
        private_key: &[u8; 32],
        initiator: bool,
        round_deadline: std::time::Instant,
    ) -> Result<(Self, Vec<u8>, [u8; 32]), String> {
        Self::handshake_until_checked(stream, private_key, initiator, round_deadline, &|| true)
    }
    fn handshake_until_checked(
        mut stream: TcpStream,
        private_key: &[u8; 32],
        initiator: bool,
        round_deadline: std::time::Instant,
        allowed: &impl Fn() -> bool,
    ) -> Result<(Self, Vec<u8>, [u8; 32]), String> {
        if !allowed() {
            return Err("sync handshake cancelled".into());
        }
        // Length frames and durable acknowledgements alternate. Buffering a
        // small prefix behind a delayed TCP ACK adds a stall to every chunk.
        stream
            .set_nodelay(true)
            .map_err(|e| format!("TCP progress setup: {e}"))?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(|e| format!("timeout: {e}"))?;
        stream
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(|e| format!("timeout: {e}"))?;
        let deadline = round_deadline.min(std::time::Instant::now() + IO_TIMEOUT);
        let mut hs = Self::builder(private_key, initiator)?;
        let mut buf = vec![0u8; NOISE_BUF];
        let mut payload = vec![0u8; NOISE_BUF];
        if initiator {
            // XX initiator: write, read, write.
            loop {
                let n = hs
                    .write_message(&[], &mut buf)
                    .map_err(|e| format!("handshake write: {e}"))?;
                write_frame_checked(&mut stream, &buf[..n], deadline, allowed)?;
                if hs.is_handshake_finished() {
                    break;
                }
                let msg = read_frame_checked(&mut stream, deadline, allowed)?;
                hs.read_message(&msg, &mut payload)
                    .map_err(|e| format!("handshake read: {e}"))?;
                if hs.is_handshake_finished() {
                    break;
                }
            }
        }
        if !initiator {
            loop {
                let msg = read_frame_checked(&mut stream, deadline, allowed)?;
                hs.read_message(&msg, &mut payload)
                    .map_err(|e| format!("handshake read: {e}"))?;
                if hs.is_handshake_finished() {
                    break;
                }
                let n = hs
                    .write_message(&[], &mut buf)
                    .map_err(|e| format!("handshake write: {e}"))?;
                write_frame_checked(&mut stream, &buf[..n], deadline, allowed)?;
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
                cap: HELLO_CAP,
                deadline: round_deadline,
                policy: None,
                authorization_epoch: None,
                owner_stop: None,
            },
            hash,
            remote,
        ))
    }

    fn progress_deadline(&mut self) -> Result<(), String> {
        if crate::runtime::closing() {
            return Err("sync cancelled by shutdown".into());
        }
        if self
            .owner_stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Acquire))
        {
            return Err("sync owner stopped".into());
        }
        if let Some(policy) = &self.policy {
            policy.check()?;
        }
        let remaining = self
            .deadline
            .saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err("sync round deadline exceeded".into());
        }
        let timeout = remaining.min(IO_TIMEOUT);
        self.stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| e.to_string())?;
        self.stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn write_chunk(&mut self, plaintext: &[u8]) -> Result<(), String> {
        self.progress_deadline()?;
        let mut buf = vec![0u8; plaintext.len() + 32];
        let n = self
            .transport
            .write_message(plaintext, &mut buf)
            .map_err(|e| format!("encrypt: {e}"))?;
        write_frame(&mut self.stream, &buf[..n], self.deadline)
    }

    /// Send one logical message (chunked at 60 KiB plaintext).
    pub fn send_msg(&mut self, data: &[u8]) -> Result<(), String> {
        if data.len() > self.cap {
            return Err("message too large".into());
        }
        let total = data.len() as u64;
        self.write_chunk(&total.to_be_bytes())?;
        for chunk in data.chunks(CHUNK) {
            self.write_chunk(chunk)?;
        }
        Ok(())
    }

    pub fn send_json(&mut self, value: &impl Serialize) -> Result<(), String> {
        let mut buffer = JsonBuffer {
            bytes: Vec::new(),
            cap: self.cap,
        };
        serde_json::to_writer(&mut buffer, value)
            .map_err(|e| format!("encode: {}", crate::diagnostics::json(&e)))?;
        self.send_msg(&buffer.bytes)
    }

    fn read_chunk(&mut self) -> Result<Vec<u8>, String> {
        self.progress_deadline()?;
        let frame = read_frame(&mut self.stream, self.deadline)?;
        let n = self
            .transport
            .read_message(&frame, &mut self.rbuf)
            .map_err(|e| format!("decrypt: {e}"))?;
        Ok(self.rbuf[..n].to_vec())
    }

    /// Receive one logical message, reassembling chunks.
    pub fn recv_msg(&mut self) -> Result<Vec<u8>, String> {
        let head = self.read_chunk()?;
        if head.len() != 8 {
            return Err("bad message header".to_string());
        }
        let total = u64::from_be_bytes(head[..8].try_into().unwrap()) as usize;
        if total > self.cap {
            return Err("message too large".to_string());
        }
        let mut out = Vec::with_capacity(total.min(1024 * 1024));
        while out.len() < total {
            let chunk = self.read_chunk()?;
            if chunk.is_empty() || chunk.len() > total - out.len() {
                return Err("message made invalid progress".into());
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
        serde_json::from_slice(&bytes)
            .map_err(|e| format!("decode: {}", crate::diagnostics::json(&e)))
    }
}

struct SyncPolicy {
    store: Store,
    peer: String,
    epoch: i64,
    owner_stop: Option<Arc<AtomicBool>>,
    stop: Arc<AtomicBool>,
    watcher: Option<crate::runtime::OwnedThread>,
}

fn permitted(store: &Store, peer: &str) -> Result<(), String> {
    if crate::runtime::closing() {
        return Err("sync cancelled by shutdown".into());
    }
    store
        .check_sync_identity()
        .map_err(|error| error.to_string())?;
    if !store.load_settings().sync.enabled {
        return Err("sync disabled".into());
    }
    if store.is_revoked(peer).map_err(|e| e.to_string())? {
        return Err("peer revoked".into());
    }
    if !store
        .get_peer(peer)
        .map_err(|e| e.to_string())?
        .is_some_and(|p| p.trusted)
    {
        return Err("peer not trusted locally".into());
    }
    Ok(())
}

impl SyncPolicy {
    /// A replaced database (restore/delete-recreate) must abort the
    /// exchange: long-lived handles would otherwise authorize against
    /// an unlinked inode while a new database serves new policy.
    fn file_replaced(store: &Store) -> bool {
        use std::os::unix::fs::MetadataExt;
        let Some(path) = store.path() else {
            return true;
        };
        let (device, inode) = match store.file_identity() {
            Some(identity) => identity,
            None => return true,
        };
        match std::fs::symlink_metadata(path) {
            Ok(current) => !current.is_file() || current.dev() != device || current.ino() != inode,
            Err(_) => true,
        }
    }
    fn check(&self) -> Result<(), String> {
        if Self::file_replaced(&self.store) {
            return Err("sync database was replaced during exchange".into());
        }
        if self
            .owner_stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Acquire))
        {
            return Err("sync owner stopped".into());
        }
        permitted(&self.store, &self.peer)?;
        if self.store.sync_epoch() != self.epoch {
            return Err("sync policy changed during exchange".into());
        }
        Ok(())
    }
    fn install(
        session: &mut NoiseStream,
        store: &Store,
        peer: &str,
        epoch: i64,
    ) -> Result<(), String> {
        permitted(store, peer)?;
        if store.sync_epoch() != epoch {
            return Err("sync authorization changed during setup".into());
        }
        if let Some(path) = store.path() {
            let path = path.to_path_buf();
            let watch_path = path.clone();
            let peer = peer.to_string();
            let watch_peer = peer.clone();
            let socket = session.stream.try_clone().map_err(|e| e.to_string())?;
            let stop = Arc::new(AtomicBool::new(false));
            let done = stop.clone();
            let owner_stop = session.owner_stop.clone();
            let watcher_owner = owner_stop.clone();
            let policy_store = Store::open_existing(&path).map_err(|e| e.to_string())?;
            // Dedicated authorization connection: quick policy reads must
            // never queue behind a busy writer or mutate the database.
            policy_store
                .policy_reads_only()
                .map_err(|e| e.to_string())?;
            let watcher = crate::runtime::spawn_dedicated("sync-policy", move || {
                let Ok(store) = Store::open_existing(&watch_path) else {
                    let _ = socket.shutdown(std::net::Shutdown::Both);
                    return;
                };
                while !done.load(Ordering::Acquire) {
                    if watcher_owner
                        .as_ref()
                        .is_some_and(|stop| stop.load(Ordering::Acquire))
                        || SyncPolicy::file_replaced(&store)
                        || permitted(&store, &watch_peer).is_err()
                        || store.sync_epoch() != epoch
                    {
                        let _ = socket.shutdown(std::net::Shutdown::Both);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            })?;
            session.policy = Some(Self {
                store: policy_store,
                peer,
                epoch,
                owner_stop,
                stop,
                watcher: Some(watcher),
            });
        }
        session.cap = CONTENT_CAP;
        Ok(())
    }
}
impl Drop for SyncPolicy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.watcher.take() {
            let _ = worker.join();
        }
    }
}

// -- exchange ---------------------------------------------------------------

/// Run one side of an exchange over an encrypted session.
///
/// * `initiator` -- dialer sends first at every step (strict
///   alternation, so large payloads cannot deadlock).
/// * `expect_node` -- when set, the proven session must belong to
///   exactly this node id (client side).
/// * `remote` -- dial-back address observed for this contact.
#[allow(clippy::too_many_arguments)]
pub fn exchange(
    session: &mut NoiseStream,
    session_hash: &[u8],
    store: &Store,
    creds: &SyncCreds,
    name: &str,
    addresses: &[String],
    initiator: bool,
    expect_node: Option<&str>,
    remote: Option<String>,
) -> Result<SyncReport, String> {
    let policy_epoch = session
        .authorization_epoch
        .unwrap_or_else(|| store.sync_epoch());
    let hello = Hello {
        v: PROTOCOL_V,
        pages: true,
        segments: true,
        node_id: creds.node_id.clone(),
        name: name.to_string(),
        addresses: addresses.to_vec(),
        introducer: false,
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
    let proof_ok = peer.sig.len() == 64 && {
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
    } else {
        if peer.v != PROTOCOL_V || !proof_ok {
            session.send_json(&Verdict {
                ok: false,
                error: "identity proof failed".to_string(),
            })?;
            return Err("identity proof failed".to_string());
        }
    }
    if let Some(expected) = expect_node
        && peer.node_id != expected
    {
        return Err("peer node id mismatch".to_string());
    }

    // Sync-off must not create contact rows: check authorization before
    // any record_seen write. Disabled/revoked/epoch-changed peers get a
    // clean rejection with no pending row for this contact.
    if !store.load_settings().sync.enabled {
        if initiator {
            return Err("sync disabled".to_string());
        }
        session.send_json(&Verdict {
            ok: false,
            error: "sync disabled".to_string(),
        })?;
        return Err("sync disabled".to_string());
    }
    if store.is_revoked(&peer.node_id).map_err(|e| e.to_string())? {
        if initiator {
            return Err("peer revoked".to_string());
        }
        session.send_json(&Verdict {
            ok: false,
            error: "peer revoked".to_string(),
        })?;
        return Err("peer revoked".to_string());
    }
    // Test-only parked rendezvous for the authorization race: a test
    // client installs a hook, passes the gate above, then blocks until
    // the test releases it — so policy edits land deterministically
    // mid-exchange instead of racing gate passage. Compiled out in
    // production; without an installed hook this branch is a no-op.
    #[cfg(test)]
    if initiator && let Some(path) = store.path() {
        crate::private_fs::replacement_fault::point("sync-client-parked", path);
    }
    // Contact bookkeeping: unknown nodes become pending requests,
    // known ones refresh addresses.
    let mut contact_addrs: Vec<String> = peer.addresses.clone();
    if let Some(remote) = remote.filter(|_| initiator)
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

    if initiator {
        let verdict: Verdict = session.recv_json()?;
        if !verdict.ok {
            return Err(if verdict.error.is_empty() {
                "rejected".to_string()
            } else {
                verdict.error
            });
        }
    } else if !trusted {
        session.send_json(&Verdict {
            ok: false,
            error: "not trusted: request recorded, ask them to accept".to_string(),
        })?;
        return Err(if is_new {
            format!("new pairing request from {}", short(&peer.node_id))
        } else {
            "peer not trusted".to_string()
        });
    }
    if !initiator {
        session.send_json(&Verdict {
            ok: true,
            error: String::new(),
        })?;
    }

    permitted(store, &peer.node_id)?;
    SyncPolicy::install(session, store, &peer.node_id, policy_epoch)?;
    if peer.pages && peer.segments {
        return exchange_packets(session, store, creds, peer, policy_epoch, initiator);
    }
    if peer.pages {
        return exchange_pages(session, store, creds, peer, policy_epoch, initiator);
    }
    // Records, strict alternation again.
    let mut ours = store
        .export_authorized(&peer.node_id, policy_epoch)
        .map_err(|e| format!("authorized export: {e}"))?;
    ours.peers.retain(|peer| peer.node_id != creds.node_id);
    let theirs: Payload = if initiator {
        session.send_json(&ours)?;
        session.recv_json()?
    } else {
        let theirs: Payload = session.recv_json()?;
        session.send_json(&ours)?;
        theirs
    };

    permitted(store, &peer.node_id)?;
    session.progress_deadline()?;
    let introduced = store
        .apply_sync_round(
            &peer.node_id,
            policy_epoch,
            &creds.node_id,
            (&theirs.words, &theirs.snippets, &theirs.history),
            &theirs.peers,
        )
        .map_err(|e| format!("merge round: {e}"))?;
    Ok(SyncReport {
        peer_node: peer.node_id,
        peer_name: peer.name,
        words_in: theirs.words.len(),
        snippets_in: theirs.snippets.len(),
        history_in: theirs.history.len(),
        introduced,
        has_more: false,
    })
}

/// Each page is acknowledged only after its merge and receive cursor commit.
/// Alternate directions so a large sender cannot deadlock a large receiver.
/// A fixed upper sequence excludes changes made by the incoming half of this
/// session; their fresh sequences are sent by the next round.
fn exchange_pages(
    session: &mut NoiseStream,
    store: &Store,
    creds: &SyncCreds,
    peer: Hello,
    epoch: i64,
    initiator: bool,
) -> Result<SyncReport, String> {
    use crate::store::{SyncManifest, SyncPage};
    let ours = store
        .sync_manifest(&peer.node_id, epoch)
        .map_err(|e| e.to_string())?;
    let theirs: SyncManifest = if initiator {
        session.send_json(&ours)?;
        session.recv_json()?
    } else {
        let theirs = session.recv_json()?;
        session.send_json(&ours)?;
        theirs
    };
    let mut incoming = store
        .sync_received_cursor(&peer.node_id, epoch, &theirs)
        .map_err(|e| e.to_string())?;
    let mut outgoing: i64 = if initiator {
        session.send_json(&incoming)?;
        session.recv_json()?
    } else {
        let outgoing = session.recv_json()?;
        session.send_json(&incoming)?;
        outgoing
    };
    if outgoing < 0 || outgoing > ours.upper {
        return Err("invalid peer receive cursor".into());
    }
    let mut report = SyncReport {
        peer_node: peer.node_id.clone(),
        peer_name: peer.name,
        words_in: 0,
        snippets_in: 0,
        history_in: 0,
        introduced: 0,
        has_more: true,
    };
    for _ in 0..1024 {
        let page = store
            .export_sync_page(&peer.node_id, epoch, &creds.node_id, outgoing, ours.upper)
            .map_err(|e| e.to_string())?;
        let outgoing_done = page.done;
        let send = |session: &mut NoiseStream| -> Result<(), String> {
            session.send_json(&page)?;
            let ack: i64 = session.recv_json()?;
            if ack != page.to {
                return Err("invalid page acknowledgement".into());
            }
            Ok(())
        };
        let mut receive = |session: &mut NoiseStream| -> Result<bool, String> {
            let received: SyncPage = session.recv_json()?;
            session.progress_deadline()?;
            if received.from != incoming {
                return Err("unexpected page position".into());
            }
            report.introduced += store
                .apply_sync_page(&peer.node_id, epoch, &creds.node_id, &theirs, &received)
                .map_err(|e| e.to_string())?;
            report.words_in += received.content.words.len();
            report.snippets_in += received.content.snippets.len();
            report.history_in += received.content.history.len();
            incoming = received.to;
            session.send_json(&incoming)?;
            Ok(received.done)
        };
        let incoming_done = if initiator {
            send(session)?;
            receive(session)?
        } else {
            let done = receive(session)?;
            send(session)?;
            done
        };
        outgoing = page.to;
        if outgoing_done && incoming_done {
            report.has_more = false;
            break;
        }
    }
    Ok(report)
}

fn exchange_packets(
    session: &mut NoiseStream,
    store: &Store,
    creds: &SyncCreds,
    peer: Hello,
    epoch: i64,
    initiator: bool,
) -> Result<SyncReport, String> {
    use crate::store::{SyncCursor, SyncManifest, SyncPacket};
    let ours = store
        .sync_manifest(&peer.node_id, epoch)
        .map_err(|e| e.to_string())?;
    let theirs: SyncManifest = if initiator {
        session.send_json(&ours)?;
        session.recv_json()?
    } else {
        let theirs = session.recv_json()?;
        session.send_json(&ours)?;
        theirs
    };
    let mut incoming = store
        .sync_packet_cursor(&peer.node_id, epoch, &theirs)
        .map_err(|e| e.to_string())?;
    let mut outgoing: SyncCursor = if initiator {
        session.send_json(&incoming)?;
        session.recv_json()?
    } else {
        let outgoing = session.recv_json()?;
        session.send_json(&incoming)?;
        outgoing
    };
    if outgoing.committed < 0 || outgoing.committed > ours.upper {
        return Err("invalid peer receive cursor".into());
    }
    let mut report = SyncReport {
        peer_node: peer.node_id.clone(),
        peer_name: peer.name,
        words_in: 0,
        snippets_in: 0,
        history_in: 0,
        introduced: 0,
        has_more: true,
    };
    for _ in 0..1024 {
        let packet = store
            .export_sync_packet(&peer.node_id, epoch, &creds.node_id, &ours, &outgoing)
            .map_err(|e| e.to_string())?;
        let outgoing_done = packet.done();
        let send = |session: &mut NoiseStream| -> Result<SyncCursor, String> {
            session.send_json(&packet)?;
            let ack: SyncCursor = session.recv_json()?;
            if !packet.accepts(&ack) {
                return Err("invalid page acknowledgement".into());
            }
            store
                .acknowledge_packet(&peer.node_id, epoch, &ack)
                .map_err(|e| e.to_string())?;
            Ok(ack)
        };
        let mut receive = |session: &mut NoiseStream| -> Result<bool, String> {
            let received: SyncPacket = session.recv_json()?;
            session.progress_deadline()?;
            if received.from() != incoming.committed {
                return Err("unexpected packet position".into());
            }
            let applied = store
                .apply_sync_packet(&peer.node_id, epoch, &creds.node_id, &theirs, &received)
                .map_err(|e| e.to_string())?;
            report.introduced += applied.introduced;
            report.words_in += applied.words;
            report.snippets_in += applied.snippets;
            report.history_in += applied.history;
            incoming = applied.cursor;
            session.send_json(&incoming)?;
            Ok(applied.done)
        };
        let incoming_done = if initiator {
            outgoing = send(session)?;
            receive(session)?
        } else {
            let done = receive(session)?;
            outgoing = send(session)?;
            done
        };
        if outgoing_done && incoming_done {
            report.has_more = false;
            break;
        }
    }
    Ok(report)
}

fn short(node_id: &str) -> String {
    crate::device::short_id(node_id)
}

/// Dial a peer's known addresses in order; first success wins.
/// Runs the Noise handshake, then requires the proven session to
/// belong to `expect_node` before any record moves.
pub fn sync_now(
    addresses: &[String],
    db_path: &Path,
    creds: &SyncCreds,
    name: &str,
    expect_node: &str,
) -> Result<SyncReport, String> {
    sync_now_at_epoch(addresses, db_path, creds, name, expect_node, None)
}

pub(crate) fn sync_now_at_epoch(
    addresses: &[String],
    db_path: &Path,
    creds: &SyncCreds,
    name: &str,
    expect_node: &str,
    requested_epoch: Option<i64>,
) -> Result<SyncReport, String> {
    sync_now_owned(
        addresses,
        db_path,
        creds,
        name,
        expect_node,
        requested_epoch,
        None,
    )
}

fn sync_now_owned(
    addresses: &[String],
    db_path: &Path,
    creds: &SyncCreds,
    name: &str,
    expect_node: &str,
    requested_epoch: Option<i64>,
    owner_stop: Option<Arc<AtomicBool>>,
) -> Result<SyncReport, String> {
    let owner_running = || {
        !crate::runtime::closing()
            && !owner_stop
                .as_ref()
                .is_some_and(|stop| stop.load(Ordering::Acquire))
    };
    if !owner_running() {
        return Err("sync owner stopped".into());
    }
    let initial = Store::open_existing(db_path).map_err(|e| e.to_string())?;
    if !initial.load_settings().sync.enabled {
        return Err("sync disabled".into());
    }
    let epoch = initial.sync_epoch();
    if requested_epoch.is_some_and(|requested| requested != epoch) {
        return Err("queued sync authorization expired".into());
    }
    if addresses.is_empty() {
        return Err("no known address for peer".to_string());
    }
    if addresses.len() > 8 {
        return Err("too many peer addresses".into());
    }
    let round_deadline = std::time::Instant::now() + ROUND_TIMEOUT;
    let mut last_err = String::from("no usable address");
    for address in addresses {
        if !owner_running()
            || initial.sync_epoch() != epoch
            || !initial.load_settings().sync.enabled
        {
            return Err("sync authorization changed while dialing".into());
        }
        let targets = match crate::address::resolve(address, round_deadline, || {
            owner_running() && initial.sync_epoch() == epoch && initial.load_settings().sync.enabled
        }) {
            Ok(targets) => targets,
            Err(e) => {
                last_err = format!("resolve: {e}");
                continue;
            }
        };
        for addr in targets {
            let result = (|| {
                let remaining = round_deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(CONNECT_TIMEOUT);
                if remaining.is_zero() {
                    return Err("sync connection deadline exceeded".into());
                }
                if !owner_running()
                    || initial.sync_epoch() != epoch
                    || !initial.load_settings().sync.enabled
                {
                    return Err("sync authorization changed while dialing".into());
                }
                let stream =
                    crate::address::connect(addr, std::time::Instant::now() + remaining, || {
                        owner_running()
                            && initial.sync_epoch() == epoch
                            && initial.load_settings().sync.enabled
                    })?;
                let (mut session, hash, _) = NoiseStream::handshake_until_checked(
                    stream,
                    &creds.noise_static,
                    true,
                    round_deadline,
                    &|| {
                        owner_running()
                            && initial.sync_epoch() == epoch
                            && initial.load_settings().sync.enabled
                    },
                )?;
                session.authorization_epoch = Some(epoch);
                session.owner_stop = owner_stop.clone();
                let store = Store::open_existing(db_path).map_err(|e| e.to_string())?;
                let addresses = store.load_settings().sync.listen_port;
                let ours = lan_ip()
                    .filter(|_| addresses != 0)
                    .map(|ip| std::net::SocketAddr::new(ip, addresses).to_string())
                    .into_iter()
                    .collect::<Vec<_>>();
                exchange(
                    &mut session,
                    &hash,
                    &store,
                    creds,
                    name,
                    &ours,
                    true,
                    Some(expect_node),
                    None,
                )
            })();
            match result {
                Ok(report) => return Ok(report),
                Err(e) => last_err = e,
            }
        }
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
    addresses: Vec<String>,
    stop: &AtomicBool,
) {
    let _ = listener.set_nonblocking(true);
    let Ok(accept_store) = Store::open_existing(&db_path) else {
        eprintln!("[sync] listener database unavailable");
        return;
    };
    let mut workers: Vec<(crate::runtime::OwnedThread, TcpStream)> = Vec::new();
    let reap = |workers: &mut Vec<(crate::runtime::OwnedThread, TcpStream)>| {
        let mut i = 0;
        while i < workers.len() {
            if workers[i].0.is_finished() {
                let (worker, _) = workers.swap_remove(i);
                let _ = worker.join();
            } else {
                i += 1;
            }
        }
    };
    for conn in listener.incoming() {
        if crate::runtime::closing() || stop.load(Ordering::Relaxed) {
            break;
        }
        match conn {
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                reap(&mut workers);
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Ok(stream) => {
                reap(&mut workers);
                if workers.len() >= 8 {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                // Disabled listener still accepts TCP but must not handshake
                // or record contact: close early without Noise/hello work.
                // Settings are re-read per accept so toggle-off applies now.
                if !accept_store.load_settings().sync.enabled {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                let Ok(owned_socket) = stream.try_clone() else {
                    continue;
                };
                let remote = stream
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| String::new());
                let remote = if remote.is_empty() {
                    None
                } else {
                    Some(remote)
                };
                let db_path = db_path.clone();
                let creds = creds.clone();
                let name = name.clone();
                let addresses = addresses.clone();
                let authorization_epoch = accept_store.sync_epoch();
                let worker = crate::runtime::spawn_dedicated("sync-connection", move || {
                    let res = (|| -> Result<SyncReport, String> {
                        // Re-check toggle inside the worker: the accept-time
                        // gate above races toggle-off, so never handshake or
                        // record contact while disabled.
                        let pre =
                            Store::open_existing(&db_path).map_err(|e| format!("open db: {e}"))?;
                        if !pre.load_settings().sync.enabled {
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                            return Err("sync disabled".to_string());
                        }
                        let (mut session, hash, _remote_static) =
                            NoiseStream::handshake(stream, &creds.noise_static, false)?;
                        session.authorization_epoch = Some(authorization_epoch);
                        let store =
                            Store::open_existing(&db_path).map_err(|e| format!("open db: {e}"))?;
                        exchange(
                            &mut session,
                            &hash,
                            &store,
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
                            "[sync] {} {} ({} words in, {} introduced)",
                            if report.has_more {
                                "partial progress from"
                            } else {
                                "served"
                            },
                            report.peer_node,
                            report.words_in,
                            report.introduced
                        ),
                        Err(e) => {
                            eprintln!("[sync] {}: {e}", remote.unwrap_or_else(|| "?".to_string()))
                        }
                    }
                });
                match worker {
                    Ok(worker) => workers.push((worker, owned_socket)),
                    Err(error) => eprintln!("[sync] connection budget exhausted: {error}"),
                }
            }
            Err(e) => eprintln!("[sync] accept: {e}"),
        }
    }
    for (_, socket) in &workers {
        let _ = socket.shutdown(std::net::Shutdown::Both);
    }
    for (worker, _) in workers {
        let _ = worker.join();
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
    let sock = addr.parse().ok()?;
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
    stop: Arc<AtomicBool>,
    browser: Mutex<Option<crate::runtime::SessionTask>>,
}

impl MdnsHandles {
    /// Unregister + stop the daemon (browser recv ends, threads exit).
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(browser) = self
            .browser
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            browser.join();
        }
    }
}

impl Drop for MdnsHandles {
    fn drop(&mut self) {
        // GTK owners only signal shutdown. SessionTask transfers joining to
        // the bounded reaper; engine owners explicitly call shutdown above.
        if !self.stop.swap(true, Ordering::AcqRel)
            && self
                .browser
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_none()
        {
            // Initialization failed before the browser took daemon ownership.
            let _ = self._daemon.shutdown();
        }
    }
}

fn shutdown_mdns(daemon: &mdns_sd::ServiceDaemon) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match daemon.shutdown() {
            Ok(stopped) => {
                if stopped
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .is_err()
                {
                    eprintln!("[sync] mDNS shutdown acknowledgement unavailable");
                }
                return;
            }
            Err(mdns_sd::Error::Again) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                eprintln!("[sync] mDNS shutdown: {error}");
                return;
            }
        }
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
    if crate::runtime::closing() {
        return Err("discovery cancelled by shutdown".into());
    }
    let handles = MdnsHandles {
        _daemon: mdns_sd::ServiceDaemon::new().map_err(|e| format!("mdns: {e}"))?,
        stop: Arc::new(AtomicBool::new(false)),
        browser: Mutex::new(None),
    };
    let daemon = &handles._daemon;
    if advertise {
        let short_id = &node_id[..node_id.len().min(8)];
        let mut txt = std::collections::HashMap::new();
        txt.insert("node".to_string(), node_id.to_string());
        txt.insert("name".to_string(), name.to_string());
        let host = format!("mywhisper-{short_id}.local.");
        let mut addrs: Vec<IpAddr> = Vec::new();
        if let Some(ip) = lan_ip() {
            addrs.push(ip);
        }
        let info =
            mdns_sd::ServiceInfo::new(MDNS_SERVICE, short_id, &host, &addrs[..], port, Some(txt))
                .map_err(|e| format!("mdns service: {e}"))?;
        daemon
            .register(info)
            .map_err(|e| format!("mdns register: {e}"))?;
    }

    let browser = daemon
        .browse(MDNS_SERVICE)
        .map_err(|e| format!("mdns browse: {e}"))?;
    let self_id = node_id.to_string();
    let stop = handles.stop.clone();
    let daemon = daemon.clone();
    let worker = crate::runtime::spawn_session("sync-mdns-browser", move || {
        let mut service_nodes = HashMap::new();
        while !crate::runtime::closing() && !stop.load(Ordering::Acquire) {
            let event = match browser.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => event,
                Err(_) if browser.is_disconnected() => break,
                Err(_) => continue,
            };
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                    let node = info
                        .get_property_val_str("node")
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    if !crate::device::valid_node_id(&node) || node == self_id {
                        continue;
                    }
                    let name = info
                        .get_property_val_str("name")
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    let port = info.get_port();
                    if name.len() > 256 || port == 0 {
                        continue;
                    }
                    let addresses: Vec<String> = info
                        .get_addresses()
                        .iter()
                        .map(|ip| match ip {
                            mdns_sd::ScopedIp::V6(ip) => std::net::SocketAddrV6::new(
                                *ip.addr(),
                                port,
                                0,
                                ip.scope_id().index,
                            )
                            .to_string(),
                            _ => std::net::SocketAddr::new(ip.to_ip_addr(), port).to_string(),
                        })
                        .take(8)
                        .collect();
                    if addresses.is_empty() {
                        continue;
                    }
                    let full_name = info.get_fullname().to_string();
                    if service_nodes.len() >= 256 && !service_nodes.contains_key(&full_name) {
                        continue;
                    }
                    service_nodes.insert(full_name, node.clone());
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
                    if let Some(node) = service_nodes.remove(&full_name)
                        && !service_nodes.values().any(|present| *present == node)
                    {
                        sink.lock().unwrap().remove(&node);
                    }
                }
                _ => {}
            }
        }
        shutdown_mdns(&daemon);
    })?;
    *handles.browser.lock().unwrap_or_else(|p| p.into_inner()) = Some(worker);
    Ok(handles)
}

// -- periodic and triggered rounds ------------------------------------------

/// One best-effort round with every trusted peer that has a known
/// address. Returns per-peer outcome lines for logging. Unreachable
/// peers are simply skipped; the next round (or the next local
/// change) retries them.
pub fn sync_all_trusted(db_path: &Path, creds: &SyncCreds) -> Vec<String> {
    let store = match Store::open_existing(db_path) {
        Ok(s) => s,
        Err(e) => return vec![format!("open db: {e}")],
    };
    let settings = store.load_settings();
    if !settings.sync.enabled {
        return Vec::new();
    }
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
    let epoch = store.sync_epoch();
    let mut out = Vec::new();
    for peer in peers {
        match sync_now_at_epoch(
            &peer.dial_targets(),
            db_path,
            creds,
            &name,
            &peer.node_id,
            Some(epoch),
        ) {
            Ok(r) => out.push(format!(
                "{} {} ({} words in)",
                if r.has_more {
                    "progress saved for; sync continues"
                } else {
                    "synced"
                },
                short(&r.peer_node),
                r.words_in
            )),
            Err(e) => out.push(format!("{} unreachable ({e})", short(&peer.node_id))),
        }
    }
    out
}

/// Every minute, sync with each trusted peer that has a known
/// address. Failures just log; the next round retries.
pub fn auto_sync_loop(db_path: PathBuf, creds: SyncCreds, stop: Arc<AtomicBool>) {
    for _ in 0..AUTO_SYNC_SECS {
        if crate::runtime::closing() || stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    loop {
        if crate::runtime::closing() || stop.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(store) = Store::open_existing(&db_path) {
            schedule_all(
                db_path.clone(),
                creds.clone(),
                store.sync_epoch(),
                Duration::ZERO,
                Some(stop.clone()),
            );
        }
        for _ in 0..AUTO_SYNC_SECS {
            if crate::runtime::closing() || stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

enum BackgroundRequest {
    Enumerate {
        db: PathBuf,
        creds: SyncCreds,
        epoch: i64,
        owner_stop: Option<Arc<AtomicBool>>,
        after: std::time::Instant,
        offset: usize,
        budget_reported: bool,
    },
    Peer {
        db: PathBuf,
        creds: SyncCreds,
        epoch: i64,
        owner_stop: Option<Arc<AtomicBool>>,
        node: String,
        name: String,
        addresses: Vec<String>,
    },
}
type BackgroundKey = (PathBuf, String);
type Outgoing = crate::sync_jobs::Coalescer<BackgroundKey, BackgroundRequest>;
static OUTGOING: std::sync::OnceLock<Result<Outgoing, String>> = std::sync::OnceLock::new();
static OUTGOING_INIT: Mutex<()> = Mutex::new(());
fn outgoing() -> Result<&'static Outgoing, String> {
    let _init = OUTGOING_INIT.lock().unwrap_or_else(|p| p.into_inner());
    if crate::runtime::closing() {
        return Err("sync scheduler stopped".into());
    }
    OUTGOING
        .get_or_init(|| Outgoing::new(4, 64, run_background))
        .as_ref()
        .map_err(Clone::clone)
}
pub(crate) fn shutdown_outgoing() {
    let _init = OUTGOING_INIT.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(Ok(scheduler)) = OUTGOING.get() {
        scheduler.stop();
    }
}
// The catch-up delay belongs to the original engine. Closing or replacing that
// owner must interrupt its wait before any database open or peer enumeration.
fn wait_background_delay(after: std::time::Instant, cancelled: impl Fn() -> bool) -> bool {
    let now = std::time::Instant::now();
    let deadline = now
        + after
            .saturating_duration_since(now)
            .min(Duration::from_secs(3));
    loop {
        if crate::runtime::closing() || cancelled() {
            return false;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return true;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

#[cfg(test)]
#[test]
fn background_catchup_delay_interrupts_original_owner_before_database_io() {
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stop = stopped.clone();
    let (entered, ready) = std::sync::mpsc::channel();
    let (exited, done) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let first = AtomicBool::new(true);
        let result =
            wait_background_delay(std::time::Instant::now() + Duration::from_secs(3), || {
                let cancelled = worker_stop.load(Ordering::Acquire);
                if first.swap(false, Ordering::AcqRel) {
                    // The first permission check has passed before cancellation.
                    entered.send(()).unwrap();
                }
                cancelled
            });
        exited.send(result).unwrap();
    });
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    stopped.store(true, Ordering::Release);
    assert!(!done.recv_timeout(Duration::from_millis(250)).unwrap());
    worker.join().unwrap();
    assert!(wait_background_delay(std::time::Instant::now(), || false));
    assert!(!wait_background_delay(std::time::Instant::now(), || true));
}

fn run_background(request: BackgroundRequest) {
    match request {
        BackgroundRequest::Enumerate {
            db,
            creds,
            epoch,
            owner_stop,
            after,
            offset,
            mut budget_reported,
        } => {
            if crate::runtime::closing()
                || owner_stop
                    .as_ref()
                    .is_some_and(|stop| stop.load(Ordering::Acquire))
            {
                return;
            }
            if !wait_background_delay(after, || {
                owner_stop
                    .as_ref()
                    .is_some_and(|stop| stop.load(Ordering::Acquire))
            }) {
                return;
            }
            let store = match Store::open_existing(&db) {
                Ok(store) => store,
                Err(_) => {
                    eprintln!("[sync] outgoing database unavailable");
                    return;
                }
            };
            let settings = store.load_settings();
            if owner_stop
                .as_ref()
                .is_some_and(|stop| stop.load(Ordering::Acquire))
                || store.sync_epoch() != epoch
                || !settings.sync.enabled
            {
                return;
            }
            let name = crate::device::display_name(&settings.sync.device_name, &creds.node_id);
            let mut peers: Vec<_> = match store.list_peers() {
                Ok(peers) => peers
                    .into_iter()
                    .filter(|peer| peer.trusted && !peer.dial_targets().is_empty())
                    .collect(),
                Err(_) => {
                    eprintln!("[sync] outgoing peer list unavailable");
                    return;
                }
            };
            peers.sort_unstable_by(|a, b| a.node_id.cmp(&b.node_id));
            let total = peers.len();
            let mut next = offset;
            for peer in peers.into_iter().skip(offset).take(16) {
                let addresses = peer.dial_targets();
                if addresses.is_empty() {
                    continue;
                }
                let key = (db.clone(), peer.node_id.clone());
                let request = BackgroundRequest::Peer {
                    db: db.clone(),
                    creds: creds.clone(),
                    epoch,
                    owner_stop: owner_stop.clone(),
                    node: peer.node_id,
                    name: name.clone(),
                    addresses,
                };
                if let Err(error) = outgoing().and_then(|jobs| jobs.submit(key, request)) {
                    if !budget_reported {
                        eprintln!(
                            "[sync] outgoing admission: {error}; remaining peers retained for retry"
                        );
                        budget_reported = true;
                    }
                    break;
                }
                next += 1;
            }
            if next < total {
                let key = (db.clone(), String::new());
                let request = BackgroundRequest::Enumerate {
                    db,
                    creds,
                    epoch,
                    owner_stop: owner_stop.clone(),
                    after: std::time::Instant::now() + Duration::from_millis(100),
                    offset: next,
                    budget_reported,
                };
                if let Err(error) = outgoing().and_then(|jobs| jobs.continue_if_clean(key, request))
                {
                    eprintln!("[sync] outgoing continuation: {error}");
                }
            }
        }
        BackgroundRequest::Peer {
            db,
            creds,
            epoch,
            owner_stop,
            node,
            name,
            addresses,
        } => match sync_now_owned(
            &addresses,
            &db,
            &creds,
            &name,
            &node,
            Some(epoch),
            owner_stop.clone(),
        ) {
            Ok(report) => {
                eprintln!(
                    "[sync] {} {} ({} words in)",
                    if report.has_more {
                        "progress saved for"
                    } else {
                        "synced"
                    },
                    short(&report.peer_node),
                    report.words_in
                );
                if report.has_more {
                    let key = (db.clone(), node.clone());
                    let request = BackgroundRequest::Peer {
                        db,
                        creds,
                        epoch,
                        owner_stop,
                        node,
                        name,
                        addresses,
                    };
                    if let Err(error) =
                        outgoing().and_then(|jobs| jobs.continue_if_clean(key, request))
                    {
                        eprintln!("[sync] page continuation: {error}");
                    }
                }
            }
            Err(error) => eprintln!("[sync] {}: {error}", short(&node)),
        },
    }
}
fn schedule_all(
    db: PathBuf,
    creds: SyncCreds,
    epoch: i64,
    delay: Duration,
    owner_stop: Option<Arc<AtomicBool>>,
) {
    if db == Path::new(":memory:") {
        return;
    }
    let db = match std::fs::canonicalize(db) {
        Ok(db) => db,
        Err(_) => {
            eprintln!("[sync] outgoing database path unavailable");
            return;
        }
    };
    let key = (db.clone(), String::new());
    let request = BackgroundRequest::Enumerate {
        db,
        creds,
        epoch,
        owner_stop,
        after: std::time::Instant::now() + delay,
        offset: 0,
        budget_reported: false,
    };
    if let Err(error) = outgoing().and_then(|jobs| jobs.submit(key, request)) {
        eprintln!("[sync] outgoing admission: {error}");
    }
}

/// GUI callers pass the epoch from their already-open writer connection at
/// the change boundary. Queued work cannot gain permission after off/on.
pub(crate) fn sync_after_change_at_epoch(db: PathBuf, creds: SyncCreds, epoch: i64) {
    schedule_all(db, creds, epoch, Duration::ZERO, None);
}
/// Compatibility entry point for callers without an existing Store.
pub fn sync_after_change(db: PathBuf, creds: SyncCreds) {
    if db == Path::new(":memory:") {
        return;
    }
    if let Ok(store) = Store::open_existing(&db) {
        sync_after_change_at_epoch(db, creds, store.sync_epoch());
    }
}
pub(crate) fn sync_on_startup_owned(
    db: PathBuf,
    creds: SyncCreds,
    epoch: i64,
    stop: Arc<AtomicBool>,
) {
    schedule_all(db, creds, epoch, Duration::from_secs(3), Some(stop));
}
pub(crate) fn sync_on_startup_at_epoch(db: PathBuf, creds: SyncCreds, epoch: i64) {
    schedule_all(db, creds, epoch, Duration::from_secs(3), None);
}
/// One catch-up round shortly after launch, with permission captured now.
pub fn sync_on_startup(db: PathBuf, creds: SyncCreds) {
    if db == Path::new(":memory:") {
        return;
    }
    if let Ok(store) = Store::open_existing(&db) {
        sync_on_startup_at_epoch(db, creds, store.sync_epoch());
    }
}

/// mDNS may observe anyone on the LAN: only known non-revoked rows get
/// address refreshes. Unknown/revoked ids never gain rows or writes here;
/// trust stays an explicit pairing decision enforced by permitted().
pub(crate) fn should_refresh_addresses(store: &Store, node_id: &str) -> bool {
    if !crate::device::valid_node_id(node_id) {
        return false;
    }
    let known = store.get_peer(node_id).unwrap_or(None).is_some();
    let revoked = store.is_revoked(node_id).unwrap_or(true);
    known && !revoked
}

/// Watch mDNS discovery: whenever a *trusted* peer newly appears on
/// the LAN, sync immediately instead of waiting for the loop.
pub fn watch_discovered(
    db_path: PathBuf,
    creds: SyncCreds,
    discovered: Arc<Mutex<HashMap<String, Discovered>>>,
    stop: Arc<AtomicBool>,
) -> Result<crate::runtime::OwnedThread, String> {
    crate::runtime::spawn_dedicated("sync-discovery-watcher", move || {
        let mut known_present: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            for _ in 0..10 {
                if crate::runtime::closing() || stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            let store = match Store::open_existing(&db_path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let discovered = discovered.lock().unwrap().clone();
            // Refresh addresses only for known non-revoked rows. Unknown or
            // revoked ids must not gain rows or address writes from mDNS;
            // trust is decided by explicit pairing, later enforced by
            // permitted()/require_sync_authorized.
            for peer in discovered.values() {
                if should_refresh_addresses(&store, &peer.node_id) {
                    let _ = store.set_peer_addresses(&peer.node_id, &peer.addresses);
                }
            }
            let trusted: std::collections::HashSet<String> = store
                .list_peers()
                .unwrap_or_default()
                .into_iter()
                .filter(|p| p.trusted)
                .map(|p| p.node_id)
                .collect();
            let present: std::collections::HashSet<String> = discovered
                .keys()
                .filter(|id| trusted.contains(*id) && !known_present.contains(*id))
                .cloned()
                .collect();
            if !present.is_empty() {
                eprintln!("[sync] peer appeared, syncing now");
                schedule_all(
                    db_path.clone(),
                    creds.clone(),
                    store.sync_epoch(),
                    Duration::ZERO,
                    Some(stop.clone()),
                );
            }
            known_present = discovered
                .keys()
                .filter(|id| trusted.contains(*id))
                .cloned()
                .collect();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceIdentity;

    /// Bounded rendezvous for fixture listeners: a peer that can never
    /// arrive (early client failure) fails the round instead of hanging
    /// the suite. 30 s is orders of magnitude above healthy localhost
    /// connect latency; expiry always means a true stall.
    fn accept_bounded(
        listener: &TcpListener,
        what: &str,
    ) -> std::io::Result<(TcpStream, std::net::SocketAddr)> {
        listener.set_nonblocking(true)?;
        let start = std::time::Instant::now();
        loop {
            match listener.accept() {
                Ok(pair) => {
                    let _ = listener.set_nonblocking(false);
                    return Ok(pair);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() > Duration::from_secs(30) {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("{what} rendezvous stalled with no peer"),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        }
    }

    #[test]
    fn stopped_engine_cancels_queued_dials_and_an_active_noise_handshake() {
        use std::sync::mpsc;
        let db = temp_db("owner-cancellation");
        let store = Store::open(&db).unwrap();
        let local = DeviceIdentity::ephemeral().creds();
        let remote = DeviceIdentity::ephemeral().creds();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        store
            .upsert_peer(&remote.node_id, "synthetic remote", &address, true)
            .unwrap();
        let stop = Arc::new(AtomicBool::new(true));
        let addresses = vec![address];
        let error = sync_now_owned(
            &addresses,
            &db,
            &local,
            "synthetic local",
            &remote.node_id,
            Some(store.sync_epoch()),
            Some(stop.clone()),
        )
        .unwrap_err();
        assert!(error.contains("owner stopped"));
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        listener.set_nonblocking(false).unwrap();
        stop.store(false, Ordering::Release);
        let (entered, ready) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            read_frame(
                &mut socket,
                std::time::Instant::now() + Duration::from_secs(2),
            )
            .unwrap();
            // Framed I/O uses short progress polls. Restore this observer's
            // bounded EOF wait; cancellation itself must still finish <1s.
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            entered.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(
                socket.read(&mut byte).unwrap(),
                0,
                "cancelled handshake retained its socket"
            );
        });
        let dial_db = db.clone();
        let dial_stop = stop.clone();
        let epoch = store.sync_epoch();
        let client = std::thread::spawn(move || {
            sync_now_owned(
                &addresses,
                &dial_db,
                &local,
                "synthetic local",
                &remote.node_id,
                Some(epoch),
                Some(dial_stop),
            )
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let cancelled = std::time::Instant::now();
        stop.store(true, Ordering::Release);
        assert!(client.join().unwrap().unwrap_err().contains("cancel"));
        server.join().unwrap();
        assert!(cancelled.elapsed() < Duration::from_secs(1));
    }
    use crate::store::Store;
    use std::net::ToSocketAddrs;
    struct TestDb {
        path: PathBuf,
        _directory: tempfile::TempDir,
    }
    impl std::ops::Deref for TestDb {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.path
        }
    }
    impl AsRef<Path> for TestDb {
        fn as_ref(&self) -> &Path {
            &self.path
        }
    }
    fn temp_db(tag: &str) -> TestDb {
        let directory = tempfile::Builder::new()
            .prefix(&format!("mywhisper-sync-{tag}-"))
            .tempdir()
            .unwrap();
        TestDb {
            path: directory.path().join("state.db"),
            _directory: directory,
        }
    }

    #[test]
    fn queued_enumeration_cannot_gain_permission_after_disable_and_reenable() {
        let db = temp_db("queued-epoch");
        let store = Store::open(&db).unwrap();
        let local = DeviceIdentity::ephemeral().creds();
        let remote = DeviceIdentity::ephemeral().creds();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        store
            .upsert_peer(
                &remote.node_id,
                "synthetic peer",
                &listener.local_addr().unwrap().to_string(),
                true,
            )
            .unwrap();
        let mut settings = store.load_settings();
        settings.sync.enabled = true;
        store.save_settings(&settings).unwrap();
        let queued_epoch = store.sync_epoch();
        settings.sync.enabled = false;
        store.save_settings(&settings).unwrap();
        settings.sync.enabled = true;
        store.save_settings(&settings).unwrap();
        assert_ne!(queued_epoch, store.sync_epoch());
        run_background(BackgroundRequest::Enumerate {
            db: db.clone(),
            creds: local,
            epoch: queued_epoch,
            owner_stop: None,
            after: std::time::Instant::now(),
            offset: 0,
            budget_reported: false,
        });
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "a stale queued trigger attempted network work"
        );
    }

    #[test]
    fn edits_during_an_active_outgoing_job_replay_over_real_encrypted_loopback() {
        use std::sync::mpsc;
        let a = DeviceIdentity::ephemeral().creds();
        let b = DeviceIdentity::ephemeral().creds();
        let db_a = temp_db("dirty-source");
        let db_b = temp_db("dirty-target");
        let store_a = Store::open(&db_a).unwrap();
        let store_b = Store::open(&db_b).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        store_a
            .upsert_peer(&b.node_id, "synthetic b", &address, true)
            .unwrap();
        store_b
            .upsert_peer(&a.node_id, "synthetic a", "", true)
            .unwrap();
        for store in [&store_a, &store_b] {
            let mut settings = store.load_settings();
            settings.sync.enabled = true;
            store.save_settings(&settings).unwrap();
        }
        store_a
            .upsert_word("synthetic initial", &a.node_id)
            .unwrap();
        let server_db = db_b.clone();
        let server_b = b.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                let stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "replay did not connect"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("synthetic listener: {error}"),
                    }
                };
                let (mut session, hash, _) =
                    NoiseStream::handshake_until(stream, &server_b.noise_static, false, deadline)
                        .unwrap();
                let store = Store::open(&server_db).unwrap();
                exchange(
                    &mut session,
                    &hash,
                    &store,
                    &server_b,
                    "synthetic b",
                    &[],
                    false,
                    None,
                    None,
                )
                .unwrap();
            }
        });
        let (entered, events) = mpsc::channel();
        let (release, waiting) = mpsc::channel();
        let waiting = Mutex::new(waiting);
        let passes = std::sync::atomic::AtomicUsize::new(0);
        let scheduler = Outgoing::new(1, 2, move |request| {
            run_background(request);
            let pass = passes.fetch_add(1, Ordering::AcqRel);
            entered.send(pass).unwrap();
            if pass == 0 {
                waiting
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
            }
        })
        .unwrap();
        let request = || BackgroundRequest::Peer {
            db: db_a.clone(),
            creds: a.clone(),
            epoch: store_a.sync_epoch(),
            owner_stop: None,
            node: b.node_id.clone(),
            name: "synthetic a".into(),
            addresses: vec![address.clone()],
        };
        let key = (db_a.clone(), b.node_id.clone());
        scheduler.submit(key.clone(), request()).unwrap();
        assert_eq!(events.recv_timeout(Duration::from_secs(3)).unwrap(), 0);
        assert_eq!(store_b.load_words().unwrap(), ["synthetic initial"]);
        store_a.upsert_word("synthetic latest", &a.node_id).unwrap();
        for _ in 0..100 {
            scheduler.submit(key.clone(), request()).unwrap();
        }
        release.send(()).unwrap();
        assert_eq!(events.recv_timeout(Duration::from_secs(3)).unwrap(), 1);
        assert!(events.recv_timeout(Duration::from_millis(50)).is_err());
        drop(scheduler);
        server.join().unwrap();
        assert_eq!(
            store_b.load_words().unwrap(),
            ["synthetic initial", "synthetic latest"]
        );
    }

    /// Full client/server round-trip over real loopback TCP with Noise
    /// encryption: B dials A, both sides converge, untrusted dialers
    /// are rejected.
    #[test]
    fn disable_and_reenable_during_handshake_never_exports_the_queued_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.db");
        let local = DeviceIdentity::ephemeral().creds();
        let remote = DeviceIdentity::ephemeral().creds();
        let store = Store::open(&path).unwrap();
        store
            .upsert_peer(&remote.node_id, "synthetic remote", "", true)
            .unwrap();
        store
            .add_history(
                "synthetic raw secret",
                "synthetic queued secret",
                3,
                1,
                &local.node_id,
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        // Client-parked rendezvous: the toggle below must land AFTER the
        // client passes its post-Hello authorization gate (epoch captured)
        // and BEFORE policy install observes it. Toggling on the server's
        // Hello completion instead races gate passage under load: a client
        // that has not reached the gate fails closed with "sync disabled"
        // (no epoch-change path) and drops the socket before the Verdict.
        let (parked_tx, parked_rx) = std::sync::mpsc::channel();
        let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
        let (resume, release) = std::sync::mpsc::channel();
        let remote_id = remote.clone();
        let server = std::thread::spawn(move || {
            let (socket, _) = accept_bounded(&listener, "handshake server").unwrap();
            let (mut session, hash, _) =
                NoiseStream::handshake(socket, &remote_id.noise_static, false).unwrap();
            let _hello: Hello = session.recv_json().unwrap();
            session
                .send_json(&Hello {
                    v: PROTOCOL_V,
                    pages: false,
                    segments: false,
                    node_id: remote_id.node_id.clone(),
                    name: "synthetic remote".into(),
                    addresses: Vec::new(),
                    introducer: false,
                    sig: DeviceIdentity::sign_session_static(&remote_id.signing, &hash).to_vec(),
                })
                .unwrap();
            release.recv_timeout(Duration::from_secs(30)).unwrap();
            session
                .send_json(&Verdict {
                    ok: true,
                    error: String::new(),
                })
                .unwrap();
            assert!(
                session.recv_json::<Payload>().is_err(),
                "old authorization must not export content after off/on"
            );
        });
        let client_path = path.clone();
        let client = std::thread::spawn(move || {
            crate::private_fs::replacement_fault::install(move |phase, _| {
                if phase == "sync-client-parked" {
                    let _ = parked_tx.send(());
                    let _ = proceed_rx.recv_timeout(Duration::from_secs(30));
                }
            });
            sync_now(
                &[address],
                &client_path,
                &local,
                "synthetic local",
                &remote.node_id,
            )
        });
        parked_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("client never parked at the authorization gate");
        let mut settings = store.load_settings();
        settings.sync.enabled = false;
        store.save_settings(&settings).unwrap();
        settings.sync.enabled = true;
        store.save_settings(&settings).unwrap();
        // Release the parked client first so it is reading when the
        // server writes the Verdict; either order is safe over TCP, but
        // this one never depends on buffer behavior.
        proceed_tx.send(()).unwrap();
        resume.send(()).unwrap();
        assert!(
            client
                .join()
                .unwrap()
                .unwrap_err()
                .contains("authorization changed")
        );
        server.join().unwrap();
    }

    #[test]
    fn disabling_sync_interrupts_an_active_stalled_encrypted_export() {
        use std::os::fd::AsRawFd;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.db");
        let local = DeviceIdentity::ephemeral().creds();
        let remote = DeviceIdentity::ephemeral().creds();
        let store = Store::open(&path).unwrap();
        store
            .upsert_peer(&remote.node_id, "synthetic remote", "", true)
            .unwrap();
        let synthetic = "synthetic-".repeat(250_000);
        store
            .add_history(&synthetic, &synthetic, 1, 1, &local.node_id)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (reached, ready) = std::sync::mpsc::channel();
        let (resume, release) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let (mut session, hash, _) =
                NoiseStream::handshake(socket, &remote.noise_static, false).unwrap();
            let _hello: Hello = session.recv_json().unwrap();
            session
                .send_json(&Hello {
                    v: PROTOCOL_V,
                    pages: false,
                    segments: false,
                    node_id: remote.node_id.clone(),
                    name: "synthetic remote".into(),
                    addresses: Vec::new(),
                    introducer: false,
                    sig: DeviceIdentity::sign_session_static(&remote.signing, &hash).to_vec(),
                })
                .unwrap();
            session
                .send_json(&Verdict {
                    ok: true,
                    error: String::new(),
                })
                .unwrap();
            session.cap = CONTENT_CAP;
            let header = session.read_chunk().unwrap();
            let total = u64::from_be_bytes(header.try_into().unwrap()) as usize;
            assert!(total > 4_000_000);
            reached.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(3)).unwrap();
            let mut received = 0;
            while let Ok(chunk) = session.read_chunk() {
                received += chunk.len();
                if received >= total {
                    break;
                }
            }
            assert!(
                received < total,
                "disabled operation must never deliver a complete content frame"
            );
        });
        let client_path = path.clone();
        let client = std::thread::spawn(move || {
            let socket = TcpStream::connect(address).unwrap();
            let size: libc::c_int = 8192;
            // SAFETY: set a bounded send buffer on this test's owned TCP socket.
            assert_eq!(
                unsafe {
                    libc::setsockopt(
                        socket.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_SNDBUF,
                        (&size as *const libc::c_int).cast(),
                        std::mem::size_of_val(&size) as libc::socklen_t,
                    )
                },
                0
            );
            let (mut session, hash, _) =
                NoiseStream::handshake(socket, &local.noise_static, true).unwrap();
            let store = Store::open(&client_path).unwrap();
            exchange(
                &mut session,
                &hash,
                &store,
                &local,
                "synthetic local",
                &[],
                true,
                None,
                None,
            )
        });
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        let mut settings = store.load_settings();
        settings.sync.enabled = false;
        store.save_settings(&settings).unwrap();
        let started = std::time::Instant::now();
        assert!(client.join().unwrap().is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        resume.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn trickled_frames_obey_whole_frame_deadline_and_reject_lengths() {
        use std::time::Instant;
        for length in [0u32, FRAME_CAP as u32 + 1] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (mut reader, _) = listener.accept().unwrap();
            sender.write_all(&length.to_be_bytes()).unwrap();
            assert_eq!(
                read_frame(&mut reader, Instant::now() + Duration::from_secs(1)).unwrap_err(),
                "bad frame length"
            );
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut reader, _) = listener.accept().unwrap();
        let worker = std::thread::spawn(move || {
            for byte in 16u32
                .to_be_bytes()
                .into_iter()
                .chain(std::iter::repeat_n(1u8, 16))
            {
                if sender.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        });
        let started = Instant::now();
        assert!(read_frame(&mut reader, started + Duration::from_millis(100)).is_err());
        assert!(started.elapsed() < Duration::from_millis(250));
        reader.shutdown(std::net::Shutdown::Both).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn discarded_payloads_converge_over_encrypted_sync_and_later_deletion() {
        let root = tempfile::tempdir().unwrap();
        let a = DeviceIdentity::ephemeral().creds();
        let b = DeviceIdentity::ephemeral().creds();
        let db_a = root.path().join("a.db");
        let db_b = root.path().join("b.db");
        let store_a = Store::open(&db_a).unwrap();
        let store_b = Store::open(&db_b).unwrap();
        for store in [&store_a, &store_b] {
            let mut settings = store.load_settings();
            settings.sync.enabled = true;
            store.save_settings(&settings).unwrap();
        }
        store_a.upsert_peer(&b.node_id, "b", "", true).unwrap();
        store_b.upsert_peer(&a.node_id, "a", "", true).unwrap();
        for (uuid, reverse) in [("synthetic-first", false), ("synthetic-second", true)] {
            let normal = crate::store::HistoryRecord {
                uuid: uuid.into(),
                text: "synthetic private speech".into(),
                raw_text: "synthetic private raw".into(),
                words: 3,
                duration_secs: 17,
                recorded_at: 123,
                deleted: false,
                discarded: false,
            };
            let discarded = crate::store::HistoryRecord {
                discarded: true,
                ..normal.clone()
            };
            store_a
                .apply_history(&[if reverse {
                    discarded.clone()
                } else {
                    normal.clone()
                }])
                .unwrap();
            store_b
                .apply_history(&[if reverse { normal } else { discarded }])
                .unwrap();
        }
        for deleted in [false, true] {
            if deleted {
                store_a.delete_history("synthetic-first").unwrap();
            }
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap().to_string();
            let server_db = db_b.clone();
            let server_b = b.clone();
            let server = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                let stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(std::time::Instant::now() < deadline);
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("synthetic listener: {error}"),
                    }
                };
                let (mut session, hash, _) =
                    NoiseStream::handshake_until(stream, &server_b.noise_static, false, deadline)
                        .unwrap();
                exchange(
                    &mut session,
                    &hash,
                    &Store::open(&server_db).unwrap(),
                    &server_b,
                    "b",
                    &[],
                    false,
                    None,
                    None,
                )
                .unwrap();
            });
            sync_now(&[address], &db_a, &a, "a", &b.node_id).unwrap();
            server.join().unwrap();
            let rows = store_a.export_history().unwrap();
            assert_eq!(rows, store_b.export_history().unwrap());
            for row in rows {
                assert!(row.discarded && row.text.is_empty() && row.raw_text.is_empty());
                assert_eq!(row.words, 0);
                assert_eq!(row.duration_secs, if row.deleted { 0 } else { 17 });
            }
        }
    }

    #[test]
    fn oversized_record_survives_throttled_encrypted_rounds_and_lost_ack() {
        use crate::store::{HistoryRecord, SyncCursor, SyncManifest, SyncPacket};
        let root = tempfile::tempdir().unwrap();
        let a = DeviceIdentity::ephemeral().creds();
        let b = DeviceIdentity::ephemeral().creds();
        let db_a = root.path().join("a.db");
        let db_b = root.path().join("b.db");
        let store_a = Store::open(&db_a).unwrap();
        let store_b = Store::open(&db_b).unwrap();
        for store in [&store_a, &store_b] {
            let mut settings = store.load_settings();
            settings.sync.enabled = true;
            store.save_settings(&settings).unwrap();
        }
        store_a
            .upsert_peer(&b.node_id, "synthetic b", "", true)
            .unwrap();
        store_b
            .upsert_peer(&a.node_id, "synthetic a", "", true)
            .unwrap();
        let original = HistoryRecord {
            uuid: "synthetic oversized live record".into(),
            text: "é🙂\\\0\n".repeat(2 * 1024 * 1024),
            raw_text: "synthetic raw".into(),
            words: 7,
            duration_secs: 4,
            deleted: false,
            discarded: false,
            recorded_at: 1_700_000_000,
        };
        store_a
            .apply_history(std::slice::from_ref(&original))
            .unwrap();
        assert!(original.text.len() > CONTENT_CAP);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let receiver_db = db_b.clone();
        let receiver_a = a.clone();
        let receiver_b = b.clone();
        let cut = std::thread::spawn(move || {
            let (socket, _) = accept_bounded(&listener, "cut server").unwrap();
            let (mut session, hash, _) =
                NoiseStream::handshake(socket, &receiver_b.noise_static, false).unwrap();
            let hello: Hello = session.recv_json().unwrap();
            assert!(hello.pages && hello.segments);
            session
                .send_json(&Hello {
                    v: PROTOCOL_V,
                    pages: true,
                    segments: true,
                    node_id: receiver_b.node_id.clone(),
                    name: "synthetic b".into(),
                    addresses: Vec::new(),
                    introducer: false,
                    sig: DeviceIdentity::sign_session_static(&receiver_b.signing, &hash).to_vec(),
                })
                .unwrap();
            let signature: [u8; 64] = hello.sig.try_into().unwrap();
            assert!(DeviceIdentity::verify_session(
                &hello.node_id,
                &hash,
                &signature
            ));
            session
                .send_json(&Verdict {
                    ok: true,
                    error: String::new(),
                })
                .unwrap();
            session.cap = CONTENT_CAP;
            let store = Store::open_existing(&receiver_db).unwrap();
            let theirs: SyncManifest = session.recv_json().unwrap();
            let ours = store
                .sync_manifest(&receiver_a.node_id, store.sync_epoch())
                .unwrap();
            session.send_json(&ours).unwrap();
            let mut outgoing: SyncCursor = session.recv_json().unwrap();
            session
                .send_json(
                    &store
                        .sync_packet_cursor(&receiver_a.node_id, store.sync_epoch(), &theirs)
                        .unwrap(),
                )
                .unwrap();
            for _ in 0..16 {
                let packet: SyncPacket = session.recv_json().unwrap();
                let received = store
                    .apply_sync_packet(
                        &receiver_a.node_id,
                        store.sync_epoch(),
                        &receiver_b.node_id,
                        &theirs,
                        &packet,
                    )
                    .unwrap();
                assert!(store.export_history().unwrap().is_empty());
                if serde_json::to_value(&received.cursor).unwrap()["partial"].is_object() {
                    drop(session);
                    return;
                }
                session.send_json(&received.cursor).unwrap();
                let packet = store
                    .export_sync_packet(
                        &receiver_a.node_id,
                        store.sync_epoch(),
                        &receiver_b.node_id,
                        &ours,
                        &outgoing,
                    )
                    .unwrap();
                session.send_json(&packet).unwrap();
                outgoing = session.recv_json().unwrap();
                assert!(packet.accepts(&outgoing));
                store
                    .acknowledge_packet(&receiver_a.node_id, store.sync_epoch(), &outgoing)
                    .unwrap();
            }
            panic!("large wire record did not begin a partial transfer");
        });
        assert!(
            sync_now(
                std::slice::from_ref(&address),
                &db_a,
                &a,
                "synthetic a",
                &b.node_id
            )
            .is_err()
        );
        cut.join().unwrap();
        store_b.checkpoint().unwrap();
        drop(store_b);
        let store_b = Store::open_existing(&db_b).unwrap();
        assert!(store_b.export_history().unwrap().is_empty());
        fn relay(mut source: TcpStream, mut target: TcpStream, budget: u64) {
            // Deterministic slow-link cut: relay at most `budget` bytes
            // this round, then break mid-transfer. Wall-clock sleeps
            // made rounds arbitrarily slow under parallel load and
            // exhausted the round/timeout budgets without exercising
            // any new protocol path; a byte budget forces partial
            // progress and lost-ACK resume deterministically on any
            // machine while keeping every assertion intact. The generous
            // 60 s socket backstop below never fires on healthy-slow
            // rounds (they take seconds even loaded); it only converts
            // a wedged peer — one that neither sends, closes, nor lets
            // the session deadline fire — from a suite hang into a
            // failed round retried from persisted offsets.
            source
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            target
                .set_write_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            let mut buffer = [0u8; 8192];
            let mut relayed: u64 = 0;
            loop {
                if relayed >= budget {
                    break;
                }
                let n = match source.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if target.write_all(&buffer[..n]).is_err() {
                    break;
                }
                relayed += n as u64;
            }
            let _ = source.shutdown(std::net::Shutdown::Both);
            let _ = target.shutdown(std::net::Shutdown::Both);
        }
        let mut interrupted = 0;
        let mut completed = false;
        for round in 0..24 {
            // Early rounds are cut after 1 MiB (mid-record for the
            // ~19 MiB fixture) to force interrupted rounds; later
            // rounds are uncut so persisted offsets must converge.
            let budget = if round < 3 { 1 << 20 } else { u64::MAX };
            let target = TcpListener::bind("127.0.0.1:0").unwrap();
            let target_address = target.local_addr().unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap().to_string();
            let proxy = std::thread::spawn(move || {
                let (client, _) = accept_bounded(&listener, "proxy client").unwrap();
                let peer = TcpStream::connect(target_address).unwrap();
                client.set_nodelay(true).unwrap();
                peer.set_nodelay(true).unwrap();
                let client_in = client.try_clone().unwrap();
                let peer_out = peer.try_clone().unwrap();
                let outbound = std::thread::spawn(move || relay(client_in, peer_out, budget));
                relay(peer, client, budget);
                outbound.join().unwrap();
            });
            let receiver_db = db_b.clone();
            let receiver_b = b.clone();
            let server = std::thread::spawn(move || {
                let (socket, _) = accept_bounded(&target, "sync server").unwrap();
                let (mut session, hash, _) =
                    NoiseStream::handshake(socket, &receiver_b.noise_static, false).unwrap();
                // The byte-budget proxy cuts below force interrupted
                // rounds deterministically; this deadline stays generous
                // so per-round progress depends on persisted offsets,
                // not on machine speed under parallel load. Production's
                // 60-second budget is not increased.
                session.deadline = std::time::Instant::now() + Duration::from_secs(30);
                let store = Store::open_existing(&receiver_db).unwrap();
                exchange(
                    &mut session,
                    &hash,
                    &store,
                    &receiver_b,
                    "synthetic b",
                    &[],
                    false,
                    None,
                    None,
                )
            });
            let result = sync_now(
                std::slice::from_ref(&address),
                &db_a,
                &a,
                "synthetic a",
                &b.node_id,
            );
            let receiver = server.join().unwrap();
            proxy.join().unwrap();
            match (result, receiver) {
                (Ok(sent), Ok(received)) if !sent.has_more && !received.has_more => {
                    completed = true;
                    break;
                }
                (left, right) => {
                    assert!(
                        left.is_err() || right.is_err(),
                        "unexpected non-progress round"
                    );
                    let ours = store_a
                        .sync_manifest(&b.node_id, store_a.sync_epoch())
                        .unwrap();
                    let theirs = store_b
                        .sync_manifest(&a.node_id, store_b.sync_epoch())
                        .unwrap();
                    eprintln!(
                        "synthetic deadline round: client={left:?} server={right:?} incoming_a={} incoming_b={}",
                        serde_json::to_value(
                            store_a
                                .sync_packet_cursor(&b.node_id, store_a.sync_epoch(), &theirs)
                                .unwrap()
                        )
                        .unwrap(),
                        serde_json::to_value(
                            store_b
                                .sync_packet_cursor(&a.node_id, store_b.sync_epoch(), &ours)
                                .unwrap()
                        )
                        .unwrap()
                    );
                    interrupted += 1;
                }
            }
        }
        assert!(
            interrupted > 0,
            "fixture never exercised its byte-budget progress cuts"
        );
        assert!(completed, "bounded slow-link retries did not converge");
        assert_eq!(
            store_b.export_history().unwrap().as_slice(),
            std::slice::from_ref(&original)
        );
        assert_eq!(store_a.export_history().unwrap(), [original]);
        for store in [&store_a, &store_b] {
            let connection = rusqlite::Connection::open(store.path().unwrap()).unwrap();
            connection
                .execute_batch(&format!("PRAGMA key=\"x'{}'\"", "2a".repeat(32)))
                .unwrap();
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM sync_transfer_parts", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn large_encrypted_history_resumes_after_committed_page_loses_ack_and_then_sends_only_deltas() {
        use crate::store::{HistoryRecord, SyncManifest, SyncPage};
        let root = tempfile::tempdir().unwrap();
        let a = DeviceIdentity::ephemeral().creds();
        let b = DeviceIdentity::ephemeral().creds();
        let db_a = root.path().join("a.db");
        let db_b = root.path().join("b.db");
        let store_a = Store::open(&db_a).unwrap();
        let store_b = Store::open(&db_b).unwrap();
        for store in [&store_a, &store_b] {
            let mut settings = store.load_settings();
            settings.sync.enabled = true;
            store.save_settings(&settings).unwrap();
        }
        store_a
            .upsert_peer(&b.node_id, "synthetic b", "", true)
            .unwrap();
        store_b
            .upsert_peer(&a.node_id, "synthetic a", "", true)
            .unwrap();
        for n in 0..9 {
            store_a
                .apply_history(&[HistoryRecord {
                    uuid: format!("synthetic-large-{n}"),
                    text: "é".repeat(512 * 1024),
                    raw_text: "Synthetic raw. ".repeat(75_000),
                    words: 1,
                    duration_secs: 1,
                    deleted: false,
                    discarded: false,
                    recorded_at: 123 + n,
                }])
                .unwrap();
        }
        let originals = store_a.export_history().unwrap();
        assert!(serde_json::to_vec(&originals).unwrap().len() > CONTENT_CAP);
        assert!(
            store_a
                .export_authorized(&b.node_id, store_a.sync_epoch())
                .unwrap_err()
                .to_string()
                .contains("upgrade the peer")
        );

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let server_db = db_b.clone();
        let server_a = a.clone();
        let server_b = b.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let (mut session, hash, _) =
                NoiseStream::handshake(stream, &server_b.noise_static, false).unwrap();
            let hello: Hello = session.recv_json().unwrap();
            assert_eq!(hello.node_id, server_a.node_id);
            assert!(hello.pages);
            session
                .send_json(&Hello {
                    v: PROTOCOL_V,
                    pages: true,
                    segments: false,
                    node_id: server_b.node_id.clone(),
                    name: "synthetic b".into(),
                    addresses: vec![],
                    introducer: false,
                    sig: DeviceIdentity::sign_session_static(&server_b.signing, &hash).to_vec(),
                })
                .unwrap();
            session
                .send_json(&Verdict {
                    ok: true,
                    error: String::new(),
                })
                .unwrap();
            session.cap = CONTENT_CAP;
            let store = Store::open_existing(&server_db).unwrap();
            let incoming: SyncManifest = session.recv_json().unwrap();
            let ours = store
                .sync_manifest(&server_a.node_id, store.sync_epoch())
                .unwrap();
            session.send_json(&ours).unwrap();
            let remote_cursor: i64 = session.recv_json().unwrap();
            assert_eq!(remote_cursor, 0);
            session.send_json(&0i64).unwrap();
            let page: SyncPage = session.recv_json().unwrap();
            assert!(!page.done);
            assert!(serde_json::to_vec(&page).unwrap().len() < CONTENT_CAP);
            store
                .apply_sync_page(
                    &server_a.node_id,
                    store.sync_epoch(),
                    &server_b.node_id,
                    &incoming,
                    &page,
                )
                .unwrap();
            assert!(
                store
                    .sync_received_cursor(&server_a.node_id, store.sync_epoch(), &incoming)
                    .unwrap()
                    > 0
            );
            // Kill the connection after the durable commit and before ACK.
            // Restarted peers must ask for the persisted cursor, not resend
            // the first page or lose records from the unfinished page.
            drop(session);
            page.content.history.len()
        });
        assert!(
            sync_now(
                std::slice::from_ref(&address),
                &db_a,
                &a,
                "synthetic a",
                &b.node_id
            )
            .is_err()
        );
        let first_count = server.join().unwrap();
        assert!(first_count > 0 && first_count < 9);
        assert_eq!(store_b.export_history().unwrap().len(), first_count);
        let round = || {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap().to_string();
            let server_db = db_b.clone();
            let server_b = b.clone();
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let (mut session, hash, _) =
                    NoiseStream::handshake(stream, &server_b.noise_static, false).unwrap();
                let store = Store::open_existing(&server_db).unwrap();
                exchange(
                    &mut session,
                    &hash,
                    &store,
                    &server_b,
                    "synthetic b",
                    &[],
                    false,
                    None,
                    None,
                )
                .unwrap()
            });
            let result = sync_now(&[address], &db_a, &a, "synthetic a", &b.node_id).unwrap();
            (result, server.join().unwrap())
        };
        let (outbound, inbound) = round();
        assert!(!outbound.has_more && !inbound.has_more);
        assert_eq!(inbound.history_in, 9 - first_count);
        let mut expected = originals;
        expected.sort_by(|a, b| a.uuid.cmp(&b.uuid));
        let mut actual = store_b.export_history().unwrap();
        actual.sort_by(|a, b| a.uuid.cmp(&b.uuid));
        assert_eq!(
            actual, expected,
            "every Unicode transcript must survive restart/resume exactly"
        );
        // The responder snapshot was fixed before receiving the unfinished
        // records. Those two fresh local changes are eligible in its next
        // snapshot; acknowledge them once before checking the steady state.
        let (outbound, inbound) = round();
        assert_eq!(
            (outbound.history_in, inbound.history_in),
            (9 - first_count, 0)
        );
        let (outbound, inbound) = round();
        assert_eq!(
            (outbound.history_in, inbound.history_in),
            (0, 0),
            "unchanged histories must not be resent"
        );
        store_a.delete_history("synthetic-large-0").unwrap();
        store_a
            .upsert_word("synthetic delta after resume", &a.node_id)
            .unwrap();
        let (_, inbound) = round();
        assert_eq!((inbound.history_in, inbound.words_in), (1, 1));
        let tombstone = store_b
            .export_history()
            .unwrap()
            .into_iter()
            .find(|row| row.uuid == "synthetic-large-0")
            .unwrap();
        assert!(tombstone.deleted && tombstone.text.is_empty() && tombstone.raw_text.is_empty());
        // A fresh encrypted store can legitimately retain its device key.
        // Its new stream identity and empty receive cursor require all data,
        // including records it previously supplied to the other peer.
        store_b.checkpoint().unwrap();
        drop(store_b);
        std::fs::rename(&db_b, root.path().join("preserved-b.db")).unwrap();
        let fresh_b = Store::open(&db_b).unwrap();
        let mut settings = fresh_b.load_settings();
        settings.sync.enabled = true;
        fresh_b.save_settings(&settings).unwrap();
        fresh_b
            .upsert_peer(&a.node_id, "synthetic a", "", true)
            .unwrap();
        let (_, inbound) = round();
        assert_eq!(inbound.history_in, 9);
        let mut recovered = fresh_b.export_history().unwrap();
        recovered.sort_by(|a, b| a.uuid.cmp(&b.uuid));
        let mut current = store_a.export_history().unwrap();
        current.sort_by(|a, b| a.uuid.cmp(&b.uuid));
        assert_eq!(
            recovered, current,
            "fresh peer recovery must include formerly shared data and tombstones"
        );
    }

    #[test]
    fn escaped_json_limit_rejects_before_wire_progress_and_preserves_next_unicode_message() {
        let a = DeviceIdentity::ephemeral().creds();
        let b = DeviceIdentity::ephemeral().creds();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let (mut session, _, _) =
                NoiseStream::handshake(socket, &b.noise_static, false).unwrap();
            session.cap = 64;
            session.recv_json::<String>().unwrap()
        });
        let socket = TcpStream::connect(address).unwrap();
        let (mut session, _, _) = NoiseStream::handshake(socket, &a.noise_static, true).unwrap();
        session.cap = 64;
        assert!(session.send_json(&"\0".repeat(100)).is_err());
        session.send_json(&"🦀 é synthetic").unwrap();
        assert_eq!(server.join().unwrap(), "🦀 é synthetic");
    }

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
            let store = Store::open(&db_a2).unwrap();
            exchange(
                &mut session,
                &hash,
                &store,
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
            let store = Store::open(&db_a3).unwrap();
            exchange(
                &mut session,
                &hash,
                &store,
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
            let store = Store::open(&db_v2).unwrap();

            exchange(
                &mut session,
                &hash,
                &store,
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
            pages: false,
            segments: false,
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

    #[test]
    fn discovery_refresh_only_touches_known_non_revoked_peers() {
        let db = temp_db("discovery-guard");
        let store = Store::open(&db).unwrap();
        let trusted = DeviceIdentity::ephemeral().creds();
        let revoked = DeviceIdentity::ephemeral().creds();
        let unknown = DeviceIdentity::ephemeral().creds();
        store
            .upsert_peer(&trusted.node_id, "synthetic trusted", "", true)
            .unwrap();
        store
            .upsert_peer(&revoked.node_id, "synthetic revoked", "", true)
            .unwrap();
        store.remove_peer(&revoked.node_id).unwrap();
        assert!(should_refresh_addresses(&store, &trusted.node_id));
        assert!(!should_refresh_addresses(&store, &revoked.node_id));
        assert!(!should_refresh_addresses(&store, &unknown.node_id));
        assert!(!should_refresh_addresses(&store, "not-a-node-id"));
        // Unknown ids gain no rows from address refresh alone.
        assert!(store.get_peer(&unknown.node_id).unwrap().is_none());
    }
}

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
//! Reachability v1: same LAN (mDNS/discovered/gossiped addresses),
//! VPN, or internet with a known address. Hole punching and relays
//! (libp2p-class) are the later internet phase.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::device::{DeviceIdentity, SyncCreds};
use crate::store::{HistoryRecord, PeerGossip, SnippetRecord, Store, WordRecord};

const PROTOCOL_V: u32 = 1;
const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const NOISE_BUF: usize = 65535;
const CHUNK: usize = 60000;
const FRAME_CAP: usize = 4 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const AUTO_SYNC_SECS: u64 = 60;
const MDNS_SERVICE: &str = "_mywhisper._tcp.local.";

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

fn write_frame(stream: &mut TcpStream, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > FRAME_CAP {
        return Err("frame too large".to_string());
    }
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .map_err(|e| format!("write: {e}"))?;
    stream.write_all(bytes).map_err(|e| format!("write: {e}"))
}

fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut len = [0u8; 4];
    stream
        .read_exact(&mut len)
        .map_err(|e| format!("read: {e}"))?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > FRAME_CAP {
        return Err("bad frame length".to_string());
    }
    let mut buf = vec![0u8; len];
    stream
        .read_exact(&mut buf)
        .map_err(|e| format!("read: {e}"))?;
    Ok(buf)
}

/// One encrypted session: Noise_XX handshake, then length-framed
/// chunked messages over the transport.
pub struct NoiseStream {
    stream: TcpStream,
    transport: snow::TransportState,
    rbuf: Vec<u8>,
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
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(|e| format!("timeout: {e}"))?;
        let mut hs = Self::builder(private_key, initiator)?;
        let mut buf = vec![0u8; NOISE_BUF];
        let mut payload = vec![0u8; NOISE_BUF];
        if initiator {
            // XX initiator: write, read, write.
            loop {
                let n = hs
                    .write_message(&[], &mut buf)
                    .map_err(|e| format!("handshake write: {e}"))?;
                write_frame(&mut stream, &buf[..n])?;
                if hs.is_handshake_finished() {
                    break;
                }
                let msg = read_frame(&mut stream)?;
                hs.read_message(&msg, &mut payload)
                    .map_err(|e| format!("handshake read: {e}"))?;
                if hs.is_handshake_finished() {
                    break;
                }
            }
        } else if {
            loop {
                let msg = read_frame(&mut stream)?;
                hs.read_message(&msg, &mut payload)
                    .map_err(|e| format!("handshake read: {e}"))?;
                if hs.is_handshake_finished() {
                    break;
                }
                let n = hs
                    .write_message(&[], &mut buf)
                    .map_err(|e| format!("handshake write: {e}"))?;
                write_frame(&mut stream, &buf[..n])?;
            }
            false
        } {
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
            },
            hash,
            remote,
        ))
    }

    fn write_chunk(&mut self, plaintext: &[u8]) -> Result<(), String> {
        let mut buf = vec![0u8; plaintext.len() + 32];
        let n = self
            .transport
            .write_message(plaintext, &mut buf)
            .map_err(|e| format!("encrypt: {e}"))?;
        write_frame(&mut self.stream, &buf[..n])
    }

    /// Send one logical message (chunked at 60 KiB plaintext).
    pub fn send_msg(&mut self, data: &[u8]) -> Result<(), String> {
        let total = data.len() as u64;
        self.write_chunk(&total.to_be_bytes())?;
        for chunk in data.chunks(CHUNK) {
            self.write_chunk(chunk)?;
        }
        Ok(())
    }

    pub fn send_json(&mut self, value: &impl Serialize) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|e| format!("encode: {e}"))?;
        self.send_msg(&bytes)
    }

    fn read_chunk(&mut self) -> Result<Vec<u8>, String> {
        let frame = read_frame(&mut self.stream)?;
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
        if total > 256 * 1024 * 1024 {
            return Err("message too large".to_string());
        }
        let mut out = Vec::with_capacity(total.min(1024 * 1024));
        while out.len() < total {
            out.extend_from_slice(&self.read_chunk()?);
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

fn gossip_out(store: &Store, self_id: &str) -> Vec<PeerGossip> {
    store
        .export_gossip()
        .unwrap_or_default()
        .into_iter()
        .filter(|g| g.node_id != self_id)
        .collect()
}

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
    if let Some(expected) = expect_node {
        if peer.node_id != expected {
            return Err("peer node id mismatch".to_string());
        }
    }

    // Contact bookkeeping: unknown nodes become pending requests,
    // known ones refresh addresses.
    let mut contact_addrs: Vec<String> = peer.addresses.clone();
    if let Some(remote) = remote {
        if !contact_addrs.contains(&remote) {
            contact_addrs.push(remote);
        }
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

    // Records, strict alternation again.
    let ours = Payload {
        words: store.export_words().map_err(|e| format!("export: {e}"))?,
        snippets: store
            .export_snippets()
            .map_err(|e| format!("export: {e}"))?,
        history: store.export_history().map_err(|e| format!("export: {e}"))?,
        peers: gossip_out(store, &creds.node_id),
    };
    let theirs: Payload = if initiator {
        session.send_json(&ours)?;
        session.recv_json()?
    } else {
        let theirs: Payload = session.recv_json()?;
        session.send_json(&ours)?;
        theirs
    };

    // Merge content.
    store
        .apply_words(&theirs.words)
        .map_err(|e| format!("merge words: {e}"))?;
    store
        .apply_snippets(&theirs.snippets)
        .map_err(|e| format!("merge snippets: {e}"))?;
    store
        .apply_history(&theirs.history)
        .map_err(|e| format!("merge history: {e}"))?;
    // Merge introductions: strangers they vouch for join only when we
    // marked the sender an introducer.
    let sender_introducer = store
        .get_peer(&peer.node_id)
        .map_err(|e| format!("peer lookup: {e}"))?
        .map(|p| p.introducer)
        .unwrap_or(false);
    let introduced = store
        .apply_gossip(&theirs.peers, sender_introducer, &creds.node_id)
        .map_err(|e| format!("merge peers: {e}"))?;

    store
        .touch_peer_synced(&peer.node_id, &peer.name)
        .map_err(|e| format!("record sync: {e}"))?;
    Ok(SyncReport {
        peer_node: peer.node_id,
        peer_name: peer.name,
        words_in: theirs.words.len(),
        snippets_in: theirs.snippets.len(),
        history_in: theirs.history.len(),
        introduced,
    })
}

fn short(node_id: &str) -> String {
    if node_id.len() > 12 {
        format!("{}…{}", &node_id[..8], &node_id[node_id.len() - 4..])
    } else {
        node_id.to_string()
    }
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
    if addresses.is_empty() {
        return Err("no known address for peer".to_string());
    }
    let mut last_err = String::new();
    for address in addresses {
        let addr = match address
            .to_socket_addrs()
            .map_err(|e| format!("resolve {address}: {e}"))?
            .next()
        {
            Some(a) => a,
            None => {
                last_err = format!("no address for {address}");
                continue;
            }
        };
        let stream = match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(s) => s,
            Err(e) => {
                last_err = format!("dial {address}: {e}");
                continue;
            }
        };
        let (mut session, hash, _remote_static) =
            NoiseStream::handshake(stream, &creds.noise_static, true)?;
        let store = Store::open(db_path).map_err(|e| format!("open db: {e}"))?;
        return exchange(
            &mut session,
            &hash,
            &store,
            creds,
            name,
            &[],
            true,
            Some(expect_node),
            None,
        );
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
) {
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
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
                std::thread::spawn(move || {
                    let res = (|| -> Result<SyncReport, String> {
                        let (mut session, hash, _remote_static) =
                            NoiseStream::handshake(stream, &creds.noise_static, false)?;
                        let store = Store::open(&db_path).map_err(|e| format!("open db: {e}"))?;
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

fn firewall_query(args: &[&str]) -> Option<bool> {
    let out = std::process::Command::new("firewall-cmd")
        .args(args)
        .output()
        .ok()?;
    Some(out.status.success())
}

/// Local firewall state for our sync port. `None` when firewalld is
/// absent/unreadable. Returns `(port_open, mdns_allowed)`.
///
/// NOTE: on systems with polkit auth for firewalld queries this call
/// can pop an auth dialog, so callers must cache it process-wide
/// (see `firewall_state`), never per click/page.
pub fn firewall_state_uncached(port: u16) -> Option<(bool, bool)> {
    let arg = format!("--query-port={port}/tcp");
    let port_open = firewall_query(&[&arg])?;
    let mdns = firewall_query(&["--query-service=mdns"])?;
    Some((port_open, mdns))
}

static FIREWALL_CACHE: std::sync::OnceLock<(u16, Option<(bool, bool)>)> =
    std::sync::OnceLock::new();

/// Process-wide cached firewall state for one port. Firewall rules
/// effectively never change mid-session, and every query risks a
/// polkit auth dialog, so this runs exactly once.
pub fn firewall_state(port: u16) -> Option<(bool, bool)> {
    FIREWALL_CACHE
        .get_or_init(|| (port, firewall_state_uncached(port)))
        .1
}

// -- mDNS ----------------------------------------------------------------

pub struct MdnsHandles {
    _daemon: mdns_sd::ServiceDaemon,
}

/// Advertise this node on the LAN and browse for peers in the
/// background. Discovered nodes land in `sink` (deduplicated by node
/// id); the caller polls it to refresh the UI.
pub fn start_mdns(
    node_id: &str,
    name: &str,
    port: u16,
    sink: Arc<Mutex<HashMap<String, Discovered>>>,
) -> Result<MdnsHandles, String> {
    let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| format!("mdns: {e}"))?;
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

    let browser = daemon
        .browse(MDNS_SERVICE)
        .map_err(|e| format!("mdns browse: {e}"))?;
    let self_id = node_id.to_string();
    std::thread::spawn(move || {
        while let Ok(event) = browser.recv() {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                    let node = info
                        .get_property_val_str("node")
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    if node.is_empty() || node == self_id {
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
                        .map(|ip| format!("{ip}:{port}"))
                        .collect();
                    if addresses.is_empty() {
                        continue;
                    }
                    eprintln!("[sync] mdns found {name} {addresses:?}");
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
                }
                _ => {}
            }
        }
    });
    Ok(MdnsHandles { _daemon: daemon })
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
pub fn auto_sync_loop(db_path: PathBuf, creds: SyncCreds) {
    loop {
        std::thread::sleep(Duration::from_secs(AUTO_SYNC_SECS));
        for line in sync_all_trusted(&db_path, &creds) {
            eprintln!("[sync] auto: {line}");
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
) {
    std::thread::spawn(move || {
        let mut known_present: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            std::thread::sleep(Duration::from_secs(10));
            let store = match Store::open(&db_path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let trusted: std::collections::HashSet<String> = store
                .list_peers()
                .unwrap_or_default()
                .into_iter()
                .filter(|p| p.trusted)
                .map(|p| p.node_id)
                .collect();
            let present: std::collections::HashSet<String> = discovered
                .lock()
                .unwrap()
                .keys()
                .filter(|id| trusted.contains(*id) && !known_present.contains(*id))
                .cloned()
                .collect();
            if !present.is_empty() {
                eprintln!("[sync] peer appeared, syncing now");
                for line in sync_all_trusted(&db_path, &creds) {
                    eprintln!("[sync] on-appear: {line}");
                }
            }
            known_present = discovered
                .lock()
                .unwrap()
                .keys()
                .filter(|id| trusted.contains(*id))
                .cloned()
                .collect();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceIdentity;
    use crate::store::Store;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_db(tag: &str) -> PathBuf {
        let n = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("mywhisper-sync-test-{pid}-{n}-{tag}.db"))
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
            let res = exchange(
                &mut session,
                &hash,
                &store,
                &creds_vk,
                "v",
                &[],
                false,
                None,
                None,
            );
            res
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
}

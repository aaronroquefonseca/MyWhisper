//! Node identity for device-to-device sync.
//!
//! Each device owns two keypairs in `device.key` (mode 0600):
//!
//! * ed25519 (32-byte seed): the long-term identity. The node id is
//!   the hex-encoded public key; records attribute versions to it.
//! * x25519 (32-byte static): the Noise transport key, kept separate
//!   from the signing key on purpose (no cross-protocol key reuse).
//!   Legacy 32-byte key files gain a fresh x25519 key on first load.

use ed25519_dalek::{SigningKey, VerifyingKey};
use std::path::Path;

pub struct DeviceIdentity {
    pub node_id: String,
    signing_key: SigningKey,
    noise_static: [u8; 32],
}

/// Portable credentials for worker threads (all `Send`).
#[derive(Clone)]
pub struct SyncCreds {
    pub node_id: String,
    pub noise_static: [u8; 32],
    pub signing: SigningKey,
}

impl DeviceIdentity {
    /// Load the node key, creating and persisting one on first run.
    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        let _lock = crate::private_fs::lock(&path.with_extension("key.lock"))?;
        let bytes = match crate::private_fs::read_private(path, 64) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        let mut both = [0u8; 64];
        match bytes.len() {
            64 => both.copy_from_slice(&bytes),
            32 => {
                both[..32].copy_from_slice(&bytes);
                getrandom::fill(&mut both[32..])
                    .map_err(|e| std::io::Error::other(format!("rng: {e}")))?;
                crate::private_fs::replace(path, &both)?;
            }
            0 => {
                // An existing empty file is corruption, never a new identity.
                if path.exists() {
                    return Err(std::io::Error::other("empty device key"));
                }
                getrandom::fill(&mut both)
                    .map_err(|e| std::io::Error::other(format!("rng: {e}")))?;
                crate::private_fs::replace(path, &both)?;
            }
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "bad device key length",
                ));
            }
        }
        Ok(Self::from_keys(&both))
    }

    fn from_keys(both: &[u8; 64]) -> Self {
        let mut seed = [0u8; 32];
        let mut noise_static = [0u8; 32];
        seed.copy_from_slice(&both[..32]);
        noise_static.copy_from_slice(&both[32..]);
        let signing_key = SigningKey::from_bytes(&seed);
        let verifying = VerifyingKey::from(&signing_key);
        Self {
            node_id: hex(verifying.as_bytes()),
            signing_key,
            noise_static,
        }
    }

    #[allow(dead_code)]
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Raw x25519 static private key for the Noise handshake.
    pub fn noise_static(&self) -> &[u8; 32] {
        &self.noise_static
    }

    /// Snapshot thread-safe credentials for sync workers.
    pub fn creds(&self) -> SyncCreds {
        SyncCreds {
            node_id: self.node_id.clone(),
            noise_static: self.noise_static,
            signing: self.signing_key.clone(),
        }
    }

    /// Sign handshake bytes, proving ownership of this node id for
    /// exactly this session (replay across sessions fails: every
    /// Noise handshake hashes fresh ephemerals).
    pub fn sign_session(&self, handshake_hash: &[u8]) -> [u8; 64] {
        Self::sign_session_static(&self.signing_key, handshake_hash)
    }

    pub fn sign_session_static(signing: &SigningKey, handshake_hash: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        signing.sign(handshake_hash).to_bytes()
    }

    /// Verify a peer's session proof against its claimed node id.
    pub fn verify_session(node_id_hex: &str, handshake_hash: &[u8], sig: &[u8; 64]) -> bool {
        use ed25519_dalek::Verifier;
        let raw = match hex_decode(node_id_hex) {
            Some(b) => b,
            None => return false,
        };
        let verifying = match VerifyingKey::from_bytes(&raw) {
            Ok(v) => v,
            Err(_) => return false,
        };
        verifying
            .verify(handshake_hash, &ed25519_dalek::Signature::from_bytes(sig))
            .is_ok()
    }

    /// Throwaway identity for tests and in-memory previews.
    pub fn ephemeral() -> Self {
        let mut both = [0u8; 64];
        getrandom::fill(&mut both).expect("rng");
        Self::from_keys(&both)
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn hex_decode(s: &str) -> Option<[u8; 32]> {
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

const NAME_ADJECTIVES: [&str; 32] = [
    "Agile",
    "Amber",
    "Arctic",
    "Astral",
    "Bold",
    "Brave",
    "Bright",
    "Calm",
    "Cedar",
    "Clever",
    "Coral",
    "Crimson",
    "Daring",
    "Dawn",
    "Ebony",
    "Ember",
    "Fleet",
    "Frost",
    "Gentle",
    "Glacier",
    "Harbor",
    "Hazel",
    "Ivory",
    "Jade",
    "Keen",
    "Lunar",
    "Maple",
    "Nimble",
    "Onyx",
    "Quicksilver",
    "Rapid",
    "Solar",
];

const NAME_NOUNS: [&str; 32] = [
    "Albatross",
    "Badger",
    "Beacon",
    "Bison",
    "Comet",
    "Crane",
    "Dingo",
    "Eagle",
    "Falcon",
    "Fern",
    "Fox",
    "Giraffe",
    "Harbor",
    "Heron",
    "Ibex",
    "Ibis",
    "Jackal",
    "Jaguar",
    "Kestrel",
    "Koala",
    "Lark",
    "Lynx",
    "Mesa",
    "Otter",
    "Owl",
    "Pine",
    "Quail",
    "Raven",
    "Reef",
    "Sparrow",
    "Wolf",
    "Wren",
];

/// Memorable default device name, deterministic per node id so it is
/// stable across restarts until the user renames the device.
pub fn default_device_name(node_id: &str) -> String {
    fn hex_byte(s: &str, at: usize) -> usize {
        let h = |c: u8| match c {
            b'0'..=b'9' => (c - b'0') as usize,
            b'a'..=b'f' => (c - b'a') as usize + 10,
            b'A'..=b'F' => (c - b'A') as usize + 10,
            _ => 0,
        };
        let b = s.as_bytes();
        if at + 1 >= b.len() {
            return 0;
        }
        (h(b[at]) << 4) | h(b[at + 1])
    }
    let a = hex_byte(node_id, 0) % NAME_ADJECTIVES.len();
    let b = hex_byte(node_id, 2) % NAME_NOUNS.len();
    format!("{}-{}", NAME_ADJECTIVES[a], NAME_NOUNS[b])
}

/// Display name: explicit setting wins, otherwise the generated one.
pub fn display_name(settings_name: &str, node_id: &str) -> String {
    if settings_name.is_empty() {
        default_device_name(node_id)
    } else {
        settings_name.to_string()
    }
}

/// Canonical identity accepted at discovery and gossip boundaries.
pub fn valid_node_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn short_id(id: &str) -> String {
    if id.chars().count() <= 16 {
        return id.to_string();
    }
    let head: String = id.chars().take(8).collect();
    let tail: String = id
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod provisioning_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Barrier};

    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if !matches!(self.0.try_wait(), Ok(Some(_))) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    fn identity_fingerprint(identity: &DeviceIdentity) -> String {
        use sha2::{Digest, Sha256};
        // Only synthetic credentials are used. Never print or write the
        // transport private key into a test result/log.
        format!(
            "{}:{}",
            identity.node_id,
            hex(&Sha256::digest(identity.noise_static()))
        )
    }

    fn child(root: &Path, action: &str) -> OwnedChild {
        OwnedChild(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "device::provisioning_tests::interrupted_replacement_and_fresh_processes_preserve_both_keys",
                    "--nocapture",
                ])
                .env("MYWHISPER_IDENTITY_TEST_ROOT", root)
                .env("MYWHISPER_IDENTITY_TEST_ACTION", action)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn wait_child(child: &mut OwnedChild) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success(), "identity child failed: {status}");
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "identity child stalled"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn interrupted_replacement_and_fresh_processes_preserve_both_keys() {
        use std::io::Write;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        use std::os::unix::process::ExitStatusExt;
        use std::time::{Duration, Instant};

        if let Some(root) = std::env::var_os("MYWHISPER_IDENTITY_TEST_ROOT") {
            let root = std::path::PathBuf::from(root);
            let metadata = std::fs::symlink_metadata(&root).unwrap();
            assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
            assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
            assert!(
                root.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("mywhisper-identity-audit.")
            );
            assert!(root.join(".disposable").is_file());
            let action = std::env::var("MYWHISPER_IDENTITY_TEST_ACTION").unwrap();
            let path = root.join("device.key");
            if let Some(phase) = action.strip_prefix("stop:") {
                let phase = phase.to_owned();
                let ready = root.join("ready");
                let target = path.clone();
                crate::private_fs::replacement_fault::install(move |point, path| {
                    if path == target && point == phase {
                        let mut marker = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(&ready)
                            .unwrap();
                        marker.write_all(b"owned replacement reached").unwrap();
                        marker.sync_all().unwrap();
                        // The parent SIGKILLs this exact retained Child. A
                        // failed parent cannot leave this process indefinitely.
                        std::thread::sleep(Duration::from_secs(5));
                        panic!("interruption barrier was not killed by its owner");
                    }
                });
            }
            let identity = DeviceIdentity::load_or_create(&path).unwrap();
            let mut report = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(root.join(action))
                .unwrap();
            report
                .write_all(identity_fingerprint(&identity).as_bytes())
                .unwrap();
            return;
        }

        for legacy in [false, true] {
            for phase in [
                "created",
                "written",
                "file-synced",
                "renamed",
                "directory-synced",
            ] {
                let root = tempfile::Builder::new()
                    .prefix("mywhisper-identity-audit.")
                    .tempdir()
                    .unwrap();
                std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                std::fs::write(root.path().join(".disposable"), b"").unwrap();
                let path = root.path().join("device.key");
                if legacy {
                    std::fs::write(&path, [7; 32]).unwrap();
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                        .unwrap();
                }
                let mut interrupted = child(root.path(), &format!("stop:{phase}"));
                let deadline = Instant::now() + Duration::from_secs(3);
                while !root.path().join("ready").is_file() {
                    assert!(
                        interrupted.0.try_wait().unwrap().is_none(),
                        "child exited before replacement barrier"
                    );
                    assert!(
                        Instant::now() < deadline,
                        "replacement barrier never reached"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                let published = matches!(phase, "renamed" | "directory-synced");
                let before = if path.exists() {
                    Some(std::fs::read(&path).unwrap())
                } else {
                    None
                };
                assert_eq!(
                    before.as_ref().map(Vec::len),
                    if published {
                        Some(64)
                    } else if legacy {
                        Some(32)
                    } else {
                        None
                    }
                );
                if let Some(bytes) = &before {
                    assert_eq!(
                        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                    if legacy {
                        assert_eq!(&bytes[..32], &[7; 32]);
                    }
                }
                for entry in std::fs::read_dir(root.path()).unwrap() {
                    let entry = entry.unwrap();
                    if entry.file_name() != ".disposable" {
                        assert_eq!(
                            entry.metadata().unwrap().permissions().mode() & 0o777,
                            0o600,
                            "interrupted material must already be private"
                        );
                    }
                }
                interrupted.0.kill().unwrap();
                assert_eq!(interrupted.0.wait().unwrap().signal(), Some(libc::SIGKILL));

                // Hold the same real initialization lock until every fresh
                // process has opened it. This guarantees process contention,
                // rather than relying on spawn timing to establish overlap.
                let lock_path = path.with_extension("key.lock");
                let gate = crate::private_fs::lock(&lock_path).unwrap();
                let mut creators: Vec<_> = (0..8)
                    .map(|index| child(root.path(), &format!("report-{index}")))
                    .collect();
                let admission = Instant::now() + Duration::from_secs(3);
                for creator in &mut creators {
                    loop {
                        assert!(creator.0.try_wait().unwrap().is_none());
                        let descriptors =
                            std::fs::read_dir(format!("/proc/{}/fd", creator.0.id())).unwrap();
                        if descriptors.filter_map(Result::ok).any(|descriptor| {
                            std::fs::read_link(descriptor.path()).ok().as_ref() == Some(&lock_path)
                        }) {
                            break;
                        }
                        assert!(
                            Instant::now() < admission,
                            "fresh process did not reach its lock"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                drop(gate);
                // Each process reports both keys, not just the signing ID.
                for creator in &mut creators {
                    wait_child(creator);
                }
                let winner = DeviceIdentity::load_or_create(&path).unwrap();
                let expected = identity_fingerprint(&winner);
                for index in 0..8 {
                    assert_eq!(
                        std::fs::read_to_string(root.path().join(format!("report-{index}")))
                            .unwrap(),
                        expected
                    );
                }
                let persisted = std::fs::read(&path).unwrap();
                assert_eq!(persisted.len(), 64);
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                if published {
                    assert_eq!(
                        Some(&persisted),
                        before.as_ref(),
                        "recovery must never replace a published identity"
                    );
                }
                if legacy {
                    assert_eq!(&persisted[..32], &[7; 32]);
                }
                let mut reopened = child(root.path(), "fresh-reopen");
                wait_child(&mut reopened);
                assert_eq!(
                    std::fs::read_to_string(root.path().join("fresh-reopen")).unwrap(),
                    expected
                );
            }
        }
    }

    #[test]
    fn concurrent_create_and_legacy_migration_share_one_private_identity() {
        for legacy in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("device.key");
            if legacy {
                std::fs::write(&path, [7u8; 32]).unwrap();
            }
            let gate = Arc::new(Barrier::new(12));
            let workers: Vec<_> = (0..12)
                .map(|_| {
                    let gate = gate.clone();
                    let path = path.clone();
                    std::thread::spawn(move || {
                        gate.wait();
                        DeviceIdentity::load_or_create(&path).unwrap().node_id
                    })
                })
                .collect();
            let persisted = workers
                .into_iter()
                .map(|w| w.join().unwrap())
                .collect::<Vec<_>>();
            let winner = DeviceIdentity::load_or_create(&path).unwrap();
            assert!(persisted.iter().all(|id| id == &winner.node_id));
            assert_eq!(std::fs::read(&path).unwrap().len(), 64);
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            if legacy {
                assert_eq!(&std::fs::read(&path).unwrap()[..32], &[7u8; 32]);
            }
        }
    }
    #[test]
    fn invalid_identity_is_preserved_and_display_is_unicode_safe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, b"partial").unwrap();
        assert!(DeviceIdentity::load_or_create(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"partial");
        assert_eq!(
            short_id(&"界".repeat(20)),
            format!("{}…{}", "界".repeat(8), "界".repeat(4))
        );
        assert!(!valid_node_id(&"界".repeat(20)));
    }
}

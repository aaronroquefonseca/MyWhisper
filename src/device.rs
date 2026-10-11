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

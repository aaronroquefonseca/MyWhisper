//! Bounded Secret Service access without background unlock prompts.
//!
//! Provisioning is serialized by the caller's cross-process lock. New items
//! never replace an existing key: a delayed interrupted CreateItem may leave
//! duplicate candidates, but cannot overwrite the key used by a database.
//!
//! Linux talks to the D-Bus Secret Service directly through
//! `dbus-secret-service` with a zero prompt timeout, so a locked collection
//! fails fast instead of activating an unlock prompt. Lookups match the
//! `service=mywhisper`/`username=state-db` attributes, so items written by the
//! earlier zbus and `keyring-core` backends stay readable. macOS and Windows
//! keep the native `keyring-core` stores.
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(5);
const LOCKED: &str = "database keyring is locked; unlock the login keyring and retry";
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
const UNAVAILABLE: &str = "Secret Service unavailable; start or unlock the login keyring and retry";
const MISSING: &str =
    "encrypted database exists but its key is missing; restore the keyring secret";

pub(crate) fn candidates(existing_encrypted: bool) -> Result<Vec<[u8; 32]>, String> {
    bounded(move || fetch(existing_encrypted))
}

/// Blocking keyring calls run on a helper thread so the caller gets an answer
/// within `TIMEOUT` even if the service stalls. An abandoned call can at most
/// add a duplicate write-once candidate, which key selection tolerates.
fn bounded(
    operation: impl FnOnce() -> Result<Vec<[u8; 32]>, String> + Send + 'static,
) -> Result<Vec<[u8; 32]>, String> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("database-key".into())
        .spawn(move || {
            let _ = sender.send(operation());
        })
        .map_err(|_| "keyring worker unavailable".to_string())?;
    receiver.recv_timeout(TIMEOUT).unwrap_or_else(|_| {
        Err("Secret Service deadline exceeded; unlock the login keyring and retry".to_string())
    })
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn fetch(existing_encrypted: bool) -> Result<Vec<[u8; 32]>, String> {
    fetch_with(existing_encrypted, dbus_secret_service::EncryptionType::Dh).map_err(str::to_string)
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn fetch_with(
    existing_encrypted: bool,
    encryption: dbus_secret_service::EncryptionType,
) -> Result<Vec<[u8; 32]>, &'static str> {
    use dbus_secret_service::{Error, SecretService};
    use std::collections::HashMap;
    // A zero prompt timeout returns Error::Prompt instead of activating a prompt.
    let service =
        SecretService::connect_with_max_prompt_timeout(encryption, 0).map_err(|_| UNAVAILABLE)?;
    let attributes = || HashMap::from([("service", "mywhisper"), ("username", "state-db")]);
    let found = service
        .search_items(attributes())
        .map_err(|_| UNAVAILABLE)?;
    if !found.locked.is_empty() {
        return Err(LOCKED);
    }
    if found.unlocked.len() > 8 {
        return Err("too many database keyring entries; repair duplicates before opening");
    }
    let mut keys = Vec::new();
    for item in found.unlocked {
        let bytes = item.get_secret().map_err(|error| match error {
            Error::Prompt | Error::Locked => LOCKED,
            _ => UNAVAILABLE,
        })?;
        let key = std::str::from_utf8(&bytes)
            .ok()
            .and_then(crate::store::hex_decode_key)
            .ok_or("keyring holds a malformed database key; original entry preserved")?;
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    if !keys.is_empty() {
        return Ok(keys);
    }
    if existing_encrypted {
        return Err(MISSING);
    }
    let collection = service
        .get_default_collection()
        .map_err(|_| "default login keyring is unavailable; configure or unlock it and retry")?;
    if collection.is_locked().map_err(|_| UNAVAILABLE)? {
        return Err(LOCKED);
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|_| "secure random source unavailable")?;
    let encoded = crate::store::hex_encode(&key);
    let item = collection
        .create_item(
            "keyring:state-db@mywhisper",
            attributes(),
            encoded.as_bytes(),
            false, // write once: interrupted requests cannot replace a key
            "text/plain; charset=utf8",
        )
        .map_err(|error| match error {
            Error::Prompt | Error::Locked => LOCKED,
            _ => UNAVAILABLE,
        })?;
    if item.get_secret().map_err(|_| UNAVAILABLE)? != encoded.as_bytes() {
        return Err("database keyring write verification failed; database preserved");
    }
    Ok(vec![key])
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "windows"))]
fn fetch(existing_encrypted: bool) -> Result<Vec<[u8; 32]>, String> {
    use std::sync::OnceLock;
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.get().is_none() {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        let store = apple_native_keyring_store::keychain::Store::new();
        #[cfg(target_os = "windows")]
        let store = windows_native_keyring_store::Store::new();
        keyring_core::set_default_store(store.map_err(|e| format!("keyring unavailable: {e}"))?);
        let _ = DONE.set(());
    }
    let entry = keyring_core::Entry::new("mywhisper", "state-db")
        .map_err(|e| format!("keyring unavailable: {e}"))?;
    match entry.get_password() {
        Ok(stored) => crate::store::hex_decode_key(&stored)
            .map(|key| vec![key])
            .ok_or_else(|| "keyring holds a malformed database key".to_string()),
        Err(keyring_core::Error::NoEntry) if existing_encrypted => Err(MISSING.to_string()),
        Err(keyring_core::Error::NoEntry) => {
            let mut raw = [0u8; 32];
            getrandom::fill(&mut raw).map_err(|e| format!("rng: {e}"))?;
            entry
                .set_password(&crate::store::hex_encode(&raw))
                .map_err(|e| format!("keyring store failed: {e}"))?;
            Ok(vec![raw])
        }
        Err(e) => Err(format!("keyring read failed: {e}")),
    }
}

#[cfg(not(any(
    all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ),
    target_os = "macos",
    target_os = "ios",
    target_os = "windows"
)))]
fn fetch(_existing_encrypted: bool) -> Result<Vec<[u8; 32]>, String> {
    Err("keyring unavailable: unsupported platform".to_string())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Instant;
    use zbus::{OwnedObjectPath, OwnedVariant, Variant};

    const CHILD_BUS: &str = "MYWHISPER_DATABASE_KEY_TEST_BUS";

    struct Service;
    #[zbus::interface(name = "org.freedesktop.Secret.Service")]
    impl Service {
        fn open_session(
            &self,
            algorithm: &str,
            _input: Variant<'_>,
        ) -> (OwnedVariant, OwnedObjectPath) {
            assert_eq!(algorithm, "plain");
            (
                Variant::from("").try_into().unwrap(),
                "/disposable_session".try_into().unwrap(),
            )
        }
        fn search_items(
            &self,
            attributes: HashMap<String, String>,
        ) -> (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) {
            assert_eq!(
                attributes.get("service").map(String::as_str),
                Some("mywhisper")
            );
            assert_eq!(
                attributes.get("username").map(String::as_str),
                Some("state-db")
            );
            (Vec::new(), Vec::new())
        }
        fn read_alias(&self, alias: &str) -> OwnedObjectPath {
            assert_eq!(alias, "default");
            "/disposable_collection".try_into().unwrap()
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, zbus::Type)]
    struct Secret {
        session: OwnedObjectPath,
        parameters: Vec<u8>,
        value: Vec<u8>,
        content_type: String,
    }

    struct Collection {
        calls: Arc<AtomicUsize>,
    }
    #[zbus::interface(name = "org.freedesktop.Secret.Collection")]
    impl Collection {
        #[zbus(property)]
        fn locked(&self) -> bool {
            false
        }
        fn create_item(
            &self,
            _properties: HashMap<String, OwnedVariant>,
            _secret: Secret,
            replace: bool,
        ) -> (OwnedObjectPath, OwnedObjectPath) {
            assert!(!replace, "provisioning must be write once");
            self.calls.fetch_add(1, Ordering::SeqCst);
            (
                "/".try_into().unwrap(),
                "/disposable_prompt".try_into().unwrap(),
            )
        }
    }

    struct Prompt {
        prompted: Arc<AtomicUsize>,
    }
    #[zbus::interface(name = "org.freedesktop.Secret.Prompt")]
    impl Prompt {
        fn prompt(&self, _window: &str) {
            self.prompted.fetch_add(1, Ordering::SeqCst);
        }
        fn dismiss(&self) {}
    }

    fn private_bus() -> (tempfile::TempDir, crate::child::OwnedChild, String) {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("session.conf");
        // TMPDIR can exceed sockaddr_un's path limit in a Nix build. This
        // Linux-only fixture uses a unique short abstract address; its owned
        // daemon and explicit client address never touch the real session bus.
        let address = format!(
            "mywhisper-key-test-{}-{}",
            std::process::id(),
            root.path().file_name().unwrap().to_string_lossy()
        );
        std::fs::write(
            &config,
            format!(
                "<busconfig><type>session</type><listen>unix:abstract={address}</listen>\
                 <auth>EXTERNAL</auth><policy context=\"default\">\
                 <allow own=\"*\"/><allow send_destination=\"*\"/>\
                 <allow receive_sender=\"*\"/></policy></busconfig>"
            ),
        )
        .unwrap();
        let mut daemon = crate::child::OwnedChild::spawn(
            Command::new("dbus-daemon")
                .arg("--config-file")
                .arg(config)
                .args(["--nofork", "--print-address=1", "--nopidfile"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit()),
        )
        .unwrap();
        let mut output = daemon.0.stdout.take().unwrap();
        crate::child::nonblocking(&output).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut address = Vec::new();
        loop {
            let mut chunk = [0; 256];
            match output.read(&mut chunk) {
                Ok(0) => panic!("private bus closed before its address"),
                Ok(count) => address.extend_from_slice(&chunk[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("private bus address failed: {error}"),
            }
            assert!(address.len() <= 4096 && Instant::now() < deadline);
            if let Some(end) = address.iter().position(|byte| *byte == b'\n') {
                return (
                    root,
                    daemon,
                    String::from_utf8(address[..end].to_vec()).unwrap(),
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn create_item_requiring_a_prompt_fails_locked_without_activating_it() {
        let (_root, _daemon, address) = private_bus();
        let calls = Arc::new(AtomicUsize::new(0));
        let prompted = Arc::new(AtomicUsize::new(0));
        let server = async_io::block_on(
            zbus::connection::Builder::address(address.as_str())
                .name("org.freedesktop.secrets")
                .serve_at("/org/freedesktop/secrets", Service)
                .serve_at(
                    "/disposable_collection",
                    Collection {
                        calls: calls.clone(),
                    },
                )
                .serve_at(
                    "/disposable_prompt",
                    Prompt {
                        prompted: prompted.clone(),
                    },
                )
                .build(),
        )
        .unwrap();
        // dbus-secret-service only connects to the session bus named by the
        // environment, so the client runs in a child scoped to the private bus.
        let output = crate::child::run(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "database_key::tests::provisioning_against_private_bus",
                    "--ignored",
                    "--test-threads=1",
                ])
                .env("DBUS_SESSION_BUS_ADDRESS", &address)
                .env(CHILD_BUS, &address),
            b"",
            Duration::from_secs(20),
            1 << 16,
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&output).contains("1 passed"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            prompted.load(Ordering::SeqCst),
            0,
            "unlock prompt activated"
        );
        drop(server);
    }

    #[test]
    #[ignore = "child of create_item_requiring_a_prompt_fails_locked_without_activating_it"]
    fn provisioning_against_private_bus() {
        let address = std::env::var(CHILD_BUS).expect("private bus address");
        assert_eq!(std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap(), address);
        let result = bounded(|| {
            fetch_with(false, dbus_secret_service::EncryptionType::Plain).map_err(str::to_string)
        });
        assert_eq!(result, Err(LOCKED.to_string()));
    }
}

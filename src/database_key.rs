//! Bounded Secret Service access without background unlock prompts.
//!
//! Provisioning is serialized by the caller's cross-process lock. New items
//! never replace an existing key: a delayed interrupted CreateItem may leave
//! duplicate candidates, but cannot overwrite the key used by a database.
use futures_util::future::Either;
use std::collections::HashMap;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(5);
const LOCKED: &str = "database keyring is locked; unlock the login keyring and retry";
const UNAVAILABLE: &str = "Secret Service unavailable; start or unlock the login keyring and retry";

pub(crate) fn candidates(existing_encrypted: bool) -> Result<Vec<[u8; 32]>, String> {
    async_io::block_on(async {
        match futures_util::future::select(
            Box::pin(async_io::Timer::after(TIMEOUT)),
            Box::pin(fetch(existing_encrypted)),
        )
        .await
        {
            Either::Left((_, operation)) => {
                drop(operation);
                Err(
                    "Secret Service deadline exceeded; unlock the login keyring and retry"
                        .to_string(),
                )
            }
            Either::Right((result, _)) => result.map_err(str::to_string),
        }
    })
}

async fn fetch(existing_encrypted: bool) -> Result<Vec<[u8; 32]>, &'static str> {
    let connection = zbus::connection::Builder::session()
        .map_err(|_| UNAVAILABLE)?
        .method_timeout(TIMEOUT)
        .max_queued(16)
        .build()
        .await
        .map_err(|_| UNAVAILABLE)?;
    let operation = async {
        let service = secret_service::SecretService::connect_with_existing(
            secret_service::EncryptionType::Dh,
            connection.clone(),
        )
        .await
        .map_err(|_| UNAVAILABLE)?;
        let attributes = || HashMap::from([("service", "mywhisper"), ("username", "state-db")]);
        let found = service
            .search_items(attributes())
            .await
            .map_err(|_| UNAVAILABLE)?;
        if !found.locked.is_empty() {
            return Err(LOCKED);
        }
        if found.unlocked.len() > 8 {
            return Err("too many database keyring entries; repair duplicates before opening");
        }
        let mut keys = Vec::new();
        for item in found.unlocked {
            let bytes = item.get_secret().await.map_err(|_| UNAVAILABLE)?;
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
            return Err(
                "encrypted database exists but its key is missing; restore the keyring secret",
            );
        }
        let collection = service.get_default_collection().await.map_err(
            |_| "default login keyring is unavailable; configure or unlock it and retry",
        )?;
        if collection.is_locked().await.map_err(|_| UNAVAILABLE)? {
            return Err(LOCKED);
        }
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|_| "secure random source unavailable")?;
        let encoded = crate::store::hex_encode(&key);
        let item = collection
            .create_item_no_prompt(
                "keyring:state-db@mywhisper",
                attributes(),
                encoded.as_bytes(),
                false, // write once: interrupted requests cannot replace a key
                "text/plain; charset=utf8",
            )
            .await
            .map_err(|error| match error {
                secret_service::Error::Prompt | secret_service::Error::Locked => LOCKED,
                _ => UNAVAILABLE,
            })?;
        if item.get_secret().await.map_err(|_| UNAVAILABLE)? != encoded.as_bytes() {
            return Err("database keyring write verification failed; database preserved");
        }
        Ok(vec![key])
    };
    operation.await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Instant;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    struct Service;
    #[zbus::interface(name = "org.freedesktop.Secret.Service")]
    impl Service {
        fn open_session(
            &self,
            algorithm: &str,
            _input: Value<'_>,
        ) -> (OwnedValue, OwnedObjectPath) {
            assert_eq!(algorithm, "plain");
            (
                zbus::zvariant::Str::from("").into(),
                "/disposable_session".try_into().unwrap(),
            )
        }
        fn read_alias(&self, alias: &str) -> OwnedObjectPath {
            assert_eq!(alias, "default");
            "/disposable_collection".try_into().unwrap()
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, zbus::zvariant::Type)]
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
        fn create_item(
            &self,
            _properties: HashMap<String, OwnedValue>,
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
        dismissed: Arc<AtomicUsize>,
    }
    #[zbus::interface(name = "org.freedesktop.Secret.Prompt")]
    impl Prompt {
        async fn prompt(
            &self,
            _window: &str,
            #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
        ) {
            self.prompted.fetch_add(1, Ordering::SeqCst);
            Self::completed(
                &emitter,
                false,
                Value::from(zbus::zvariant::ObjectPath::try_from("/created_item").unwrap()),
            )
            .await
            .unwrap();
        }
        async fn dismiss(&self) {
            // Waiting for acknowledgement must not let the operation future
            // activate a prompt while dismissal is still pending.
            async_io::Timer::after(Duration::from_millis(50)).await;
            self.dismissed.fetch_add(1, Ordering::SeqCst);
        }
        #[zbus(signal)]
        async fn completed(
            emitter: &zbus::object_server::SignalEmitter<'_>,
            dismissed: bool,
            result: Value<'_>,
        ) -> zbus::Result<()>;
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
    fn create_item_prompt_is_dismissed_before_activation_and_its_future_is_cancelled() {
        let (_root, _daemon, address) = private_bus();
        async_io::block_on(async {
            let calls = Arc::new(AtomicUsize::new(0));
            let prompted = Arc::new(AtomicUsize::new(0));
            let dismissed = Arc::new(AtomicUsize::new(0));
            let server = zbus::connection::Builder::address(address.as_str())
                .unwrap()
                .name("org.freedesktop.secrets")
                .unwrap()
                .serve_at("/org/freedesktop/secrets", Service)
                .unwrap()
                .serve_at(
                    "/disposable_collection",
                    Collection {
                        calls: calls.clone(),
                    },
                )
                .unwrap()
                .serve_at(
                    "/disposable_prompt",
                    Prompt {
                        prompted: prompted.clone(),
                        dismissed: dismissed.clone(),
                    },
                )
                .unwrap()
                .build()
                .await
                .unwrap();
            let client = zbus::connection::Builder::address(address.as_str())
                .unwrap()
                .method_timeout(Duration::from_secs(2))
                .build()
                .await
                .unwrap();
            let service = secret_service::SecretService::connect_with_existing(
                secret_service::EncryptionType::Plain,
                client,
            )
            .await
            .unwrap();
            let collection = service.get_default_collection().await.unwrap();
            let attributes = || HashMap::from([("service", "mywhisper"), ("username", "state-db")]);
            // Establish the fixture really activates a prompt without the
            // guard, then assert the corrected behavior with the same calls.
            collection
                .create_item(
                    "synthetic key",
                    attributes(),
                    b"synthetic",
                    false,
                    "text/plain",
                )
                .await
                .unwrap();
            assert_eq!(prompted.swap(0, Ordering::SeqCst), 1);
            assert_eq!(calls.swap(0, Ordering::SeqCst), 1);
            let error = match collection
                .create_item_no_prompt(
                    "synthetic key",
                    attributes(),
                    b"synthetic",
                    false,
                    "text/plain",
                )
                .await
            {
                Ok(_) => panic!("no-prompt creation succeeded unexpectedly"),
                Err(error) => error,
            };
            assert!(matches!(error, secret_service::Error::Prompt));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(dismissed.load(Ordering::SeqCst), 1);
            assert_eq!(
                prompted.load(Ordering::SeqCst),
                0,
                "operation continued while dismissal was pending"
            );
            drop(server);
        });
    }
}

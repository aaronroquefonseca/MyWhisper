//! Fresh-process acceptance probe for a disposable real Secret Service.
//! Run only through scripts/keyring-smoke.sh; never points at user data.
use mywhisper::{device::DeviceIdentity, paths::AppPaths, store::Store};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(
        std::env::var_os("MYWHISPER_AUDIT_ROOT").ok_or("disposable root required")?,
    );
    if !root.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .starts_with("mywhisper-keyring-audit.")
    }) || !root.join(".disposable").is_file()
        || root.metadata()?.permissions().mode() & 0o777 != 0o700
        || root.metadata()?.uid() != unsafe { libc::geteuid() }
    {
        return Err("refusing a non-disposable root".into());
    }
    for name in [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
    ] {
        let path = std::path::PathBuf::from(std::env::var_os(name).ok_or("isolated XDG required")?);
        if !path.starts_with(&root)
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err("refusing XDG outside the disposable root".into());
        }
    }
    if std::env::var_os("MYWHISPER_DB_KEY").is_some() {
        return Err("real keyring gate cannot use an environment key".into());
    }
    let paths = AppPaths::create()?;
    let args = std::env::args().collect::<Vec<_>>();
    match args.get(1).map(String::as_str) {
        Some("add") => {
            let index: u32 = args.get(2).ok_or("probe index required")?.parse()?;
            let identity = DeviceIdentity::load_or_create(&paths.device_key_file())?;
            let store = Store::open(&paths.db_file())?;
            store.upsert_word(&format!("synthetic-process-{index}"), &identity.node_id)?;
        }
        Some("verify") => {
            let count: usize = args.get(2).ok_or("count required")?.parse()?;
            let store = Store::open(&paths.db_file())?;
            let words = store.export_words()?;
            assert_eq!(words.len(), count);
            let identity = DeviceIdentity::load_or_create(&paths.device_key_file())?;
            assert!(words.iter().all(|word| word.updated_by == identity.node_id));
            assert_eq!(paths.device_key_file().metadata()?.len(), 64);
            assert_eq!(
                paths.device_key_file().metadata()?.permissions().mode() & 0o777,
                0o600
            );
            store.checkpoint()?;
        }
        Some("missing-key-recovery") => {
            let entry = keyring::Entry::new("mywhisper", "state-db")?;
            let secret = entry.get_password()?;
            entry.delete_credential()?;
            assert!(Store::open(&paths.db_file()).is_err());
            assert!(
                matches!(entry.get_password(), Err(keyring::Error::NoEntry)),
                "failed open must not create a replacement key"
            );
            entry.set_password(&secret)?;
            assert_eq!(Store::open(&paths.db_file())?.load_words()?.len(), 8);
        }
        Some("sync-lifecycle") => sync_lifecycle(&paths)?,
        Some("capture-provisioning") => {
            use std::time::{Duration, Instant};
            mywhisper::runtime::init()?;
            let store = Store::open(&paths.db_file())?;
            let original = store.load_settings();
            let mut settings = original.clone();
            settings.stt.entries.truncate(1);
            settings.stt.entries[0].kind = mywhisper::settings::SttEntryKind::SelfHosted;
            settings.stt.entries[0].url = "http://127.0.0.1:1".into();
            settings.stt.entries[0].streaming = false;
            store.save_settings(&settings)?;
            let input = mywhisper::transcribe::CaptureInput::new(
                Store::open(&paths.db_file())?,
                paths.data_dir.clone(),
            )?;
            std::fs::write(root.join("capture-ready"), b"ready")?;
            let deadline = Instant::now() + Duration::from_secs(10);
            while !root.join("capture-go").exists() {
                if Instant::now() >= deadline {
                    return Err("capture provisioning barrier timed out".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            // The runner has paused only its own Secret Service. Presses use
            // already-provisioned readers; fake microphone/device-failure
            // cleanup does not create discard markers or touch that daemon.
            for _ in 0..4 {
                let began = Instant::now();
                input.press()?;
                assert!(began.elapsed() < Duration::from_millis(100));
                assert!(mywhisper::transcribe::has_live());
                mywhisper::audio::tap_samples(&[71; 320], 16_000);
                mywhisper::transcribe::abort_failed_capture();
                assert!(!mywhisper::transcribe::has_live());
                mywhisper::runtime::drain(Duration::from_secs(2))?;
            }
            drop(input);
            std::fs::write(root.join("capture-done"), b"done")?;
            let deadline = Instant::now() + Duration::from_secs(10);
            while !root.join("capture-resumed").exists() {
                if Instant::now() >= deadline {
                    return Err("Secret Service resume barrier timed out".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            store.save_settings(&original)?;
            assert_eq!(Store::open(&paths.db_file())?.export_words()?.len(), 8);
            store.checkpoint()?;
            println!(
                "Real private keyring provisioned both input readers; four input/device-failure cycles completed while its daemon was paused; original settings and eight words preserved."
            );
        }
        Some("duplicate-key-recovery") => {
            use std::collections::HashMap;
            let entry = keyring::Entry::new("mywhisper", "state-db")?;
            let original = entry.get_password()?;
            let service = secret_service::blocking::SecretService::connect(
                secret_service::EncryptionType::Dh,
            )?;
            let collection = service.get_default_collection()?;
            let duplicate = collection.create_item(
                "Disposable interrupted provision",
                HashMap::from([("service", "mywhisper"), ("username", "state-db")]),
                "77".repeat(32).as_bytes(),
                false,
                "text/plain; charset=utf8",
            )?;
            assert!(
                entry.get_password().is_err(),
                "native keyring must expose the duplicate ambiguity"
            );
            assert_eq!(Store::open(&paths.db_file())?.load_words()?.len(), 8);
            assert!(
                entry.get_password().is_err(),
                "database recovery must preserve both key candidates"
            );
            duplicate.delete()?;
            assert_eq!(entry.get_password()?, original);
            println!(
                "Disposable duplicate-key recovery selected the existing database key without overwriting either secret."
            );
        }
        Some("locked-keyring") => {
            let service = secret_service::blocking::SecretService::connect(
                secret_service::EncryptionType::Dh,
            )?;
            service.get_default_collection()?.lock()?;
            let before = std::fs::read(paths.db_file())?;
            let start = std::time::Instant::now();
            let error = match Store::open(&paths.db_file()) {
                Ok(_) => panic!("locked keyring opened unexpectedly"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains("locked"),
                "locked keyring error was not actionable: {error}"
            );
            assert!(start.elapsed() < std::time::Duration::from_secs(2));
            assert_eq!(std::fs::read(paths.db_file())?, before);
            println!(
                "Disposable locked keyring refused opening promptly with the database preserved."
            );
        }
        Some("stalled-keyring") => {
            let before = std::fs::read(paths.db_file())?;
            for _ in 0..2 {
                let start = std::time::Instant::now();
                let error = match Store::open(&paths.db_file()) {
                    Ok(_) => panic!("paused Secret Service opened unexpectedly"),
                    Err(error) => error.to_string(),
                };
                assert!(
                    error.contains("deadline"),
                    "stalled keyring error was not bounded: {error}"
                );
                assert!(start.elapsed() < std::time::Duration::from_secs(6));
                assert_eq!(std::fs::read(paths.db_file())?, before);
            }
            println!(
                "Disposable stalled Secret Service reached its five-second deadline twice without altering the database."
            );
        }
        _ => return Err("expected add, verify, or missing-key-recovery".into()),
    }
    Ok(())
}

fn sync_lifecycle(paths: &AppPaths) -> Result<(), Box<dyn std::error::Error>> {
    use mywhisper::net::{NoiseStream, exchange, start_mdns};
    use mywhisper::service::{start_sync_engine, stop_sync_engine};
    use std::collections::HashMap;
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};
    let store = Store::open(&paths.db_file())?;
    let identity = DeviceIdentity::load_or_create(&paths.device_key_file())?;
    let reservation = TcpListener::bind("127.0.0.1:0")?;
    let port = reservation.local_addr()?.port();
    drop(reservation);
    let discovered = Arc::new(Mutex::new(HashMap::new()));
    let mut settings = store.load_settings();
    settings.sync.enabled = true;
    settings.sync.listen_port = port;
    settings.sync.advertise = false;
    settings.sync.device_name = "Disposable sync owner".into();
    store.save_settings(&settings)?;
    assert_eq!(
        start_sync_engine(paths, &store, &identity, discovered.clone()),
        port
    );
    for _ in 0..3 {
        let mut stalled = TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse()?,
            Duration::from_secs(1),
        )?;
        stalled.set_read_timeout(Some(Duration::from_secs(2)))?;
        let replacement = Instant::now();
        assert_eq!(
            start_sync_engine(paths, &store, &identity, discovered.clone()),
            port
        );
        assert!(replacement.elapsed() < Duration::from_secs(4));
        let mut byte = [0];
        match stalled.read(&mut byte) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) => {}
            result => panic!("old engine retained an incoming socket: {result:?}"),
        }
    }
    stop_sync_engine();
    drop(TcpListener::bind(("0.0.0.0", port))?);
    for task in std::fs::read_dir("/proc/self/task")? {
        let name = std::fs::read_to_string(task?.path().join("comm"))?;
        assert!(
            !["sync-listener", "sync-periodic", "sync-discovery-"].contains(&name.trim()),
            "stopped engine retained worker {name}"
        );
    }

    // Listening disabled still permits authenticated outbound catch-up.
    let peer_root = tempfile::tempdir_in(&paths.data_dir)?;
    let peer_db = peer_root.path().join("state.db");
    let peer_store = Store::open(&peer_db)?;
    let peer = DeviceIdentity::ephemeral();
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?.to_string();
    store.upsert_peer(&peer.node_id, "Disposable peer", &address, true)?;
    peer_store.upsert_peer(&identity.node_id, "Disposable local", "", true)?;
    listener.set_nonblocking(true)?;
    let peer_creds = peer.creds();
    let (complete, events) = mpsc::channel();
    let receiver = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                result => panic!("outbound-only engine failed to dial: {result:?}"),
            }
        };
        let (mut session, hash, _) =
            NoiseStream::handshake(socket, &peer_creds.noise_static, false).unwrap();
        let report = exchange(
            &mut session,
            &hash,
            &peer_store,
            &peer_creds,
            "Disposable peer",
            &[],
            false,
            None,
            None,
        )
        .unwrap();
        assert_eq!(report.words_in, 8);
        assert_eq!(peer_store.load_words().unwrap().len(), 8);
        assert!(
            peer_store
                .get_peer(&report.peer_node)
                .unwrap()
                .unwrap()
                .addresses
                .is_empty(),
            "outbound-only node advertised a listener"
        );
        complete.send(()).unwrap();
    });
    settings.sync.listen_port = 0;
    store.save_settings(&settings)?;
    assert_eq!(
        start_sync_engine(paths, &store, &identity, discovered.clone()),
        0
    );
    events.recv_timeout(Duration::from_secs(12))?;
    receiver.join().map_err(|_| "outbound receiver panicked")?;
    stop_sync_engine();

    // Actual multicast advertisement/browsing and goodbye disappearance,
    // each backed by a live private listener and an ephemeral identity.
    let a = TcpListener::bind("0.0.0.0:0")?;
    let b = TcpListener::bind("0.0.0.0:0")?;
    let found_a = Arc::new(Mutex::new(HashMap::new()));
    let found_b = Arc::new(Mutex::new(HashMap::new()));
    let browse_a = start_mdns(
        &identity.node_id,
        "Disposable A",
        a.local_addr()?.port(),
        found_a.clone(),
        true,
    )?;
    let browse_b = start_mdns(
        &peer.node_id,
        "Disposable B",
        b.local_addr()?.port(),
        found_b.clone(),
        true,
    )?;
    let discovery_deadline = Instant::now() + Duration::from_secs(8);
    while !(found_a.lock().unwrap().contains_key(&peer.node_id)
        && found_b.lock().unwrap().contains_key(&identity.node_id))
    {
        if Instant::now() >= discovery_deadline {
            return Err("native mDNS mutual discovery deadline exceeded".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    browse_b.shutdown();
    let removal_deadline = Instant::now() + Duration::from_secs(8);
    while found_a.lock().unwrap().contains_key(&peer.node_id) {
        if Instant::now() >= removal_deadline {
            return Err("native mDNS goodbye disappearance deadline exceeded".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    browse_a.shutdown();
    println!(
        "Native sync: fixed-port replacement, stalled-socket cleanup, watcher joins, outbound-only encrypted catch-up, mutual mDNS discovery and goodbye removal passed."
    );
    Ok(())
}

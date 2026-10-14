use mywhisper::{
    device::DeviceIdentity,
    net, runtime,
    settings::{AppSettings, HotkeySettings, SttChainEntry, SttEntryKind, SttSettings},
    store::{Store, WordRecord, now_ms},
    transcribe,
};
use std::{
    net::TcpListener,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn main() {
    let root = std::path::PathBuf::from(std::env::var("AUDIT_DIR").expect("AUDIT_DIR"));
    std::fs::create_dir_all(&root).unwrap();
    let s = Store::open_memory().unwrap();
    let id = s
        .add_history("raw secret", "private transcript", 2, 3, "A")
        .unwrap();
    s.delete_history(&id).unwrap();
    let r = s.export_history().unwrap().remove(0);
    assert!(r.deleted && r.text == "private transcript" && r.raw_text == "raw secret");
    println!("REPRODUCED deletion exports original text and raw text");
    let id = s.add_discarded(2, "A").unwrap();
    let stale = s
        .export_history()
        .unwrap()
        .into_iter()
        .find(|r| r.uuid == id)
        .unwrap();
    s.delete_history(&id).unwrap();
    s.apply_history(&[stale]).unwrap();
    assert!(
        s.load_history(100, true)
            .unwrap()
            .iter()
            .any(|r| r.uuid == id)
    );
    println!("REPRODUCED stale discarded history resurrects deleted row");
    s.apply_words(&[WordRecord {
        word: "future".into(),
        updated_at: now_ms() + 86_400_000,
        updated_by: "B".into(),
        deleted: false,
    }])
    .unwrap();
    s.delete_word("future", "A").unwrap();
    assert!(s.load_words().unwrap().iter().any(|w| w == "future"));
    println!("REPRODUCED future-dated remote word cannot be deleted locally");
    let t = now_ms();
    s.apply_words(&[WordRecord {
        word: "equal-time".into(),
        updated_at: t,
        updated_by: "A".into(),
        deleted: false,
    }])
    .unwrap();
    s.apply_words(&[WordRecord {
        word: "equal-time".into(),
        updated_at: t,
        updated_by: "A".into(),
        deleted: true,
    }])
    .unwrap();
    assert!(s.load_words().unwrap().iter().any(|w| w == "equal-time"));
    println!("REPRODUCED same-millisecond same-node delete silently ignored");
    let old = s.export_history().unwrap();
    s.upsert_peer("peer", "peer", "127.0.0.1:1", true).unwrap();
    let mut settings = AppSettings::default();
    settings.sync.enabled = false;
    s.save_settings(&settings).unwrap();
    s.reset_all_data().unwrap();
    assert!(s.get_peer("peer").unwrap().unwrap().trusted && s.load_settings().sync.enabled);
    s.apply_history(&old).unwrap();
    assert!(!s.load_history(100, true).unwrap().is_empty());
    println!("REPRODUCED reset preserves trust, re-enables sync, and permits data resurrection");
    let mut hk: HotkeySettings = serde_json::from_str(r#"{"dictation_hotkey":"Ctrl+D"}"#).unwrap();
    hk.migrate_legacy();
    assert_eq!(hk.effective_hotkeys()[0].combo, "F13");
    println!("REPRODUCED legacy Ctrl+D migrates to F13");
    let panic = std::panic::catch_unwind(|| {
        mywhisper::textnorm::dictionary_prompt(&["界".repeat(400)], 890)
    });
    assert!(panic.is_err());
    println!("REPRODUCED Unicode dictionary truncation panics");

    let a = DeviceIdentity::ephemeral();
    let b = DeviceIdentity::ephemeral();
    let ap = root.join("a.db");
    let bp = root.join("b.db");
    let sa = Store::open(&ap).unwrap();
    let sb = Store::open(&bp).unwrap();
    sa.save_settings(&settings).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    sa.upsert_peer(&b.node_id, "B", &addr, true).unwrap();
    sb.upsert_peer(&a.node_id, "A", "", true).unwrap();
    let secret = sa
        .add_history("secret", "sent with sync off", 4, 3, &a.node_id)
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let st = stop.clone();
    let bc = b.creds();
    let bp2 = bp.clone();
    let server = std::thread::spawn(move || net::serve(listener, bp2, bc, "B".into(), vec![], &st));
    let lines = net::sync_all_trusted(&ap, &a.creds());
    assert!(
        sb.load_history(100, true)
            .unwrap()
            .iter()
            .any(|r| r.uuid == secret),
        "{lines:?}"
    );
    println!("REPRODUCED real encrypted loopback sync transfers history while sync.enabled=false");
    sa.upsert_peer(&b.node_id, "B", &addr, false).unwrap();
    let secret = sa
        .add_history("secret", "sent to untrusted peer", 4, 3, &a.node_id)
        .unwrap();
    net::sync_now(&[addr], &ap, &a.creds(), "A", &b.node_id).unwrap();
    assert!(
        sb.load_history(100, true)
            .unwrap()
            .iter()
            .any(|r| r.uuid == secret)
    );
    assert!(!sa.get_peer(&b.node_id).unwrap().unwrap().trusted);
    println!("REPRODUCED outbound sync sends history to locally untrusted authenticated peer");
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();

    let mut race = false;
    for trial in 0..100 {
        let path = root.join(format!("identity-{trial}"));
        let barrier = Arc::new(Barrier::new(12));
        let mut handles = vec![];
        for _ in 0..12 {
            let p = path.clone();
            let gate = barrier.clone();
            handles.push(std::thread::spawn(move || {
                gate.wait();
                DeviceIdentity::load_or_create(&p).map(|i| i.node_id)
            }));
        }
        let mut ids = handles
            .into_iter()
            .filter_map(|h| h.join().unwrap().ok())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        if ids.len() > 1 {
            race = true;
            println!(
                "REPRODUCED concurrent identity initialization returned {} different identities",
                ids.len()
            );
            break;
        }
    }
    assert!(race, "identity race not observed in this run");

    runtime::init().unwrap();
    let barrier = Arc::new(Barrier::new(5));
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    for _ in 0..4 {
        let b = barrier.clone();
        let r = release.clone();
        runtime::submit("audit-block", move || {
            b.wait();
            let (lock, cv) = &*r;
            let mut done = lock.lock().unwrap();
            while !*done {
                done = cv.wait(done).unwrap();
            }
        })
        .unwrap();
    }
    barrier.wait();
    let mut stt = SttSettings::default();
    stt.entries = vec![SttChainEntry {
        id: "audit".into(),
        name: "audit".into(),
        kind: SttEntryKind::SelfHosted,
        enabled: true,
        provider: "".into(),
        url: "http://127.0.0.1:1".into(),
        model: "test".into(),
        api_key: "".into(),
        streaming: false,
    }];
    let data = root.clone();
    runtime::submit("audit-press", move || {
        transcribe::begin_utterance(stt, false, &data, 0)
    })
    .unwrap();
    transcribe::end_utterance(|| {});
    {
        let (lock, cv) = &*release;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    for _ in 0..100 {
        if transcribe::has_live() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(transcribe::has_live());
    println!("REPRODUCED queued press after release leaves an orphan live recording");
    let (tx, rx) = std::sync::mpsc::channel();
    transcribe::cancel_utterance(move || {
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut stt = SttSettings::default();
    stt.entries = vec![SttChainEntry {
        id: "audit".into(),
        name: "audit".into(),
        kind: SttEntryKind::SelfHosted,
        enabled: true,
        provider: "".into(),
        url: "http://127.0.0.1:1".into(),
        model: "test".into(),
        api_key: "".into(),
        streaming: false,
    }];
    transcribe::begin_utterance(stt, false, &root, 300);
    let (tx, rx) = std::sync::mpsc::channel();
    transcribe::end_utterance(move || {
        let _ = tx.send(());
    });
    let next = Arc::new(std::sync::Mutex::new(mywhisper::audio::Recorder::default()));
    next.lock().unwrap().start(16_000);
    mywhisper::audio::set_recorder(Some(next.clone()));
    mywhisper::audio::tap_samples(&[1], 16_000);
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    mywhisper::audio::tap_samples(&[2], 16_000);
    assert_eq!(next.lock().unwrap().snapshot(), vec![1]);
    println!("REPRODUCED release of old session disarms a replacement recorder");

    let entry = mywhisper::stt::find_model("tiny").unwrap();
    let dir = mywhisper::stt::model_dir(&root, entry);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ggml-tiny.bin"),
        b"audit placeholder - never decoded",
    )
    .unwrap();
    let mut stt = SttSettings::default();
    stt.whisper_model = "tiny".into();
    stt.spec_enabled = true;
    stt.spec_update_secs = 0.5;
    for n in 0..4 {
        transcribe::begin_utterance(stt.clone(), false, &root, 0);
        std::thread::sleep(Duration::from_millis(100));
        let (tx, rx) = std::sync::mpsc::channel();
        transcribe::cancel_utterance(move || {
            let _ = tx.send(());
        });
        if n < 3 {
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    runtime::submit("audit-check-workers", move || {
        let _ = tx.send(());
    })
    .unwrap();
    assert!(rx.recv_timeout(Duration::from_secs(2)).is_err());
    println!("REPRODUCED four cancelled speculative utterances exhaust all four runtime workers");
    println!("All focused reproductions completed using disposable state.");
}

use mywhisper::{audio, runtime, settings::{SttChainEntry, SttEntryKind, SttSettings}, store::{Store, HistoryRecord}, transcribe};
use std::{io::{Read, Write}, net::TcpListener, sync::mpsc, time::Duration};

fn endpoint(name: &'static str, arrived: mpsc::Sender<&'static str>, gate: Option<mpsc::Receiver<()>>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                _ => return,
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut body = Vec::new();
        let end;
        loop {
            let mut b = [0; 1]; stream.read_exact(&mut b).unwrap(); body.push(b[0]);
            if body.ends_with(b"\r\n\r\n") {end = body.len(); break;}
        }
        let length = String::from_utf8_lossy(&body).lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap();
        body.resize(end + length, 0); stream.read_exact(&mut body[end..]).unwrap();
        println!("Mock {name} received {length} body bytes");
        arrived.send(name).unwrap();
        if let Some(gate) = gate {gate.recv_timeout(Duration::from_secs(4)).unwrap();}
        stream.write_all(b"HTTP/1.1 500 Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    (url, task)
}

fn main() {
    runtime::init().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mode = std::env::args().nth(1).unwrap();
    if mode == "thinking" {
        let store=Store::open_memory().unwrap(); let mut settings=store.load_settings();
        settings.language_models.entries[0].thinking=None;
        store.save_settings(&settings).unwrap();
        println!("Explicit Ollama model-default thinking after reload: {:?}",store.load_settings().language_models.entries[0].thinking);
        return;
    }
    if mode == "dns" {
        let start=std::time::Instant::now();
        let result=transcribe::OnlineSession::open_with_url("ws://mywhisper-review.invalid:9",Duration::from_millis(50));
        println!("WebSocket advertised 10-second startup deadline: elapsed {:?}, error {:?}",start.elapsed(),result.err());
        return;
    }
    if mode == "installed" {
        use mywhisper::{stt,settings::LocalSttProvider};
        let entry = stt::find_model("parakeet-tdt-0.6b-v3").unwrap();
        let dir = stt::model_dir(root.path(),entry); std::fs::create_dir_all(&dir).unwrap();
        for file in entry.required_files {std::fs::write(dir.join(file),[]).unwrap();}
        let settings = SttSettings{local_provider:LocalSttProvider::Nvidia,..SttSettings::default()};
        println!("Zero-byte Parakeet model: installed={}, plan={:?}",stt::is_installed(root.path(),entry),transcribe::plan_for(&settings,root.path()));
        let whisper = stt::find_model("tiny").unwrap(); let directory = stt::model_dir(root.path(),whisper); std::fs::create_dir_all(&directory).unwrap();
        let file = std::fs::File::create(directory.join("ggml-tiny.bin")).unwrap();
        file.set_len(whisper.expected_bytes.unwrap()*96/100).unwrap();
        println!("Whisper truncated to 96% of pinned length: installed={}",stt::is_installed(root.path(),whisper));
        return;
    }
    if mode == "server-replacement" {
        use std::os::unix::fs::PermissionsExt;
        use mywhisper::{stt,settings::{GpuBackendSetting,SherpaBackendSetting}};
        Store::open(&root.path().join("state.db")).unwrap();
        let pack = stt::GPU_PACKS.iter().find(|p|p.id=="whisper-cpu").unwrap();
        let bin = stt::gpu_bin_dir(root.path(),pack).join("whisper-server");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin,include_str!("/tmp/mywhisper-review-results/fake_whisper.py")).unwrap();
        std::fs::set_permissions(&bin,std::fs::Permissions::from_mode(0o700)).unwrap();
        let model = root.path().join("models/whisper/tiny/ggml-tiny.bin");
        std::fs::create_dir_all(model.parent().unwrap()).unwrap(); std::fs::write(&model,b"synthetic").unwrap();
        let reserve = TcpListener::bind("127.0.0.1:0").unwrap(); let port = reserve.local_addr().unwrap().port(); drop(reserve);
        let s = SttSettings{whisper_model:"tiny".into(),language:"en".into(),server_port:port.to_string(),..SttSettings::default()};
        transcribe::begin_utterance(s.clone(),false,root.path(),0);
        audio::tap_samples(&vec![1000;16000],16000);
        let (tx,done)=mpsc::channel(); transcribe::end_utterance_with_outcome(move |outcome|tx.send(outcome).unwrap());
        let marker = model.with_file_name("ggml-tiny.bin.request_started");
        let deadline = std::time::Instant::now()+Duration::from_secs(5);
        while !marker.exists() {assert!(std::time::Instant::now()<deadline,"inference never reached mock"); std::thread::sleep(Duration::from_millis(10));}
        println!("Original inference is active; warming changed language now");
        let updated = SttSettings{language:"es".into(),..s};
        let plan = transcribe::plan_for(&updated,root.path()).unwrap();
        let warm = transcribe::ensure_for_plan(&plan,root.path(),GpuBackendSetting::Cpu,SherpaBackendSetting::Cpu,&transcribe::server_net(&updated));
        println!("Changed-language warmup: {warm:?}; original finalization: {:?}",done.recv_timeout(Duration::from_secs(5)));
        transcribe::drop_servers(root.path());
        return;
    }
    if mode == "oversized" {
        use mywhisper::{device::DeviceIdentity,net::{NoiseStream,exchange,sync_now}};
        let a = root.path().join("a"); let b = root.path().join("b");
        std::fs::create_dir(&a).unwrap(); std::fs::create_dir(&b).unwrap();
        let ca = DeviceIdentity::load_or_create(&a.join("device.key")).unwrap().creds();
        let cb = DeviceIdentity::load_or_create(&b.join("device.key")).unwrap().creds();
        let store = Store::open(&a.join("state.db")).unwrap();
        store.upsert_peer(&cb.node_id,"b","",true).unwrap();
        store.add_history(&"synthetic raw ".repeat(1400000),"synthetic final",2,1,&ca.node_id).unwrap();
        Store::open(&b.join("state.db")).unwrap().upsert_peer(&ca.node_id,"a","",true).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap(); let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream,_) = listener.accept().unwrap();
            let (mut session, hash,_) = NoiseStream::handshake(stream,&cb.noise_static,false).unwrap();
            let store = Store::open(&b.join("state.db")).unwrap();
            println!("Large-history responder: {:?}",exchange(&mut session,&hash,&store,&cb,"b",&[],false,None,None));
        });
        println!("Large-history initiator: {:?}",sync_now(&[address.to_string()],&a.join("state.db"),&ca,"a",&store.list_peers().unwrap()[0].node_id));
        server.join().unwrap();
        return;
    }
    if mode == "queued" {
        let (occupied, ready) = mpsc::channel();
        let mut releases = Vec::new();
        for _ in 0..runtime::POOL_WORKERS {
            let (tx, rx) = mpsc::channel(); releases.push(tx);
            let occupied = occupied.clone();
            runtime::submit("review-occupy", move || { occupied.send(()).unwrap(); rx.recv().unwrap(); }).unwrap();
        }
        for _ in 0..runtime::POOL_WORKERS { ready.recv_timeout(Duration::from_secs(2)).unwrap(); }
        Store::open(&root.path().join("state.db")).unwrap();
        let epoch = transcribe::reserve_utterance();
        let data = root.path().to_path_buf();
        let mut entry = SttSettings::default().entries.remove(0);
        entry.kind = SttEntryKind::SelfHosted; entry.url = "http://127.0.0.1:1".into(); entry.streaming = false;
        let s = SttSettings { entries: vec![entry], ..SttSettings::default() };
        runtime::submit("press-arm", move || transcribe::begin_reserved_utterance(epoch,s,false,&data,0)).unwrap();
        audio::tap_samples(&vec![1000;16000],16000);
        std::thread::sleep(Duration::from_millis(320));
        let (tx, rx) = mpsc::channel();
        transcribe::end_utterance_with_outcome(move |outcome| tx.send(outcome).unwrap());
        println!("Entire uncancelled hold while press-arm is queued: {:?}",rx.recv_timeout(Duration::from_secs(2)).unwrap());
        for release in releases {release.send(()).unwrap();}
        std::thread::sleep(Duration::from_millis(100));
        println!("Live after queued arm resumes: {}",transcribe::has_live());
        return;
    }
    if mode == "armlock" {
        use fs2::FileExt;
        Store::open(&root.path().join("state.db")).unwrap();
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(root.path().join("state.db.init.lock")).unwrap();
        lock.lock_exclusive().unwrap();
        let epoch = transcribe::reserve_utterance(); let data = root.path().to_path_buf();
        let worker = std::thread::spawn(move || transcribe::begin_reserved_utterance(epoch,SttSettings::default(),false,&data,0));
        std::thread::sleep(Duration::from_millis(150));
        let unlock = std::thread::spawn(move || {std::thread::sleep(Duration::from_secs(1)); FileExt::unlock(&lock).unwrap();});
        let start = std::time::Instant::now();
        transcribe::end_utterance(||{});
        println!("Release caller blocked while background arming waits for database: {:?}",start.elapsed());
        worker.join().unwrap(); unlock.join().unwrap();
        return;
    }
    if mode == "discard" {
        let store = Store::open_memory().unwrap();
        let row = HistoryRecord {uuid:"same".into(),text:"synthetic private text".into(),raw_text:"synthetic private raw".into(),words:3,duration_secs:1,recorded_at:mywhisper::store::now_ms()/1000,deleted:false,discarded:false};
        store.apply_history(&[row.clone()]).unwrap();
        let discard = HistoryRecord{discarded:true,text:String::new(),raw_text:String::new(),words:0,..row.clone()};
        store.apply_history(&[discard.clone()]).unwrap();
        println!("Discarded row after merge: {:?}",store.export_history().unwrap());
        let reversed = Store::open_memory().unwrap(); reversed.apply_history(&[discard,row]).unwrap();
        println!("Reversed merge order: {:?}",reversed.export_history().unwrap());
        println!("Converged: {}",store.export_history().unwrap()==reversed.export_history().unwrap());
        return;
    }
    if mode == "settings" {
        let path = root.path().join("state.db");
        let store = Store::open(&path).unwrap();
        let mut s = store.load_settings(); s.privacy.history_enabled = false; s.sync.enabled = false;
        store.save_settings(&s).unwrap(); drop(store);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(&format!("PRAGMA key=\"x'{}'\"",std::env::var("MYWHISPER_DB_KEY").unwrap())).unwrap();
        let raw: String = conn.query_row("SELECT value FROM kv WHERE key='settings'",[],|r|r.get(0)).unwrap();
        let mut json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        json.as_object_mut().unwrap().remove("privacy"); json.as_object_mut().unwrap().remove("sync");
        conn.execute("UPDATE kv SET value=?1 WHERE key='settings'",[json.to_string()]).unwrap(); drop(conn);
        let s = Store::open(&path).unwrap().load_settings();
        println!("Missing privacy/sync sections: history={}, sync={}",s.privacy.history_enabled,s.sync.enabled);
        return;
    }
    Store::open(&root.path().join("state.db")).unwrap();
    let (arrived, events) = mpsc::channel(); let (release, gate) = mpsc::channel();
    let (one, task1) = endpoint("primary", arrived.clone(), Some(gate));
    let (two, task2) = endpoint("fallback", arrived, None);
    let entry = |url: String| SttChainEntry{id:url.clone(),name:url.clone(),kind:SttEntryKind::SelfHosted,enabled:true,provider:String::new(),url,model:"synthetic".into(),api_key:String::new(),streaming:false};
    let s = SttSettings{entries:vec![entry(one),entry(two)],..SttSettings::default()};
    transcribe::begin_utterance(s, false, root.path(), 0);
    audio::tap_samples(&vec![1000; 16000],16000);
    let (completed, done) = mpsc::channel();
    transcribe::end_utterance_with_outcome(move |outcome| {completed.send(outcome).unwrap();});
    assert_eq!(events.recv_timeout(Duration::from_secs(4)).unwrap(),"primary");
    if mode == "reset" { Store::open(&root.path().join("state.db")).unwrap().reset_all_data().unwrap(); println!("Committed database reset while primary request is blocked"); }
    else {transcribe::cancel_utterance(|| {}); println!("Cancelled while primary request is blocked");}
    release.send(()).unwrap();
    let second = events.recv_timeout(Duration::from_secs(3));
    println!("Fallback event AFTER {mode}: {second:?}");
    println!("Completion: {:?}",done.recv_timeout(Duration::from_secs(4)));
    task1.join().unwrap(); task2.join().unwrap();
}

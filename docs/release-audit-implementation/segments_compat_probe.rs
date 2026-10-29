//! Disposable real encrypted legacy/current peer and migration acceptance.
use mywhisper::{device::DeviceIdentity, store::Store};
use std::{path::PathBuf, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root=PathBuf::from(std::env::var_os("MYWHISPER_SEGMENTS_COMPAT_ROOT").ok_or("disposable root required")?);
    assert!(root.file_name().unwrap().to_string_lossy().starts_with("mywhisper-segments-compat-"));
    assert!(root.join(".disposable").is_file());
    let args=std::env::args().collect::<Vec<_>>();
    let mode=args.get(1).ok_or("mode required")?;
    let node=args.get(2).ok_or("node required")?;
    assert!(matches!(node.as_str(),"old"|"new"));
    let db=root.join(format!("{node}.db"));
    if mode=="refuse" {
        use sha2::Digest;
        let before=sha2::Sha256::digest(std::fs::read(&db)?);
        let error=Store::open_with_key(&db,&[42;32]).err().expect("old schema must refuse 8");
        assert!(error.to_string().contains("newer than"));
        assert_eq!(before,sha2::Sha256::digest(std::fs::read(&db)?));
        println!("Actual schema-7 build refused schema 8 without changing encrypted data");
        return Ok(());
    }
    let id=DeviceIdentity::load_or_create(&root.join(format!("{node}.key")))?;
    let store=Store::open_with_key(&db,&[42;32])?;
    match mode.as_str() {
        "create" => {
            let mut settings=store.load_settings();settings.sync.enabled=true;store.save_settings(&settings)?;
            store.upsert_word(&format!("synthetic {node} word"),&id.node_id)?;
            store.add_history("synthetic raw","synthetic final",2,1,&id.node_id)?;
            std::fs::write(root.join(format!("{node}.node")),&id.node_id)?;
        },
        "trust" => {
            let other=if node=="old" {"new"} else {"old"};
            store.upsert_peer(&std::fs::read_to_string(root.join(format!("{other}.node")))?,other,"",true)?;
        },
        "serve" => {
            let listener=std::net::TcpListener::bind("127.0.0.1:0")?;
            let address=listener.local_addr()?.to_string();
            let stop=Arc::new(AtomicBool::new(false));let owner=stop.clone();
            let thread=std::thread::spawn(move || mywhisper::net::serve(listener,db,id.creds(),"synthetic peer".into(),vec![],&owner));
            std::fs::write(root.join(format!("{node}.address")),address)?;
            let deadline=Instant::now()+Duration::from_secs(30);
            while !root.join(format!("{node}.stop")).exists() {
                assert!(Instant::now()<deadline,"owned server deadline");
                std::thread::sleep(Duration::from_millis(10));
            }
            stop.store(true,Ordering::Release);thread.join().unwrap();
        },
        "dial" => {
            let other=if node=="old" {"new"} else {"old"};
            let peer=std::fs::read_to_string(root.join(format!("{other}.node")))?;
            let address=std::fs::read_to_string(root.join(format!("{other}.address")))?;
            let result=mywhisper::net::sync_now(&[address],&db,&id.creds(),"synthetic client",&peer)?;
            assert_eq!(result.peer_node,peer);
            assert_eq!(store.load_words()?,["synthetic new word","synthetic old word"]);
            assert_eq!(store.export_history()?.len(),2);
            println!("Actual legacy/current signed Noise loopback converged: {node} initiator");
        },
        "verify"|"upgrade" => {
            assert_eq!(store.load_words()?,["synthetic new word","synthetic old word"]);
            assert_eq!(store.export_history()?.len(),2);
            let conn=rusqlite::Connection::open(&db)?;
            conn.execute_batch(&format!("PRAGMA key=\"x'{}'\"","2a".repeat(32)))?;
            assert_eq!(conn.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0))?,8);
            let count:i64=conn.query_row("SELECT count(*) FROM sync_changes",[],|row|row.get(0))?;
            assert!(count>=4);
            println!("Schema 8 upgrade/recovery preserved settings, words, history and durable ledger");
        },
        _=>return Err("unknown probe mode".into()),
    }
    store.checkpoint()?;
    Ok(())
}

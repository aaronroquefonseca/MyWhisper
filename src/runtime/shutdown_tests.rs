//! Closing is irreversible; each regression executes in a fresh owned process.
use super::*;
use std::time::Duration;

fn isolated(name: &str) -> bool {
    if std::env::var("MYWHISPER_SHUTDOWN_PROBE").as_deref() == Ok(name) {
        return false;
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            &format!("runtime::shutdown_tests::{name}"),
            "--test-threads=1",
            "--nocapture",
        ])
        .env("MYWHISPER_SHUTDOWN_PROBE", name)
        .env("MYWHISPER_DB_KEY", "00".repeat(32));
    crate::child::run(&mut command, &[], Duration::from_secs(10), 128 * 1024).unwrap();
    true
}

struct TlsStore {
    store: Option<crate::store::Store>,
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}
impl Drop for TlsStore {
    fn drop(&mut self) {
        self.entered.send(()).unwrap();
        self.release.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(self.store.take());
    }
}
thread_local! {
    static RESOURCE: std::cell::RefCell<Option<TlsStore>> = const { std::cell::RefCell::new(None) };
}

#[test]
fn shutdown_joins_retained_owners_and_tls_before_database_finalization() {
    const NAME: &str = "shutdown_joins_retained_owners_and_tls_before_database_finalization";
    if isolated(NAME) {
        return;
    }
    init().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = crate::store::Store::open_with_key(&path, &[0; 32]).unwrap();
    store
        .upsert_word("Synthetic preserved word", "synthetic")
        .unwrap();
    let held = crate::store::Store::open_with_key(&path, &[0; 32]).unwrap();
    let (entered, tls_entered) = mpsc::channel();
    let (release, tls_release) = mpsc::channel();
    let retained = spawn_session("synthetic-tls-store", move || {
        RESOURCE.with(|resource| {
            *resource.borrow_mut() = Some(TlsStore {
                store: Some(held),
                entered,
                release: tls_release,
            })
        });
    })
    .unwrap();
    tls_entered.recv_timeout(Duration::from_secs(2)).unwrap();
    // The job body and its captured permit have finished. TLS still owns a
    // live encrypted connection, so a work counter alone is insufficient.
    drain(Duration::ZERO).unwrap();
    let (ready, started) = mpsc::channel();
    let (closed, idle_closed) = mpsc::channel();
    let idle_store = crate::store::Store::open_with_key(&path, &[0; 32]).unwrap();
    let admitted_at = std::time::Instant::now();
    let idle_owner = BackgroundTask::spawn("synthetic-retained-idle", move |stop| {
        ready.send(()).unwrap();
        assert!(stop.wait(Duration::from_secs(60)));
        drop(idle_store);
        closed.send(()).unwrap();
    })
    .unwrap();
    assert!(
        admitted_at.elapsed() < Duration::from_millis(100),
        "thread admission waited for a blocked TLS destructor"
    );
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let began = std::time::Instant::now();
    begin_shutdown();
    assert!(began.elapsed() < Duration::from_millis(100));
    assert!(submit("late", || panic!("late job ran")).is_err());
    assert!(submit_finalization(|| panic!("late finalization ran")).is_err());
    assert!(spawn_session("late", || panic!("late session ran")).is_err());
    assert!(spawn_dedicated("late", || panic!("late thread ran")).is_err());
    assert!(submit_process_launch(|| panic!("late process launch ran")).is_err());
    assert!(init().is_err());
    let (done, joined) = mpsc::channel();
    let finisher = std::thread::spawn(move || done.send(finish_shutdown()).unwrap());
    idle_closed.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        joined.recv_timeout(Duration::from_millis(50)).is_err(),
        "shutdown returned before the encrypted TLS destructor completed"
    );
    release.send(()).unwrap();
    joined
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    finisher.join().unwrap();
    retained.join(); // Another owner observes the same completed actual join.
    drop(idle_owner);
    assert_eq!(SESSION_TASKS.load(Ordering::Acquire), 0);
    assert!(THREADS.lock().unwrap().is_empty());
    finish_shutdown().unwrap(); // Repeated cancellation/cleanup is safe.
    store.checkpoint().unwrap();
    drop(store);
    let recovered = crate::store::Store::open_with_key(&path, &[0; 32]).unwrap();
    assert_eq!(
        recovered.load_words().unwrap(),
        vec!["Synthetic preserved word"]
    );
}

#[test]
fn shutdown_cancels_stalled_http_children_and_asset_authority() {
    const NAME: &str = "shutdown_cancels_stalled_http_children_and_asset_authority";
    if isolated(NAME) {
        return;
    }
    init().unwrap();
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/synthetic", listener.local_addr().unwrap());
    let (header, received) = mpsc::channel();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            assert_eq!(socket.read(&mut byte).unwrap(), 1);
            request.push(byte[0]);
            assert!(request.len() < 8192);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\n{")
            .unwrap();
        header.send(()).unwrap();
        assert_eq!(
            socket.read(&mut byte).unwrap(),
            0,
            "shutdown did not close the stalled provider socket"
        );
    });
    let (response, result) = mpsc::channel();
    let operation = crate::operation::Operation::unrestricted();
    let request = operation.clone();
    let http = spawn_session("synthetic-shutdown-http", move || {
        response
            .send(crate::provider_io::post(
                &url,
                &[],
                Vec::new(),
                &request,
                "synthetic provider",
            ))
            .unwrap();
    })
    .unwrap();
    received.recv_timeout(Duration::from_secs(2)).unwrap();
    let root = tempfile::tempdir().unwrap();
    let authority = crate::artifact::observe_authority(root.path(), "synthetic-shutdown").unwrap();
    let child_marker = root.path().join("child.pid");
    let (finished, child_result) = mpsc::channel();
    let marker = child_marker.clone();
    let cli = spawn_session("synthetic-shutdown-child", move || {
        let mut command = std::process::Command::new("sh");
        command.args([
            "-c",
            "printf '%s' \"$$\" > \"$1\"; exec sleep 30",
            "synthetic",
            marker.to_str().unwrap(),
        ]);
        finished
            .send(crate::child::run(
                &mut command,
                &[],
                Duration::from_secs(40),
                1024,
            ))
            .unwrap();
    })
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !child_marker.exists()
        || std::fs::read_to_string(&child_marker)
            .unwrap_or_default()
            .is_empty()
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    let child_pid: u32 = std::fs::read_to_string(child_marker)
        .unwrap()
        .parse()
        .unwrap();
    // Start the real selection worker with a denied guard. No connection to
    // the user's clipboard is permitted, even if Wayland is available.
    let prefs = crate::settings::Preferences {
        keep_transcription_in_clipboard: true,
        ..Default::default()
    };
    assert!(crate::clipboard::run_pipeline("synthetic", &prefs, Arc::new(|| false)).is_err());
    let began = std::time::Instant::now();
    begin_shutdown();
    // Async-copy denial under the rewired keep-mode leg: admission is
    // refused once closing (worker() errors); pre-shutdown admission
    // succeeds by design, so the denial assert lives here, not above.
    assert!(crate::clipboard::copy_text_async("late synthetic", true).is_err());
    assert!(!operation.permitted());
    assert!(authority.check(&AtomicBool::new(false)).is_err());
    finish_shutdown().unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "shutdown waited for provider/child normal deadlines"
    );
    assert!(
        result
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_err()
    );
    assert!(
        child_result
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_err()
    );
    assert!(
        !std::path::Path::new(&format!("/proc/{child_pid}")).exists(),
        "owned child was not reaped"
    );
    http.join();
    cli.join();
    peer.join().unwrap();
    assert_eq!(SESSION_TASKS.load(Ordering::Acquire), 0);
    assert!(THREADS.lock().unwrap().is_empty());
}

#[test]
fn shutdown_discards_queued_pool_and_finalization_work_and_closes_captured_stores() {
    const NAME: &str =
        "shutdown_discards_queued_pool_and_finalization_work_and_closes_captured_stores";
    if isolated(NAME) {
        return;
    }
    init().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = crate::store::Store::open_with_key(&path, &[0; 32]).unwrap();
    let (entered, started) = mpsc::channel();
    let (release, waiting) = mpsc::channel();
    let waiting = Arc::new(Mutex::new(waiting));
    for _ in 0..POOL_WORKERS {
        let entered = entered.clone();
        let waiting = waiting.clone();
        submit("synthetic-active", move || {
            entered.send(()).unwrap();
            waiting
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
        })
        .unwrap();
    }
    for _ in 0..POOL_WORKERS {
        started.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    let (entered, started) = mpsc::channel();
    let (final_release, final_waiting) = mpsc::channel();
    submit_finalization(move || {
        entered.send(()).unwrap();
        final_waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    })
    .unwrap();
    started.recv_timeout(Duration::from_secs(1)).unwrap();
    struct Captured {
        store: Option<crate::store::Store>,
        closed: mpsc::Sender<()>,
    }
    impl Drop for Captured {
        fn drop(&mut self) {
            drop(self.store.take());
            self.closed.send(()).unwrap();
        }
    }
    let (closed, cleaned) = mpsc::channel();
    let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for finalization in [false, false, true, true] {
        let captured = Captured {
            store: Some(crate::store::Store::open_with_key(&path, &[0; 32]).unwrap()),
            closed: closed.clone(),
        };
        let ran = ran.clone();
        let job = move || {
            ran.fetch_add(1, Ordering::AcqRel);
            drop(captured);
        };
        if finalization {
            submit_finalization(job).unwrap();
        } else {
            submit("synthetic-queued", job).unwrap();
        }
    }
    begin_shutdown();
    for _ in 0..POOL_WORKERS {
        release.send(()).unwrap();
    }
    final_release.send(()).unwrap();
    finish_shutdown().unwrap();
    for _ in 0..4 {
        cleaned.try_recv().unwrap();
    }
    assert_eq!(
        ran.load(Ordering::Acquire),
        0,
        "queued work executed after shutdown closed admission"
    );
    drain(Duration::ZERO).unwrap();
    store.checkpoint().unwrap();
    assert!(THREADS.lock().unwrap().is_empty());
}

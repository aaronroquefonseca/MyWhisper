//! Live clipboard/paste gate. Run only inside scripts/clipboard-smoke.sh's
//! disposable nested compositor; never borrows the user's real clipboard.
use gtk4::{gdk, glib, prelude::*};
use mywhisper::{
    clipboard,
    settings::{PasteBackend, PasteCombo, Preferences},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
fn wait_until(check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "native target did not receive expected text"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn main() {
    let root = std::path::PathBuf::from(
        std::env::var_os("MYWHISPER_CLIPBOARD_AUDIT_ROOT")
            .expect("use the disposable smoke runner"),
    );
    assert!(root.starts_with("/tmp") && root.join(".disposable").is_file());
    assert_eq!(
        std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from),
        Some(root.join("runtime"))
    );
    assert!(
        std::path::PathBuf::from(std::env::var_os("WAYLAND_DISPLAY").unwrap())
            .starts_with(root.join("runtime"))
    );
    gtk4::init().unwrap();
    let window = gtk4::Window::builder()
        .title("MyWhisper disposable clipboard gate")
        .default_width(600)
        .default_height(300)
        .build();
    let label = gtk4::Label::new(Some("Synthetic paste target. No real user data."));
    window.set_child(Some(&label));
    let native = gdk::Display::default().unwrap().clipboard();
    let mode = Arc::new(AtomicUsize::new(0));
    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let triggered = Arc::new(AtomicUsize::new(0));
    let keys = gtk4::EventControllerKey::new();
    let event_mode = mode.clone();
    let event_received = received.clone();
    let event_triggered = triggered.clone();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gdk::Key::v && modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
            event_triggered.fetch_add(1, Ordering::AcqRel);
            let mode = event_mode.load(Ordering::Acquire);
            let clipboard = native.clone();
            if mode == 1 {
                clipboard.set_text("synthetic user replacement");
            }
            if mode != 2 {
                let received = event_received.clone();
                glib::timeout_add_local_once(Duration::from_millis(700), move || {
                    clipboard.read_text_async(None::<&gtk4::gio::Cancellable>, move |result| {
                        received
                            .lock()
                            .unwrap()
                            .push(result.unwrap().unwrap_or_default().to_string());
                    });
                });
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);
    window.present();
    let main_loop = glib::MainLoop::new(None, false);
    let (done, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        let prefs = Preferences {
            auto_paste: true,
            keep_transcription_in_clipboard: false,
            paste_backend: PasteBackend::Wtype,
            paste_combo: PasteCombo::CtrlV,
            ..Default::default()
        };
        let permitted: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| true);
        for initial in [
            clipboard::SavedClipboard::Empty,
            clipboard::SavedClipboard::Text("synthetic previous value".into()),
            clipboard::SavedClipboard::Raw {
                mime: "image/png".into(),
                bytes: vec![137, 80, 78, 71, 0, 255],
            },
            clipboard::SavedClipboard::Formats(vec![
                ("text/plain".into(), b"synthetic plain".to_vec()),
                ("text/html".into(), b"<p>synthetic html</p>".to_vec()),
                ("image/png".into(), vec![137, 80, 78, 71, 0, 255]),
            ]),
        ] {
            clipboard::restore_clipboard(&initial).unwrap();
            received.lock().unwrap().clear();
            let started = Instant::now();
            clipboard::run_pipeline("synthetic transcript 🦀", &prefs, permitted.clone()).unwrap();
            assert!(
                started.elapsed() >= Duration::from_millis(650),
                "restored before delayed target consumed the selection"
            );
            wait_until(|| {
                received
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|s| s == "synthetic transcript 🦀")
            });
            match initial {
                clipboard::SavedClipboard::Empty => assert!(matches!(
                    clipboard::save_clipboard(),
                    Some(clipboard::SavedClipboard::Empty)
                )),
                clipboard::SavedClipboard::Text(text) => {
                    assert_eq!(clipboard::previous_text(), Some(text))
                }
                clipboard::SavedClipboard::Raw { mime, bytes } => {
                    match clipboard::save_clipboard().unwrap() {
                        clipboard::SavedClipboard::Formats(formats) => {
                            assert!(formats.contains(&(mime, bytes)))
                        }
                        _ => panic!("binary offer lost"),
                    }
                }
                clipboard::SavedClipboard::Formats(expected) => {
                    match clipboard::save_clipboard().unwrap() {
                        clipboard::SavedClipboard::Formats(formats) => {
                            for (mime, bytes) in expected {
                                assert!(
                                    formats.contains(&(mime.clone(), bytes.clone())),
                                    "multiple MIME offer lost: {mime}"
                                );
                            }
                        }
                        _ => panic!("multiple MIME offers lost"),
                    }
                }
            }
        }
        mode.store(1, Ordering::Release);
        clipboard::copy_text("synthetic earlier selection", false).unwrap();
        assert!(
            clipboard::run_pipeline("synthetic superseded transcript", &prefs, permitted.clone())
                .is_err()
        );
        assert_eq!(
            clipboard::previous_text(),
            Some("synthetic user replacement".into())
        );
        // Wait out the previous target read before testing cancellation.
        std::thread::sleep(Duration::from_millis(750));
        mode.store(2, Ordering::Release);
        clipboard::copy_text("synthetic before cancellation", false).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = cancelled.clone();
        let presses = triggered.clone();
        let before = presses.load(Ordering::Acquire);
        let cancellation = std::thread::spawn(move || {
            wait_until(|| presses.load(Ordering::Acquire) > before);
            cancel.store(true, Ordering::Release);
        });
        let guard: Arc<dyn Fn() -> bool + Send + Sync> =
            Arc::new(move || !cancelled.load(Ordering::Acquire));
        let started = Instant::now();
        assert!(clipboard::run_pipeline("synthetic cancelled transcript", &prefs, guard).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        cancellation.join().unwrap();
        assert_eq!(
            clipboard::previous_text(),
            Some("synthetic before cancellation".into())
        );
        mode.store(0, Ordering::Release);
        let keep = Preferences {
            auto_paste: false,
            keep_transcription_in_clipboard: true,
            ..prefs.clone()
        };
        clipboard::run_pipeline("synthetic copy only", &keep, permitted.clone()).unwrap();
        assert_eq!(
            clipboard::previous_text(),
            Some("synthetic copy only".into())
        );
        let noop = Preferences {
            auto_paste: false,
            keep_transcription_in_clipboard: false,
            ..prefs
        };
        clipboard::run_pipeline("must not replace selection", &noop, permitted).unwrap();
        assert_eq!(
            clipboard::previous_text(),
            Some("synthetic copy only".into())
        );
        done.send(()).unwrap();
    });
    let loop_handle = main_loop.clone();
    glib::timeout_add_local(Duration::from_millis(10), move || match result.try_recv() {
        Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
            loop_handle.quit();
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
    });
    let deadline_loop = main_loop.clone();
    glib::timeout_add_local_once(Duration::from_secs(30), move || deadline_loop.quit());
    main_loop.run();
    window.destroy();
    assert!(
        worker.is_finished(),
        "native clipboard smoke deadline exceeded"
    );
    worker.join().unwrap();
    mywhisper::runtime::finish_shutdown().expect("clipboard probe owned shutdown");
    println!(
        "Real nested niri/wtype passed: delayed consumption; empty/text/binary/multiple-MIME restoration; user replacement; cancellation; copy-only/no-op."
    );
}

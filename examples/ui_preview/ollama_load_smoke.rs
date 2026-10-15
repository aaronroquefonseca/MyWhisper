//! Real GTK cancellation against a stalled synthetic Ollama endpoint.
use gtk4::prelude::*;
use mywhisper::i18n::tr;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn run(app: &gtk4::Application, window: &gtk4::ApplicationWindow, change_inputs: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (send, received) = mpsc::channel();
    let _fixture = mywhisper::runtime::spawn_session("synthetic-model-load", move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let n = socket.read(&mut buffer).unwrap();
            assert!(n != 0);
            bytes.extend_from_slice(&buffer[..n]);
            assert!(bytes.len() < 8192);
            if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|value| value.parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    assert!(headers.starts_with("POST /api/chat HTTP/1.1"));
                    let value: serde_json::Value =
                        serde_json::from_slice(&bytes[end + 4..]).unwrap();
                    assert_eq!(value["messages"], serde_json::json!([]));
                    assert_eq!(value["model"], "synthetic-model");
                    break;
                }
            }
        }
        send.send(false).unwrap(); // Request has reached the endpoint.
        // Bounded EOF wait, not a single-shot read: page removal,
        // cancellation and TCP close propagate on their own schedule,
        // so one 500ms read races the close it is meant to observe.
        // EOF itself is the synchronization signal; the pre-existing
        // outer 5s smoke guard bounds the wait. A missing cancellation
        // never produces EOF, so the closed assert below still fails
        // deterministically without it. The per-read 500ms timeout is
        // unchanged; only the single attempt becomes a bounded wait.
        let eof_by = Instant::now() + Duration::from_secs(4);
        let closed = loop {
            if Instant::now() >= eof_by {
                break false;
            }
            match socket.read(&mut buffer) {
                Ok(0) => break true,
                Ok(_) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(error) => panic!("model-load fixture read failed: {error:?}"),
            }
        };
        // Release a regressed blocking request before reporting the failure.
        if !closed {
            let response = br#"{"done":true,"message":{"content":""}}"#;
            let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            );
            let _ = socket.write_all(response);
        }
        send.send(closed).unwrap();
    })
    .unwrap();
    let app = app.clone();
    let window = window.clone();
    let started = Instant::now();
    let mut removed = false;
    let mut old_button = None;
    gtk4::glib::timeout_add_local(Duration::from_millis(10), move || {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "native model-load check stalled"
        );
        if old_button.is_none() {
            super::smoke::expand(&window, false);
            let root = window.upcast_ref();
            let input = super::smoke::descendants(root)
                .into_iter()
                .find_map(|widget| {
                    widget
                        .downcast::<gtk4::Entry>()
                        .ok()
                        .filter(|entry| entry.text() == "http://127.0.0.1:11434")
                })
                .unwrap();
            input.set_text(&url);
            super::smoke::named::<gtk4::Entry>(root, "lm-ollama-model").set_text("synthetic-model");
            let button = super::smoke::descendants(root)
                .into_iter()
                .find_map(|widget| {
                    widget
                        .downcast::<gtk4::Button>()
                        .ok()
                        .filter(|button| button.label().as_deref() == Some(tr("action.updatenow")))
                })
                .unwrap();
            assert!(button.is_mapped());
            old_button = Some(button.downgrade());
            let before = Instant::now();
            button.emit_clicked();
            assert!(before.elapsed() < Duration::from_millis(100));
            return gtk4::glib::ControlFlow::Continue;
        }
        match received.try_recv() {
            Ok(false) if !removed => {
                if change_inputs {
                    let before = Instant::now();
                    super::smoke::named::<gtk4::Entry>(window.upcast_ref(), "lm-ollama-model")
                        .set_text("replacement-model");
                    assert!(before.elapsed() < Duration::from_millis(100));
                    let button = old_button.as_ref().unwrap().upgrade().unwrap();
                    assert!(button.is_sensitive());
                    assert_eq!(button.label().as_deref(), Some(tr("action.updatenow")));
                } else {
                    let settings: gtk4::Stack =
                        super::smoke::named(window.upcast_ref(), "settings-pages");
                    let page = settings.child_by_name("Language Models").unwrap();
                    settings.set_visible_child_name("Speech to Text");
                    settings.remove(&page);
                }
                removed = true;
            }
            Ok(closed) => {
                assert!(
                    closed,
                    "removing the originating page must close active provider IO within the smoke bound"
                );
                if change_inputs {
                    let button = old_button.as_ref().unwrap().upgrade().unwrap();
                    assert!(button.is_sensitive());
                    assert_eq!(button.label().as_deref(), Some(tr("action.updatenow")));
                    assert_eq!(
                        super::smoke::named::<gtk4::Entry>(window.upcast_ref(), "lm-ollama-model")
                            .text(),
                        "replacement-model"
                    );
                    assert!(
                        !super::smoke::descendants(window.upcast_ref())
                            .iter()
                            .any(|widget| {
                                widget.downcast_ref::<gtk4::Label>().is_some_and(|label| {
                                    label.text() == tr("lm.applied")
                                        || label.text() == tr("lm.applyfailed")
                                })
                            }),
                        "cancelled old requests must not publish a late result"
                    );
                    println!(
                        "Native model load: input replacement cancels owned IO, preserves the new draft and rejects old results."
                    );
                } else {
                    assert!(
                        old_button.as_ref().unwrap().upgrade().is_none(),
                        "a pending request must not retain the removed button"
                    );
                    println!(
                        "Native model load: owned stalled-request cancellation, removed-button release and responsive callback passed."
                    );
                }
                app.quit();
                return gtk4::glib::ControlFlow::Break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(error) => panic!("model-load fixture failed: {error}"),
        }
        gtk4::glib::ControlFlow::Continue
    });
}

/// Actual provider endpoint supplied by the namespace-owned integration runner.
pub fn live(app: &gtk4::Application, window: &gtk4::ApplicationWindow) {
    let url = std::env::var("MYWHISPER_OLLAMA_AUDIT_URL").expect("private live provider URL");
    let model = std::env::var("MYWHISPER_OLLAMA_AUDIT_MODEL").expect("private live model name");
    let unload = std::env::var("MYWHISPER_OLLAMA_AUDIT_UNLOAD").as_deref() == Ok("1");
    assert!(url.starts_with("http://127.0.0.1:"));
    let app = app.clone();
    let window = window.clone();
    let started = Instant::now();
    let mut requested = false;
    gtk4::glib::timeout_add_local(Duration::from_millis(20), move || {
        assert!(
            started.elapsed() < Duration::from_secs(65),
            "actual Ollama model control stalled"
        );
        let root = window.upcast_ref();
        if !requested {
            super::smoke::expand(&window, false);
            let input = super::smoke::descendants(root)
                .into_iter()
                .find_map(|widget| {
                    widget
                        .downcast::<gtk4::Entry>()
                        .ok()
                        .filter(|entry| entry.text() == "http://127.0.0.1:11434")
                })
                .unwrap();
            input.set_text(&url);
            super::smoke::named::<gtk4::Entry>(root, "lm-ollama-model").set_text(&model);
            super::smoke::named::<gtk4::DropDown>(root, "lm-ollama-keepalive")
                .set_selected(if unload { 2 } else { 3 });
            let button = super::smoke::descendants(root)
                .into_iter()
                .find_map(|widget| {
                    widget
                        .downcast::<gtk4::Button>()
                        .ok()
                        .filter(|button| button.label().as_deref() == Some(tr("action.updatenow")))
                })
                .unwrap();
            let before = Instant::now();
            button.emit_clicked();
            assert!(before.elapsed() < Duration::from_millis(100));
            requested = true;
        }
        let widgets = super::smoke::descendants(root);
        assert!(
            !widgets.iter().any(|widget| widget
                .downcast_ref::<gtk4::Label>()
                .is_some_and(|label| label.text() == tr("lm.applyfailed"))),
            "actual model-control request failed"
        );
        if widgets.iter().any(|widget| {
            widget
                .downcast_ref::<gtk4::Label>()
                .is_some_and(|label| label.text() == tr("lm.applied"))
        }) {
            println!(
                "Actual native Ollama model control passed: {} and visible completion.",
                if unload { "unload" } else { "load" }
            );
            app.quit();
            return gtk4::glib::ControlFlow::Break;
        }
        gtk4::glib::ControlFlow::Continue
    });
}

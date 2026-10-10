//! Exercise real GTK signals against the preview's disposable in-memory store.
use gtk4::prelude::*;
use libadwaita::prelude::*;
use mywhisper::{app::AppState, i18n::tr};
use std::rc::Rc;

fn descendants(root: &gtk4::Widget) -> Vec<gtk4::Widget> {
    let mut all = vec![root.clone()];
    let mut child = root.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        all.extend(descendants(&widget));
    }
    all
}

pub fn expand(window: &gtk4::ApplicationWindow, snippets: bool) {
    for widget in descendants(window.upcast_ref()) {
        if let Some(expander) = widget.downcast_ref::<gtk4::Expander>() {
            expander.set_expanded(true);
        }
        if snippets && let Some(notebook) = widget.downcast_ref::<gtk4::Notebook>() {
            notebook.set_current_page(Some(1));
        }
    }
}

fn button(root: &gtk4::Widget, tag: &str) -> gtk4::Button {
    let main: gtk4::Stack = named(root, "main-pages");
    let mut page = main.visible_child().unwrap();
    if main.visible_child_name().as_deref() == Some("settings") {
        let settings: gtk4::Stack = named(&page, "settings-pages");
        page = settings.visible_child().unwrap();
    }
    if main.visible_child_name().as_deref() == Some("dictionary") {
        let tabs = descendants(&page)
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk4::Notebook>().ok())
            .unwrap();
        page = tabs.nth_page(tabs.current_page()).unwrap();
    }
    let button = descendants(&page)
        .into_iter()
        .find_map(|widget| {
            widget
                .downcast::<gtk4::Button>()
                .ok()
                .filter(|button| button.is_visible() && button.label().as_deref() == Some(tr(tag)))
        })
        .unwrap_or_else(|| panic!("Visible button missing: {tag}"));
    button.grab_focus();
    button
}

fn named<T: IsA<gtk4::Widget> + gtk4::glib::object::IsClass>(root: &gtk4::Widget, name: &str) -> T {
    descendants(root)
        .into_iter()
        .find(|widget| widget.widget_name() == name)
        .unwrap()
        .downcast::<T>()
        .ok()
        .unwrap()
}

fn prompt_editor(app: &gtk4::Application) -> gtk4::Window {
    app.windows()
        .into_iter()
        .find(|window| window.widget_name() == "cleanup-prompt-editor")
        .unwrap()
}

fn respond(window: &gtk4::ApplicationWindow, response: &str) {
    let host = window
        .clone()
        .downcast::<libadwaita::ApplicationWindow>()
        .unwrap();
    let dialog = host
        .visible_dialog()
        .unwrap()
        .downcast::<libadwaita::AlertDialog>()
        .unwrap();
    let buttons: Vec<_> = descendants(dialog.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk4::Button>().ok())
        .filter(|button| button.label().is_some())
        .collect();
    let target = buttons
        .iter()
        .find(|button| {
            if response == "cancel" {
                button.label().as_deref() == Some(tr("action.cancel"))
            } else {
                button.has_css_class("destructive-action")
            }
        })
        .unwrap();
    target.grab_focus();
    target.emit_clicked();
}

fn release_regressions(app: &gtk4::Application) {
    {
        let overlay = mywhisper::overlay::VoiceOverlay::new(app);
        overlay.show_error();
        assert_eq!(overlay.state(), mywhisper::overlay::OverlayState::Error);
        assert!(
            overlay.capture_error().is_none(),
            "error UI must not open a microphone"
        );
        drop(overlay);
    }

    for _ in 0..20 {
        let visualizer = mywhisper::viz::Visualizer::new();
        let widget = visualizer.widget().downgrade();
        drop(visualizer);
        assert!(
            widget.upgrade().is_none(),
            "removed visualizer must release its frame timer and microphone owner"
        );
    }

    let directory = tempfile::tempdir().unwrap();
    let mut state = AppState::fresh();
    Rc::get_mut(&mut state).unwrap().db_path = directory.path().join("state.db");
    state.settings.borrow_mut().sync.enabled = false;
    state
        .store
        .upsert_word("synthetic audit word", "audit")
        .unwrap();
    for _ in 0..20 {
        let page = mywhisper::app_dictionary::page(&state);
        let card = descendants(page.upcast_ref())
            .into_iter()
            .find(|widget| {
                widget.has_css_class("card")
                    && widget
                        .first_child()
                        .and_then(|child| child.downcast::<gtk4::Label>().ok())
                        .is_some_and(|label| label.text() == "synthetic audit word")
            })
            .unwrap();
        let weak = card.downgrade();
        drop(card);
        drop(page);
        assert!(
            weak.upgrade().is_none(),
            "removed dictionary card must release its signals/widgets"
        );
    }
    {
        use mywhisper::settings::{LocalSttProvider, SttChainEntry, SttEntryKind};
        let mut settings = state.settings.borrow_mut();
        settings.stt.local_provider = LocalSttProvider::Nvidia;
        settings.stt.parakeet_model = "nemotron-speech-streaming-en-0.6b".into();
        settings.stt.language = "es".into();
        settings.stt.entries = vec![SttChainEntry {
            id: "remote".into(),
            name: "Synthetic Spanish remote".into(),
            kind: SttEntryKind::SelfHosted,
            enabled: true,
            provider: String::new(),
            url: "http://127.0.0.1:1".into(),
            model: "test".into(),
            api_key: String::new(),
            streaming: false,
        }];
    }
    let page = mywhisper::app_settings::page(&state, "Speech to Text");
    let language: gtk4::DropDown = named(page.upcast_ref(), "transcription-language-picker");
    assert_eq!(
        language
            .selected_item()
            .unwrap()
            .downcast::<gtk4::StringObject>()
            .unwrap()
            .string(),
        "es"
    );
    assert_eq!(
        state.settings.borrow().stt.language,
        "es",
        "rendering must preserve remote language"
    );
    drop(language);
    drop(page);
    // Removing the remote must not make rendering silently select English
    // for the remembered English-only local model. Show the existing value
    // and let the user correct it; local plan construction rejects it.
    state.settings.borrow_mut().stt.entries.clear();
    let page = mywhisper::app_settings::page(&state, "Speech to Text");
    let language: gtk4::DropDown = named(page.upcast_ref(), "transcription-language-picker");
    assert_eq!(
        language
            .selected_item()
            .unwrap()
            .downcast::<gtk4::StringObject>()
            .unwrap()
            .string(),
        "es"
    );
    assert!(
        language.is_sensitive(),
        "unsupported stored language must be editable"
    );
    assert_eq!(state.settings.borrow().stt.language, "es");
    drop(language);
    drop(page);
    state.settings.borrow_mut().stt.local_provider = mywhisper::settings::LocalSttProvider::Cohere;
    let page = mywhisper::app_settings::page(&state, "Speech to Text");
    let cohere = descendants(page.upcast_ref())
        .into_iter()
        .find_map(|widget| {
            widget
                .downcast::<gtk4::ToggleButton>()
                .ok()
                .filter(|button| button.label().as_deref() == Some("Cohere"))
        })
        .unwrap();
    assert!(
        cohere.is_sensitive() && cohere.is_active(),
        "Cohere must be selectable with the authenticated runtime contract"
    );
    let runtime: gtk4::Box = named(page.upcast_ref(), "sherpa-runtime-controls");
    assert!(
        runtime.is_visible(),
        "Cohere must expose its downloadable runtime controls"
    );
    drop(runtime);
    let language: gtk4::DropDown = named(page.upcast_ref(), "transcription-language-picker");
    let options = language.model().unwrap();
    assert_eq!(options.n_items(), 14);
    let codes = (0..options.n_items())
        .map(|index| {
            options
                .item(index)
                .unwrap()
                .downcast::<gtk4::StringObject>()
                .unwrap()
                .string()
        })
        .collect::<Vec<_>>();
    assert!(codes.iter().any(|code| code == "es"));
    assert!(!codes.iter().any(|code| code == "auto"));
    assert_eq!(state.settings.borrow().stt.language, "es");
    assert!(descendants(page.upcast_ref()).iter().any(|widget| {
        widget
            .downcast_ref::<gtk4::Label>()
            .is_some_and(|label| label.text() == tr("stt.models.coherelanguage"))
    }));
    let model = descendants(page.upcast_ref())
        .into_iter()
        .find(|widget| widget.tooltip_text().as_deref() == Some("cohere-transcribe-03-2026"))
        .unwrap();
    assert!(
        descendants(&model).iter().any(|widget| widget
            .downcast_ref::<gtk4::Button>()
            .is_some_and(|button| button.is_sensitive())),
        "Cohere download must be available"
    );
    drop(model);
    drop(options);
    drop(language);
    drop(cohere);
    drop(page);
    // Disabling new recordings/history must not disable the policy for
    // previously retained data. Rendering custom/zero policies preserves
    // their stored values rather than displaying a different default.
    for (audio_days, transcript_days) in [(2, 45), (0, 0)] {
        {
            let mut settings = state.settings.borrow_mut();
            settings.privacy.keep_audio_recordings = false;
            settings.privacy.history_enabled = false;
            settings.privacy.audio_retention_days = audio_days;
            settings.privacy.transcript_auto_delete_days = transcript_days;
        }
        let page = mywhisper::app_settings::page(&state, "Privacy & Data");
        let audio: gtk4::DropDown = named(page.upcast_ref(), "audio-retention-picker");
        let history: gtk4::DropDown = named(page.upcast_ref(), "transcript-retention-picker");
        assert!(audio.is_sensitive() && history.is_sensitive());
        let audio_text = audio
            .selected_item()
            .unwrap()
            .downcast::<gtk4::StringObject>()
            .unwrap()
            .string();
        let history_text = history
            .selected_item()
            .unwrap()
            .downcast::<gtk4::StringObject>()
            .unwrap()
            .string();
        if audio_days == 0 {
            assert_eq!(audio_text, tr("opt.older1"));
            assert_eq!(history_text, tr("opt.never"));
        } else {
            assert_eq!(audio_text, tr("opt.oldercustom").replace("{n}", "2"));
            assert_eq!(history_text, tr("opt.oldercustom").replace("{n}", "45"));
        }
        assert_eq!(
            state.settings.borrow().privacy.audio_retention_days,
            audio_days
        );
        assert_eq!(
            state.settings.borrow().privacy.transcript_auto_delete_days,
            transcript_days
        );
        audio.set_selected(1);
        history.set_selected(1);
        assert_eq!(state.store.load_settings().privacy.audio_retention_days, 7);
        assert_eq!(
            state
                .store
                .load_settings()
                .privacy
                .transcript_auto_delete_days,
            1
        );
        drop(audio);
        drop(history);
        drop(page);
    }
    // A settings rebuild must release forms and list-row signals too, not
    // merely its outer page. Timers get one turn to observe the removed page.
    for section in ["Language Models", "Speech to Text"]
        .into_iter()
        .cycle()
        .take(6)
    {
        let page = mywhisper::app_settings::page(&state, section);
        let widgets: Vec<_> = descendants(page.upcast_ref())
            .into_iter()
            .filter(|widget| {
                widget.is::<gtk4::Entry>()
                    || widget.is::<gtk4::Button>()
                    || widget.is::<gtk4::Expander>()
                    || widget.widget_name() == "local-server-status"
            })
            .map(|widget| (widget.type_().name().to_string(), widget.downgrade()))
            .collect();
        assert!(!widgets.is_empty());
        drop(page);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            while gtk4::glib::MainContext::default().pending() {
                gtk4::glib::MainContext::default().iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let retained: Vec<_> = widgets
            .iter()
            .filter_map(|(kind, weak)| {
                weak.upgrade()
                    .map(|widget| format!("{kind}: {}", widget.widget_name()))
            })
            .collect();
        assert!(
            retained.is_empty(),
            "removed settings retain widgets: {retained:?}"
        );
    }
    // Exercise the mapped page's real asynchronous socket query. Switching
    // configuration must invalidate its previous result; removing the page
    // must release all status controls while its polling source still exists.
    {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        state.settings.borrow_mut().stt.server_port =
            listener.local_addr().unwrap().port().to_string();
        let page = mywhisper::app_settings::page(&state, "Speech to Text");
        for widget in descendants(page.upcast_ref()) {
            if let Some(expander) = widget.downcast_ref::<gtk4::Expander>() {
                expander.set_expanded(true);
            }
        }
        let status: gtk4::Label = named(page.upcast_ref(), "local-server-status");
        let stop: gtk4::Button = named(page.upcast_ref(), "local-server-stop");
        let cold: gtk4::Button = named(page.upcast_ref(), "local-server-start");
        let window = gtk4::Window::builder()
            .application(app)
            .child(&page)
            .build();
        window.present();
        let await_status = |expected: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while status.text() != expected {
                assert!(
                    std::time::Instant::now() < deadline,
                    "status did not update: expected {expected}, got {}",
                    status.text()
                );
                while gtk4::glib::MainContext::default().pending() {
                    gtk4::glib::MainContext::default().iteration(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        let expected = format!(
            "{}: {}",
            tr("server.running"),
            ["whisper", "sherpa-offline", "sherpa-online"]
                .map(|name| format!("{name} ({})", tr("server.external")))
                .join(", ")
        );
        await_status(&expected);
        assert!(
            !stop.is_visible(),
            "a foreign socket does not grant stop ownership"
        );
        assert!(!cold.is_visible());
        // Reserve a different owned port without listening, so the negative
        // result cannot depend on another host service or a freed-port race.
        let closed =
            socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
        closed
            .bind(
                &"127.0.0.1:0"
                    .parse::<std::net::SocketAddr>()
                    .unwrap()
                    .into(),
            )
            .unwrap();
        state.settings.borrow_mut().stt.server_port = closed
            .local_addr()
            .unwrap()
            .as_socket()
            .unwrap()
            .port()
            .to_string();
        await_status(tr("server.none"));
        assert!(cold.is_visible());
        assert!(!stop.is_visible());
        let weak = [
            status.upcast::<gtk4::Widget>(),
            stop.upcast(),
            cold.upcast(),
        ]
        .map(|widget| widget.downgrade());
        window.set_child(None::<&gtk4::Widget>);
        window.close();
        drop(window);
        drop(page);
        assert!(
            weak.iter().all(|widget| widget.upgrade().is_none()),
            "a removed server page retained its status controls: {:?}",
            weak.iter()
                .filter_map(|widget| widget
                    .upgrade()
                    .map(|widget| widget.widget_name().to_string()))
                .collect::<Vec<_>>()
        );
        state.settings.borrow_mut().stt.server_port.clear();
    }
    // A simulated engine owner withholds acknowledgement. Keys must be
    // swallowed while waiting; only the current nonce can unlock capture.
    {
        use std::os::unix::fs::OpenOptionsExt;
        let lock = |path: &std::path::Path| -> std::io::Result<std::fs::File> {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)?;
            fs2::FileExt::try_lock_exclusive(&file)?;
            Ok(file)
        };
        let write = |path: &std::path::Path, bytes: &[u8]| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
                .unwrap();
            file.write_all(bytes).unwrap();
        };
        let bindings = || {
            state
                .settings
                .borrow()
                .hotkeys
                .effective_hotkeys()
                .into_iter()
                .map(|binding| (binding.combo, binding.mode))
                .collect::<Vec<_>>()
        };
        let _engine = lock(&directory.path().join("engine.lock")).unwrap();
        let page = mywhisper::app_settings::page(&state, "Hotkeys");
        let capture_page: gtk4::Box = named(page.upcast_ref(), "hotkey-capture-page");
        let start: gtk4::Button = named(page.upcast_ref(), "hotkey-capture-start");
        let entry: gtk4::Entry = named(page.upcast_ref(), "hotkey-capture-entry");
        let controllers = capture_page.observe_controllers();
        let controller = (0..controllers.n_items())
            .filter_map(|index| {
                controllers
                    .item(index)
                    .and_then(|item| item.downcast::<gtk4::EventControllerKey>().ok())
            })
            .next()
            .unwrap();
        let keys_before = bindings();
        let send = || {
            controller.emit_by_name::<bool>(
                "key-pressed",
                &[
                    &gtk4::gdk::Key::F14,
                    &192u32,
                    &gtk4::gdk::ModifierType::empty(),
                ],
            )
        };
        let drain = || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(60);
            while std::time::Instant::now() < deadline {
                gtk4::glib::MainContext::default().iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        start.emit_clicked();
        assert!(!entry.is_sensitive());
        assert_eq!(start.label().as_deref(), Some(tr("hotkeys.suspending")));
        assert!(send(), "waiting capture must swallow keys");
        assert_eq!(bindings(), keys_before);
        let request = directory.path().join("hotkey-capture.request");
        let ack = directory.path().join("hotkey-capture.ack");
        let nonce = std::fs::read(&request).unwrap();
        let mut stale = nonce.clone();
        stale[0] ^= 1;
        write(&ack, &stale);
        drain();
        assert_eq!(start.label().as_deref(), Some(tr("hotkeys.suspending")));
        write(&ack, &nonce);
        drain();
        assert!(send());
        assert!(
            state
                .settings
                .borrow()
                .hotkeys
                .effective_hotkeys()
                .iter()
                .any(|key| key.combo == "F14")
        );
        assert!(entry.is_sensitive());
        assert!(!request.exists());
        start.emit_clicked();
        let nonce = std::fs::read(&request).unwrap();
        write(&ack, &nonce);
        drain();
        assert!(controller.emit_by_name::<bool>(
            "key-pressed",
            &[
                &gtk4::gdk::Key::ntilde,
                &47u32,
                &gtk4::gdk::ModifierType::empty()
            ]
        ));
        assert!(
            state
                .settings
                .borrow()
                .hotkeys
                .effective_hotkeys()
                .iter()
                .any(|binding| binding.combo == "Semicolon"),
            "Spanish ñ keyval must retain the physical punctuation position"
        );
        start.emit_clicked();
        capture_page.emit_by_name::<()>("unmap", &[]);
        assert!(
            lock(&directory.path().join("hotkey-capture.lock")).is_ok(),
            "leaving capture page must release cross-process suspension"
        );
        assert!(!request.exists());
    }
    let node = "界".repeat(20);
    state.discovered.lock().unwrap().insert(
        node.clone(),
        mywhisper::net::Discovered {
            node_id: node,
            name: "Malformed unauthenticated discovery".into(),
            addresses: vec!["127.0.0.1:1".into()],
        },
    );
    let page = mywhisper::app_sync::page(&state);
    let released: Vec<_> = descendants(page.upcast_ref())
        .into_iter()
        .filter(|widget| {
            widget.is::<gtk4::Button>()
                || widget.is::<gtk4::Entry>()
                || matches!(
                    widget.widget_name().as_str(),
                    "sync-listener-status" | "sync-reachability-status"
                )
        })
        .map(|widget| widget.downgrade())
        .collect();
    drop(page);
    assert!(
        released.iter().all(|weak| weak.upgrade().is_none()),
        "removed sync page must release controls and status polling widgets"
    );
    eprintln!(
        "[ui-smoke] release regressions passed: widget lifetime, remote language, malformed discovery"
    );
}

pub fn run(app: &gtk4::Application, state: &Rc<AppState>, window: &gtk4::ApplicationWindow) {
    release_regressions(app);
    let app = app.clone();
    let state = state.clone();
    let window = window.clone();
    let root: gtk4::Widget = window.clone().upcast();
    let mut step = 0;
    let original_words = state.store.load_words().unwrap().len();
    let original_history = state.store.load_history(200, true).unwrap().len();
    gtk4::glib::timeout_add_local(std::time::Duration::from_millis(400), move || {
        eprintln!("[ui-smoke] step {step}");
        let main: gtk4::Stack = named(&root, "main-pages");
        match step {
            0 => main.set_visible_child_name("dictionary"),
            1 => {
                let entry: gtk4::Entry = named(&root, "new-word");
                entry.set_text("");
                button(&root, "dict.words.create").emit_clicked();
                assert!(entry.has_css_class("error"));
                entry.set_text("PreviewTerm");
                entry.emit_activate();
                assert_eq!(state.store.load_words().unwrap().len(), original_words + 1);
                entry.set_text("previewterm");
                button(&root, "dict.words.create").emit_clicked();
                assert!(entry.has_css_class("error"));
                assert_eq!(state.store.load_words().unwrap().len(), original_words + 1);
                button(&root, "action.edit").emit_clicked();
            }
            2 => {
                let draft = descendants(&root)
                    .into_iter()
                    .find_map(|widget| {
                        widget
                            .downcast::<gtk4::Entry>()
                            .ok()
                            .filter(|entry| entry.is_mapped() && entry.widget_name() != "new-word")
                    })
                    .unwrap();
                draft.set_text("UnsavedDraft");
                // A write triggers the background refresh while this draft is open.
                state
                    .store
                    .upsert_word("RemoteTerm", "other-device")
                    .unwrap();
            }
            3..=5 => {}
            6 => {
                assert!(
                    descendants(&root).iter().any(|widget| {
                        widget
                            .downcast_ref::<gtk4::Entry>()
                            .is_some_and(|entry| entry.text() == "UnsavedDraft")
                    }),
                    "Background refresh erased the edit"
                );
                button(&root, "action.cancel").emit_clicked();
                assert!(
                    !state
                        .store
                        .load_words()
                        .unwrap()
                        .contains(&"UnsavedDraft".into())
                );
                button(&root, "action.delete").emit_clicked();
            }
            7 => {
                respond(&window, "cancel");
                assert_eq!(state.store.load_words().unwrap().len(), original_words + 2);
            }
            8 => button(&root, "action.delete").emit_clicked(),
            9 => {
                respond(&window, "confirm");
                assert_eq!(state.store.load_words().unwrap().len(), original_words + 1);
            }
            10 => {
                let notebook = descendants(&root)
                    .into_iter()
                    .find_map(|widget| widget.downcast::<gtk4::Notebook>().ok())
                    .unwrap();
                notebook.set_current_page(Some(1));
            }
            11 => {
                let trigger: gtk4::Entry = named(&root, "snippet-trigger");
                let text: gtk4::TextView = named(&root, "snippet-expansion");
                trigger.set_text("test signature, test sign off");
                text.buffer().set_text("Line one\nLine two");
                button(&root, "dict.snippets.create").emit_clicked();
                let saved = state.store.load_snippets().unwrap();
                assert!(
                    saved
                        .iter()
                        .any(|snippet| snippet.replacement == "Line one\nLine two")
                );
                trigger.set_text("TEST SIGN OFF");
                text.buffer().set_text("Duplicate");
                button(&root, "dict.snippets.create").emit_clicked();
                assert!(trigger.has_css_class("error"));
                assert_eq!(state.store.load_snippets().unwrap().len(), saved.len());
                button(&root, "action.edit").emit_clicked();
                let edit = descendants(&main.visible_child().unwrap())
                    .into_iter()
                    .find_map(|widget| {
                        widget.downcast::<gtk4::TextView>().ok().filter(|view| {
                            view.is_visible() && view.widget_name() != "snippet-expansion"
                        })
                    })
                    .unwrap();
                edit.buffer().set_text("Edited line one\nEdited line two");
                button(&root, "action.save").emit_clicked();
                assert!(
                    state
                        .store
                        .load_snippets()
                        .unwrap()
                        .iter()
                        .any(|snippet| snippet.replacement == "Edited line one\nEdited line two")
                );
                main.set_visible_child_name("home");
            }
            12 => {
                let search = descendants(&root)
                    .into_iter()
                    .find_map(|widget| widget.downcast::<gtk4::SearchEntry>().ok())
                    .unwrap();
                search.set_text("no dictation matches this query");
            }
            13 => {
                assert!(descendants(&root).iter().any(|widget| {
                    widget.downcast_ref::<gtk4::Label>().is_some_and(|label| {
                        label.is_mapped() && label.text() == tr("home.noresults")
                    })
                }));
                button(&root, "home.setup").emit_clicked();
                assert_eq!(main.visible_child_name().as_deref(), Some("settings"));
                let settings: gtk4::Stack = named(&root, "settings-pages");
                assert_eq!(
                    settings.visible_child_name().as_deref(),
                    Some("Speech to Text")
                );
                settings.set_visible_child_name("Language Models");
            }
            14 => button(&root, "action.edit").emit_clicked(),
            15 => {
                button(&root, "action.cancel").emit_clicked();
                button(&root, "action.cleanupsettings").emit_clicked();
            }
            16 => {
                assert_eq!(
                    named::<gtk4::Stack>(&root, "settings-pages")
                        .visible_child_name()
                        .as_deref(),
                    Some("Text Cleanup")
                );
                named::<gtk4::Switch>(&root, "cleanup-enabled").set_active(false);
                assert!(!state.store.load_settings().language_models.cleanup_enabled);
                assert_eq!(
                    named::<gtk4::Label>(&root, "cleanup-prompt-status").text(),
                    tr("prompt.default")
                );
                button(&root, "action.editprompt").emit_clicked();
            }
            17 => {
                let editor = prompt_editor(&app);
                let text: gtk4::TextView = named(editor.upcast_ref(), "expanded-cleanup-prompt");
                let save: gtk4::Button = named(editor.upcast_ref(), "prompt-editor-save");
                assert!(!save.is_sensitive());
                assert!(editor.is_resizable());
                text.buffer()
                    .set_text(&"Keep the speaker’s meaning. Fix punctuation.\n".repeat(100));
                assert!(save.is_sensitive());
                assert!(
                    state
                        .store
                        .load_settings()
                        .language_models
                        .cleanup_prompt
                        .is_empty()
                );
                named::<gtk4::Button>(editor.upcast_ref(), "prompt-editor-default").emit_clicked();
                assert!(!save.is_sensitive());
                assert!(
                    state
                        .store
                        .load_settings()
                        .language_models
                        .cleanup_prompt
                        .is_empty()
                );
                text.buffer().set_text("This edit must be canceled.");
                named::<gtk4::Button>(editor.upcast_ref(), "prompt-editor-cancel").emit_clicked();
            }
            18 => {
                assert!(
                    !app.windows()
                        .iter()
                        .any(|window| window.widget_name() == "cleanup-prompt-editor")
                );
                assert!(
                    state
                        .store
                        .load_settings()
                        .language_models
                        .cleanup_prompt
                        .is_empty()
                );
                assert_eq!(
                    named::<gtk4::Label>(&root, "cleanup-prompt-status").text(),
                    tr("prompt.default")
                );
                button(&root, "action.editprompt").emit_clicked();
            }
            19 => {
                let editor = prompt_editor(&app);
                let text: gtk4::TextView = named(editor.upcast_ref(), "expanded-cleanup-prompt");
                text.buffer()
                    .set_text(&"Keep the speaker’s meaning. Fix punctuation.\n".repeat(100));
                named::<gtk4::Button>(editor.upcast_ref(), "prompt-editor-save").emit_clicked();
            }
            20 => {
                assert_eq!(
                    state.store.load_settings().language_models.cleanup_prompt,
                    "Keep the speaker’s meaning. Fix punctuation.\n".repeat(100)
                );
                assert_eq!(
                    named::<gtk4::Label>(&root, "cleanup-prompt-status").text(),
                    tr("prompt.custom")
                );
                let preview: gtk4::Label = named(&root, "cleanup-prompt-preview");
                assert!(preview.text().chars().count() <= 240);
                button(&root, "action.editprompt").emit_clicked();
            }
            21 => {
                let editor = prompt_editor(&app);
                let text: gtk4::TextView = named(editor.upcast_ref(), "expanded-cleanup-prompt");
                let buffer = text.buffer();
                assert_eq!(
                    buffer.text(&buffer.start_iter(), &buffer.end_iter(), false),
                    "Keep the speaker’s meaning. Fix punctuation.\n".repeat(100)
                );
                named::<gtk4::Button>(editor.upcast_ref(), "prompt-editor-default").emit_clicked();
                editor.close();
                assert_eq!(
                    state.store.load_settings().language_models.cleanup_prompt,
                    "Keep the speaker’s meaning. Fix punctuation.\n".repeat(100)
                );
                button(&root, "action.editprompt").emit_clicked();
            }
            22 => {
                let editor = prompt_editor(&app);
                named::<gtk4::Button>(editor.upcast_ref(), "prompt-editor-default").emit_clicked();
                named::<gtk4::Button>(editor.upcast_ref(), "prompt-editor-save").emit_clicked();
            }
            23 => {
                assert_eq!(
                    state.store.load_settings().language_models.cleanup_prompt,
                    mywhisper::transcribe::DEFAULT_CLEANUP_PROMPT
                );
                assert_eq!(
                    named::<gtk4::Label>(&root, "cleanup-prompt-status").text(),
                    tr("prompt.default")
                );
                named::<gtk4::Switch>(&root, "cleanup-enabled").set_active(true);
                assert!(state.store.load_settings().language_models.cleanup_enabled);
                button(&root, "action.managelm").emit_clicked();
            }
            24 => {
                let settings: gtk4::Stack = named(&root, "settings-pages");
                assert_eq!(
                    settings.visible_child_name().as_deref(),
                    Some("Language Models")
                );
                let page = settings.visible_child().unwrap();
                assert!(
                    !descendants(&page)
                        .iter()
                        .any(|widget| widget.is::<gtk4::Switch>() || widget.is::<gtk4::TextView>())
                );
                for widget in descendants(&page) {
                    if let Some(expander) = widget.downcast_ref::<gtk4::Expander>() {
                        expander.set_expanded(true);
                    }
                }
                named::<gtk4::Entry>(&root, "lm-name").set_text("Cleanup fallback");
                named::<gtk4::Entry>(&root, "lm-ollama-model").set_text("cleanup-smoke");
                named::<gtk4::DropDown>(&root, "lm-ollama-thinking").set_selected(0);
                button(&root, "action.add").emit_clicked();
                assert_eq!(state.store.load_settings().language_models.entries.len(), 2);
                assert_eq!(
                    state
                        .store
                        .load_settings()
                        .language_models
                        .entries
                        .iter()
                        .find(|entry| entry.model == "cleanup-smoke")
                        .unwrap()
                        .thinking,
                    None
                );
                button(&root, "action.cleanupsettings").emit_clicked();
            }
            25 => {
                let list: gtk4::Box = named(&root, "cleanup-models");
                let switches: Vec<_> = descendants(list.upcast_ref())
                    .into_iter()
                    .filter_map(|widget| widget.downcast::<gtk4::Switch>().ok())
                    .collect();
                assert_eq!(switches.len(), 2);
                switches[0].set_active(false);
                let down = descendants(list.upcast_ref())
                    .into_iter()
                    .find_map(|widget| {
                        widget.downcast::<gtk4::Button>().ok().filter(|button| {
                            button.label().as_deref() == Some("↓") && button.is_sensitive()
                        })
                    })
                    .unwrap();
                down.emit_clicked();
                let saved = state.store.load_settings();
                assert_eq!(saved.language_models.entries[0].model, "cleanup-smoke");
                assert_eq!(saved.language_models.chain().len(), 1);
                assert!(!saved.language_models.entries[1].enabled);
                button(&root, "action.managelm").emit_clicked();
            }
            26 => {
                button(&root, "action.edit").emit_clicked();
                assert_eq!(
                    named::<gtk4::Entry>(&root, "lm-name").text(),
                    "Cleanup fallback"
                );
                named::<gtk4::Entry>(&root, "lm-ollama-model").set_text("cleanup-smoke-edited");
                named::<gtk4::DropDown>(&root, "lm-ollama-thinking").set_selected(1);
                button(&root, "action.save").emit_clicked();
                assert_eq!(
                    state
                        .store
                        .load_settings()
                        .language_models
                        .entries
                        .iter()
                        .find(|entry| entry.model == "cleanup-smoke-edited")
                        .unwrap()
                        .thinking,
                    Some(true)
                );
                button(&root, "action.cleanupsettings").emit_clicked();
            }
            27 => {
                let list: gtk4::Box = named(&root, "cleanup-models");
                assert!(descendants(list.upcast_ref()).iter().any(|widget| {
                    widget
                        .downcast_ref::<gtk4::Label>()
                        .is_some_and(|label| label.text().contains("cleanup-smoke-edited"))
                }));
                let saved = state.store.load_settings();
                assert_eq!(
                    saved.language_models.chain()[0].model,
                    "cleanup-smoke-edited"
                );
                assert!(!saved.language_models.entries[1].enabled);
                button(&root, "action.managelm").emit_clicked();
            }
            28 => {
                button(&root, "action.edit").emit_clicked();
                named::<gtk4::DropDown>(&root, "lm-ollama-thinking").set_selected(2);
                button(&root, "action.save").emit_clicked();
                assert_eq!(
                    state
                        .store
                        .load_settings()
                        .language_models
                        .entries
                        .iter()
                        .find(|entry| entry.model == "cleanup-smoke-edited")
                        .unwrap()
                        .thinking,
                    Some(false)
                );
                button(&root, "action.remove").emit_clicked();
            }
            29 => {
                respond(&window, "confirm");
                button(&root, "action.cleanupsettings").emit_clicked();
            }
            30 => {
                let list: gtk4::Box = named(&root, "cleanup-models");
                assert_eq!(
                    descendants(list.upcast_ref())
                        .iter()
                        .filter(|widget| widget.is::<gtk4::Switch>())
                        .count(),
                    1
                );
                assert!(
                    state
                        .store
                        .load_settings()
                        .language_models
                        .chain()
                        .is_empty()
                );
                named::<gtk4::Stack>(&root, "settings-pages")
                    .set_visible_child_name("Privacy & Data");
            }
            31 => button(&root, "action.clearalltranscripts").emit_clicked(),
            32 => {
                respond(&window, "cancel");
                assert_eq!(
                    state.store.load_history(200, true).unwrap().len(),
                    original_history
                );
            }
            33 => button(&root, "action.clearalltranscripts").emit_clicked(),
            34 => {
                respond(&window, "confirm");
                assert!(state.store.load_history(200, true).unwrap().is_empty());
                named::<gtk4::Stack>(&root, "settings-pages").set_visible_child_name("Preferences");
            }
            35 => {
                let theme: gtk4::DropDown = named(&root, "theme-picker");
                theme.set_selected(2);
                assert_eq!(
                    state.store.load_settings().preferences.theme,
                    mywhisper::settings::Theme::Dark
                );
                theme.set_selected(1);
                assert_eq!(
                    state.store.load_settings().preferences.theme,
                    mywhisper::settings::Theme::Light
                );
                let language: gtk4::DropDown = named(&root, "language-picker");
                language.set_selected(if mywhisper::i18n::lang() == mywhisper::i18n::UiLang::Es {
                    1
                } else {
                    2
                });
            }
            36 => {
                let fresh = app.active_window().unwrap();
                assert_ne!(fresh, window);
                assert_eq!(
                    named::<gtk4::Stack>(fresh.upcast_ref(), "main-pages")
                        .visible_child_name()
                        .as_deref(),
                    Some("settings")
                );
                assert_eq!(
                    named::<gtk4::Stack>(fresh.upcast_ref(), "settings-pages")
                        .visible_child_name()
                        .as_deref(),
                    Some("Preferences")
                );
                println!(
                    "Native UI smoke passed: validation, draft retention, delete cancel/confirm, multiline snippet creation/editing, alias conflicts, search, deep links, separate model/cleanup settings, cleanup persistence, prompt draft cancel/close, explicit save, summary refresh, default restoration, model add/edit/remove, priority and selection across pages, bulk history clear, theme persistence, language rebuild."
                );
                app.quit();
                return gtk4::glib::ControlFlow::Break;
            }
            _ => unreachable!(),
        }
        step += 1;
        gtk4::glib::ControlFlow::Continue
    });
}

/// Render the actual libadwaita toast, including its close hover. Assertions
/// compare computed colors and rendered pixels, not stylesheet text.
pub fn toast_proof(app: &gtk4::Application, window: &gtk4::ApplicationWindow, dark: bool) {
    let app = app.clone();
    let window = window.clone();
    let directory = tempfile::Builder::new()
        .prefix("mywhisper-toast-proof-")
        .tempdir()
        .unwrap()
        .keep();
    gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
        let feedback = descendants(window.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<libadwaita::ToastOverlay>().ok())
            .unwrap();
        let toast = libadwaita::Toast::new(tr("feedback.deleted"));
        toast.set_timeout(0);
        feedback.add_toast(toast);
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(350), move || {
            let toast = descendants(window.upcast_ref())
                .into_iter()
                .find(|widget| widget.css_name() == "toast")
                .unwrap();
            let close = descendants(&toast)
                .into_iter()
                .find_map(|widget| {
                    widget.downcast::<gtk4::Button>().ok().filter(|button| {
                        button.icon_name().as_deref() == Some("window-close-symbolic")
                    })
                })
                .unwrap();
            let expected =
                gtk4::gdk::RGBA::parse(if dark { "#edf0f5" } else { "#202631" }).unwrap();
            assert_eq!(close.color(), expected);
            let label = descendants(&toast)
                .into_iter()
                .find_map(|widget| widget.downcast::<gtk4::Label>().ok())
                .unwrap();
            assert_eq!(label.color(), expected);
            assert!(
                close.width() <= 36 && close.height() <= 36,
                "toast close must retain compact dimensions"
            );
            let normal = render_widget(&window, &toast);
            normal.save_to_png(directory.join("normal.png")).unwrap();
            close.set_state_flags(gtk4::StateFlags::PRELIGHT, false);
            gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                let hover = render_widget(&window, &toast);
                hover.save_to_png(directory.join("hover.png")).unwrap();
                let normal_pixel = texture_pixel(&normal, 24, 12);
                let hover_pixel = texture_pixel(&hover, 24, 12);
                assert_eq!(
                    normal_pixel, hover_pixel,
                    "hover must not invert the toast surface"
                );
                let card =
                    gtk4::gdk::RGBA::parse(if dark { "#242830" } else { "#ffffff" }).unwrap();
                let expected = [card.red(), card.green(), card.blue()]
                    .map(|value| (value * 255.0).round() as u8);
                for index in 0..3 {
                    assert!(
                        normal_pixel[index].abs_diff(expected[index]) <= 2,
                        "toast background must match the active card palette"
                    );
                }
                assert_eq!(close.color(), label.color());
                assert_ne!(
                    normal.save_to_png_bytes(),
                    hover.save_to_png_bytes(),
                    "close hover must still provide feedback"
                );
                close.emit_clicked();
                println!(
                    "Native toast palette/close-hover/dismiss regression passed; visual evidence: {}",
                    directory.display()
                );
                app.quit();
            });
        });
    });
}
fn render_widget(window: &gtk4::ApplicationWindow, widget: &gtk4::Widget) -> gtk4::gdk::Texture {
    let snapshot = gtk4::Snapshot::new();
    let paintable = gtk4::WidgetPaintable::new(Some(widget));
    paintable.snapshot(
        &snapshot,
        f64::from(widget.width()),
        f64::from(widget.height()),
    );
    let node = snapshot.to_node().unwrap();
    window.renderer().unwrap().render_texture(
        &node,
        Some(&gtk4::graphene::Rect::new(
            0.0,
            0.0,
            widget.width() as f32,
            widget.height() as f32,
        )),
    )
}
fn texture_pixel(texture: &gtk4::gdk::Texture, x: usize, y: usize) -> [u8; 4] {
    let mut downloader = gtk4::gdk::TextureDownloader::new(texture);
    downloader.set_format(gtk4::gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    bytes[y * stride + x * 4..y * stride + x * 4 + 4]
        .try_into()
        .unwrap()
}

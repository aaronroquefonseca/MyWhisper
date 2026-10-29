use gtk4::prelude::*;
use mywhisper::app::AppState;
fn find_card(w: &gtk4::Widget) -> Option<gtk4::Widget> {
    if w.has_css_class("card")
        && w.first_child()
            .and_then(|c| c.downcast::<gtk4::Label>().ok())
            .is_some_and(|l| l.text() == "audit word")
    {
        return Some(w.clone());
    }
    let mut ch = w.first_child();
    while let Some(c) = ch {
        ch = c.next_sibling();
        if let Some(hit) = find_card(&c) {
            return Some(hit);
        }
    }
    None
}
fn main() {
    gtk4::init().unwrap();
    let state = AppState::fresh();
    state.settings.borrow_mut().sync.enabled = false;
    match std::env::args().nth(1).as_deref() {
        Some("mdns") => {
            let node = "界".repeat(20);
            state.discovered.lock().unwrap().insert(
                node.clone(),
                mywhisper::net::Discovered {
                    node_id: node,
                    name: "unauthenticated discovery".into(),
                    addresses: vec!["127.0.0.1:1".into()],
                },
            );
            let _page = mywhisper::app_sync::page(&state);
        }
        Some("lang") => {
            let mut settings = state.settings.borrow_mut();
            settings.stt.local_provider = mywhisper::settings::LocalSttProvider::Nvidia;
            settings.stt.parakeet_model = "nemotron-speech-streaming-en-0.6b".into();
            settings.stt.language = "es".into();
            settings.stt.entries = vec![mywhisper::settings::SttChainEntry {
                id: "remote".into(),
                name: "Spanish remote".into(),
                kind: mywhisper::settings::SttEntryKind::SelfHosted,
                enabled: true,
                provider: "".into(),
                url: "http://127.0.0.1:1".into(),
                model: "test".into(),
                api_key: "".into(),
                streaming: false,
            }];
            drop(settings);
            let _page = mywhisper::app_settings::page(&state, "Speech to Text");
            assert_eq!(state.settings.borrow().stt.language, "en");
            println!(
                "REPRODUCED opening settings overwrites remote Spanish language with local model English"
            );
        }
        _ => {
            state.store.upsert_word("audit word", "audit").unwrap();
            let page = mywhisper::app_dictionary::page(&state);
            let card = find_card(page.upcast_ref()).unwrap();
            let weak = card.downgrade();
            drop(card);
            drop(page);
            let ctx = gtk4::glib::MainContext::default();
            let until = std::time::Instant::now() + std::time::Duration::from_millis(1200);
            while std::time::Instant::now() < until {
                while ctx.pending() {
                    ctx.iteration(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(weak.upgrade().is_some());
            println!(
                "REPRODUCED dictionary card survives page removal due to strong widget/signal cycle"
            );
        }
    }
}

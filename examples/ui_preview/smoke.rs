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
        if snippets {
            if let Some(notebook) = widget.downcast_ref::<gtk4::Notebook>() {
                notebook.set_current_page(Some(1));
            }
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

pub fn run(app: &gtk4::Application, state: &Rc<AppState>, window: &gtk4::ApplicationWindow) {
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
            3 | 4 | 5 => {}
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
                let page = named::<gtk4::Stack>(&root, "settings-pages")
                    .visible_child()
                    .unwrap();
                for widget in descendants(&page) {
                    if let Some(expander) = widget.downcast_ref::<gtk4::Expander>() {
                        expander.set_expanded(true);
                    }
                }
                named::<gtk4::TextView>(&root, "cleanup-prompt")
                    .buffer()
                    .set_text("Fix punctuation only.");
                assert_eq!(
                    state.store.load_settings().language_models.cleanup_prompt,
                    "Fix punctuation only."
                );
                button(&root, "action.expandeditor").emit_clicked();
            }
            17 => {
                let editor = app
                    .windows()
                    .into_iter()
                    .find(|window| window.widget_name() == "cleanup-prompt-editor")
                    .unwrap();
                let text: gtk4::TextView = named(editor.upcast_ref(), "expanded-cleanup-prompt");
                assert_eq!(
                    text.buffer(),
                    named::<gtk4::TextView>(&root, "cleanup-prompt").buffer()
                );
                assert!(editor.is_resizable());
                let long = "Keep the speaker’s meaning. Fix punctuation.\n".repeat(100);
                text.buffer().set_text(&long);
                assert_eq!(
                    state.store.load_settings().language_models.cleanup_prompt,
                    long
                );
                let restore = descendants(editor.upcast_ref())
                    .into_iter()
                    .find_map(|widget| {
                        widget.downcast::<gtk4::Button>().ok().filter(|button| {
                            button.label().as_deref() == Some(tr("action.restore"))
                        })
                    })
                    .unwrap();
                restore.emit_clicked();
                assert_eq!(
                    state.store.load_settings().language_models.cleanup_prompt,
                    mywhisper::transcribe::DEFAULT_CLEANUP_PROMPT
                );
                text.buffer().set_text(&long);
                named::<gtk4::Button>(editor.upcast_ref(), "prompt-editor-done").emit_clicked();
            }
            18 => {
                assert!(
                    !app.windows()
                        .iter()
                        .any(|window| window.widget_name() == "cleanup-prompt-editor")
                );
                let text: gtk4::TextView = named(&root, "cleanup-prompt");
                let buffer = text.buffer();
                assert_eq!(
                    buffer.text(&buffer.start_iter(), &buffer.end_iter(), false),
                    "Keep the speaker’s meaning. Fix punctuation.\n".repeat(100)
                );
                assert!(buffer.char_count() > 4000);
            }
            19 => {
                button(&root, "action.restore").emit_clicked();
                assert_eq!(
                    state.store.load_settings().language_models.cleanup_prompt,
                    mywhisper::transcribe::DEFAULT_CLEANUP_PROMPT
                );
                named::<gtk4::Switch>(&root, "cleanup-enabled").set_active(true);
                assert!(state.store.load_settings().language_models.cleanup_enabled);
                button(&root, "action.managelm").emit_clicked();
            }
            20 => {
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
                button(&root, "action.add").emit_clicked();
                assert_eq!(state.store.load_settings().language_models.entries.len(), 2);
                button(&root, "action.cleanupsettings").emit_clicked();
            }
            21 => {
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
            22 => {
                button(&root, "action.edit").emit_clicked();
                assert_eq!(
                    named::<gtk4::Entry>(&root, "lm-name").text(),
                    "Cleanup fallback"
                );
                named::<gtk4::Entry>(&root, "lm-ollama-model").set_text("cleanup-smoke-edited");
                button(&root, "action.save").emit_clicked();
                button(&root, "action.cleanupsettings").emit_clicked();
            }
            23 => {
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
            24 => button(&root, "action.remove").emit_clicked(),
            25 => {
                respond(&window, "confirm");
                button(&root, "action.cleanupsettings").emit_clicked();
            }
            26 => {
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
            27 => button(&root, "action.clearalltranscripts").emit_clicked(),
            28 => {
                respond(&window, "cancel");
                assert_eq!(
                    state.store.load_history(200, true).unwrap().len(),
                    original_history
                );
            }
            29 => button(&root, "action.clearalltranscripts").emit_clicked(),
            30 => {
                respond(&window, "confirm");
                assert!(state.store.load_history(200, true).unwrap().is_empty());
                named::<gtk4::Stack>(&root, "settings-pages").set_visible_child_name("Preferences");
            }
            31 => {
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
            32 => {
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
                    "Native UI smoke passed: validation, draft retention, delete cancel/confirm, multiline snippet creation/editing, alias conflicts, search, deep links, separate model/cleanup settings, cleanup persistence, expanded prompt editing and restoration, model add/edit/remove, priority and selection across pages, bulk history clear, theme persistence, language rebuild."
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

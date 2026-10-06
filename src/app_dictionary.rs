//! Dictionary page: words + snippets tabs, fully wired to the store.
//!
//! Adds/deletes write versioned records (sync-ready). Every entry
//! gets its own card. Lists refresh from a generation counter, so
//! rows arriving from a sync round appear without touching anything.

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Button, Entry, Label, Notebook, Orientation, TextView};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use super::app::AppState;

fn clear_box(container: &GtkBox) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn delete_button() -> Button {
    let b = Button::with_label("Delete");
    b.set_valign(gtk4::Align::Center);
    b
}

fn empty_card(text: &str) -> GtkBox {
    let card = GtkBox::new(Orientation::Vertical, 8);
    card.add_css_class("card");
    let l = Label::new(Some(text));
    l.add_css_class("dim");
    l.set_xalign(0.0);
    card.append(&l);
    card
}

fn edit_button() -> Button {
    let b = Button::with_label("Edit");
    b.set_valign(gtk4::Align::Center);
    b
}

fn save_button() -> Button {
    let b = Button::with_label("Save");
    b.set_valign(gtk4::Align::Center);
    b
}

fn cancel_button() -> Button {
    let b = Button::with_label("Cancel");
    b.set_valign(gtk4::Align::Center);
    b
}

/// Rebuild one word card in place. `editing` swaps the label for an
/// entry with Save/Cancel.
fn word_card(state: &Rc<AppState>, container: &GtkBox, word: &str, editing: bool) {
    clear_box(container);
    if !editing {
        let l = Label::new(Some(word));
        l.set_xalign(0.0);
        l.set_hexpand(true);
        container.append(&l);
        let edit = edit_button();
        {
            let state = state.clone();
            let container = container.clone();
            let word = word.to_string();
            edit.connect_clicked(move |_| {
                word_card(&state, &container, &word, true);
            });
        }
        container.append(&edit);
        let del = delete_button();
        {
            let state = state.clone();
            let word = word.to_string();
            del.connect_clicked(move |_| {
                if state.store.delete_word(&word, &state.node_id).is_err() {
                    return;
                }
                state
                    .settings
                    .borrow_mut()
                    .dictionary
                    .words
                    .retain(|x| x != &word);
                crate::net::sync_after_change(state.db_path.clone(), state.creds.clone());
                eprintln!("[app] dictionary: deleted word {word:?}");
            });
        }
        container.append(&del);
    } else {
        let entry = Entry::new();
        entry.set_text(word);
        entry.set_hexpand(true);
        container.append(&entry);
        let save = save_button();
        {
            let state = state.clone();
            let container = container.clone();
            let entry = entry.clone();
            let old = word.to_string();
            save.connect_clicked(move |_| {
                let new = entry.text().trim().to_string();
                if new.is_empty() || new == old {
                    word_card(&state, &container, &old, false);
                    return;
                }
                let _ = state.store.delete_word(&old, &state.node_id);
                if state.store.upsert_word(&new, &state.node_id).is_err() {
                    return;
                }
                {
                    let mut settings = state.settings.borrow_mut();
                    settings.dictionary.words.retain(|x| x != &old);
                    if !settings.dictionary.words.iter().any(|x| x == &new) {
                        settings.dictionary.words.push(new.clone());
                    }
                }
                crate::net::sync_after_change(state.db_path.clone(), state.creds.clone());
                eprintln!("[app] dictionary: renamed word {old:?} -> {new:?}");
            });
        }
        container.append(&save);
        let cancel = cancel_button();
        {
            let state = state.clone();
            let container = container.clone();
            let old = word.to_string();
            cancel.connect_clicked(move |_| {
                word_card(&state, &container, &old, false);
            });
        }
        container.append(&cancel);
    }
}

fn fill_words(container: &GtkBox, state: &Rc<AppState>) {
    clear_box(container);
    let words = state.store.load_words().unwrap_or_default();
    if words.is_empty() {
        container.append(&empty_card(
            "No words yet. Added words are always recognized.",
        ));
        return;
    }
    for w in words {
        let card = GtkBox::new(Orientation::Horizontal, 8);
        card.add_css_class("card");
        word_card(state, &card, &w, false);
        container.append(&card);
    }
}

fn words_tab(state: &Rc<AppState>) -> (GtkBox, GtkBox) {
    let root = GtkBox::new(Orientation::Vertical, 12);

    let add_row = GtkBox::new(Orientation::Horizontal, 8);
    let entry = Entry::new();
    entry.set_placeholder_text(Some("Add a word..."));
    entry.set_hexpand(true);
    let add = Button::with_label("Add");
    add_row.append(&entry);
    add_row.append(&add);
    root.append(&add_row);

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    root.append(&scrolled);
    let list = GtkBox::new(Orientation::Vertical, 8);
    scrolled.set_child(Some(&list));
    fill_words(&list, state);

    {
        let state = state.clone();
        let list = list.clone();
        let input = entry.clone();
        let commit = Rc::new(move || {
            let w = input.text().trim().to_string();
            if w.is_empty() {
                return;
            }
            if state.store.upsert_word(&w, &state.node_id).is_err() {
                return;
            }
            {
                let mut settings = state.settings.borrow_mut();
                if !settings.dictionary.words.iter().any(|x| x == &w) {
                    settings.dictionary.words.push(w.clone());
                }
            }
            input.set_text("");
            fill_words(&list, &state);
            crate::net::sync_after_change(state.db_path.clone(), state.creds.clone());
            eprintln!("[app] dictionary: added word {w:?}");
        });
        let commit_enter = commit.clone();
        entry.connect_activate(move |_| commit_enter());
        add.connect_clicked(move |_| commit());
    }

    (root, list)
}

fn snippet_text(view: &TextView) -> String {
    let buf = view.buffer();
    buf.text(&buf.start_iter(), &buf.end_iter(), false)
        .to_string()
}

/// Rebuild one snippet card in place. `editing` swaps labels for
/// entries with Save/Cancel.
fn snippet_card(
    state: &Rc<AppState>,
    container: &GtkBox,
    trigger_text: &str,
    replacement_text: &str,
    editing: bool,
) {
    clear_box(container);
    if !editing {
        let top = GtkBox::new(Orientation::Horizontal, 8);
        let trigger = Label::new(Some(trigger_text));
        trigger.add_css_class("row-label");
        trigger.set_xalign(0.0);
        trigger.set_hexpand(true);
        top.append(&trigger);
        let edit = edit_button();
        {
            let state = state.clone();
            let container = container.clone();
            let trigger_text = trigger_text.to_string();
            let replacement_text = replacement_text.to_string();
            edit.connect_clicked(move |_| {
                snippet_card(&state, &container, &trigger_text, &replacement_text, true);
            });
        }
        top.append(&edit);
        let del = delete_button();
        {
            let state = state.clone();
            let trigger_text = trigger_text.to_string();
            del.connect_clicked(move |_| {
                if state
                    .store
                    .delete_snippet(&trigger_text, &state.node_id)
                    .is_err()
                {
                    return;
                }
                state
                    .settings
                    .borrow_mut()
                    .dictionary
                    .snippets
                    .retain(|x| x.trigger != trigger_text);
                crate::net::sync_after_change(state.db_path.clone(), state.creds.clone());
                eprintln!("[app] dictionary: deleted snippet {trigger_text:?}");
            });
        }
        top.append(&del);
        container.append(&top);
        let body = Label::new(Some(replacement_text));
        body.set_xalign(0.0);
        body.set_wrap(true);
        body.add_css_class("dim");
        container.append(&body);
    } else {
        let trigger_entry = Entry::new();
        trigger_entry.set_text(trigger_text);
        trigger_entry.set_hexpand(true);
        container.append(&trigger_entry);
        let replacement_entry = Entry::new();
        replacement_entry.set_text(replacement_text);
        replacement_entry.set_hexpand(true);
        container.append(&replacement_entry);
        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.set_halign(gtk4::Align::End);
        let save = save_button();
        {
            let state = state.clone();
            let trigger_entry = trigger_entry.clone();
            let replacement_entry = replacement_entry.clone();
            let old = trigger_text.to_string();
            save.connect_clicked(move |_| {
                let t = trigger_entry.text().trim().to_string();
                let r = replacement_entry.text().trim().to_string();
                if t.is_empty() || r.is_empty() {
                    return;
                }
                if t != old {
                    let _ = state.store.delete_snippet(&old, &state.node_id);
                }
                if state.store.upsert_snippet(&t, &r, &state.node_id).is_err() {
                    return;
                }
                {
                    let mut settings = state.settings.borrow_mut();
                    settings.dictionary.snippets.retain(|x| x.trigger != old);
                    if let Some(existing) = settings
                        .dictionary
                        .snippets
                        .iter_mut()
                        .find(|x| x.trigger == t)
                    {
                        existing.replacement = r.clone();
                    } else {
                        settings.dictionary.snippets.push(crate::settings::Snippet {
                            trigger: t.clone(),
                            replacement: r.clone(),
                        });
                    }
                }
                crate::net::sync_after_change(state.db_path.clone(), state.creds.clone());
                eprintln!("[app] dictionary: edited snippet {old:?} -> {t:?}");
            });
        }
        row.append(&save);
        let cancel = cancel_button();
        {
            let state = state.clone();
            let container = container.clone();
            let trigger_text = trigger_text.to_string();
            let replacement_text = replacement_text.to_string();
            cancel.connect_clicked(move |_| {
                snippet_card(&state, &container, &trigger_text, &replacement_text, false);
            });
        }
        row.append(&cancel);
        container.append(&row);
    }
}

fn fill_snippets(container: &GtkBox, state: &Rc<AppState>) {
    clear_box(container);
    let snippets = state.store.load_snippets().unwrap_or_default();
    if snippets.is_empty() {
        container.append(&empty_card(
            "No snippets yet. Type a trigger anywhere to expand it.",
        ));
        return;
    }
    for s in snippets {
        let card = GtkBox::new(Orientation::Vertical, 4);
        card.add_css_class("card");
        snippet_card(state, &card, &s.trigger, &s.replacement, false);
        container.append(&card);
    }
}

fn snippets_tab(state: &Rc<AppState>) -> (GtkBox, GtkBox) {
    let root = GtkBox::new(Orientation::Vertical, 12);

    let trigger = Entry::new();
    trigger.set_placeholder_text(Some("Trigger, e.g. omw"));
    trigger.set_hexpand(true);
    root.append(&trigger);

    let expansion = TextView::new();
    expansion.set_size_request(-1, 80);
    root.append(&expansion);

    let create = Button::with_label("Create snippet");
    create.set_halign(gtk4::Align::End);
    root.append(&create);

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    root.append(&scrolled);
    let list = GtkBox::new(Orientation::Vertical, 8);
    scrolled.set_child(Some(&list));
    fill_snippets(&list, state);

    {
        let state = state.clone();
        let list = list.clone();
        create.connect_clicked(move |_| {
            let t = trigger.text().trim().to_string();
            let r = snippet_text(&expansion).trim().to_string();
            if t.is_empty() || r.is_empty() {
                return;
            }
            if state.store.upsert_snippet(&t, &r, &state.node_id).is_err() {
                return;
            }
            {
                let mut settings = state.settings.borrow_mut();
                if let Some(existing) = settings
                    .dictionary
                    .snippets
                    .iter_mut()
                    .find(|x| x.trigger == t)
                {
                    existing.replacement = r.clone();
                } else {
                    settings.dictionary.snippets.push(crate::settings::Snippet {
                        trigger: t.clone(),
                        replacement: r.clone(),
                    });
                }
            }
            trigger.set_text("");
            expansion.buffer().set_text("");
            fill_snippets(&list, &state);
            crate::net::sync_after_change(state.db_path.clone(), state.creds.clone());
            eprintln!("[app] dictionary: added snippet {t:?}");
        });
    }

    (root, list)
}

pub fn page(state: &Rc<AppState>) -> GtkBox {
    let root = GtkBox::new(Orientation::Vertical, 16);
    root.add_css_class("page");

    let col = super::app::centered_column(&root, 880);

    let title = Label::new(Some("Dictionary"));
    title.add_css_class("page-title");
    title.set_xalign(0.0);
    col.append(&title);

    let tabs = Notebook::new();
    let (words_root, words_list) = words_tab(state);
    let (snippets_root, snippets_list) = snippets_tab(state);
    tabs.append_page(&words_root, Some(&Label::new(Some("Dictionary"))));
    tabs.append_page(&snippets_root, Some(&Label::new(Some("Snippets"))));
    tabs.set_vexpand(true);
    col.append(&tabs);

    // Live refresh: the merge engine bumps the generation on every
    // mutation (local CRUD and incoming syncs alike). When it moves,
    // repaint both lists and re-sync the settings mirror so a future
    // settings save persists the same content.
    {
        let state = state.clone();
        let seen = Rc::new(Cell::new(crate::store::data_gen()));
        gtk4::glib::timeout_add_local(Duration::from_millis(1000), move || {
            if seen.get() != crate::store::data_gen() {
                seen.set(crate::store::data_gen());
                {
                    let mut settings = state.settings.borrow_mut();
                    settings.dictionary.words = state.store.load_words().unwrap_or_default();
                    settings.dictionary.snippets = state.store.load_snippets().unwrap_or_default();
                }
                fill_words(&words_list, &state);
                fill_snippets(&snippets_list, &state);
            }
            gtk4::glib::ControlFlow::Continue
        });
    }

    root
}

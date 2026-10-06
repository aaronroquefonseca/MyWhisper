//! Dictionary page: words + snippets tabs.
//!
//! Model: `AppSettings.dictionary` (words + snippets). Add/remove
//! controls exist but are NOT wired yet -- the lists render from the
//! model as-is.

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Button, Entry, Label, ListBox, Notebook, Orientation, TextView};
use std::rc::Rc;

use super::app::AppState;

fn words_tab(state: &Rc<AppState>) -> GtkBox {
    let root = GtkBox::new(Orientation::Vertical, 12);

    let add_row = GtkBox::new(Orientation::Horizontal, 8);
    let entry = Entry::new();
    entry.set_placeholder_text(Some("Add a word..."));
    entry.set_hexpand(true);
    let add = Button::with_label("Add");
    add.set_sensitive(false); // not wired yet
    add_row.append(&entry);
    add_row.append(&add);
    root.append(&add_row);

    let list = ListBox::new();
    list.add_css_class("card");
    let dict = state.settings.borrow();
    if dict.dictionary.words.is_empty() {
        let l = Label::new(Some("No words yet. Added words are always recognized."));
        l.add_css_class("dim");
        list.append(&l);
    }
    for w in &dict.dictionary.words {
        let l = Label::new(Some(w.as_str()));
        l.set_xalign(0.0);
        list.append(&l);
    }
    root.append(&list);
    root
}

fn snippets_tab(state: &Rc<AppState>) -> GtkBox {
    let root = GtkBox::new(Orientation::Vertical, 12);

    let trigger = Entry::new();
    trigger.set_placeholder_text(Some("Trigger, e.g. omw"));
    trigger.set_hexpand(true);
    root.append(&trigger);

    let expansion = TextView::new();
    expansion.set_size_request(-1, 80);
    root.append(&expansion);

    let add = Button::with_label("Create snippet");
    add.set_halign(gtk4::Align::End);
    add.set_sensitive(false); // not wired yet
    root.append(&add);

    let list = ListBox::new();
    list.add_css_class("card");
    let dict = state.settings.borrow();
    if dict.dictionary.snippets.is_empty() {
        let l = Label::new(Some(
            "No snippets yet. Type a trigger anywhere to expand it.",
        ));
        l.add_css_class("dim");
        list.append(&l);
    }
    for s in &dict.dictionary.snippets {
        let l = Label::new(Some(format!("{} → {}", s.trigger, s.replacement).as_str()));
        l.set_xalign(0.0);
        list.append(&l);
    }
    root.append(&list);
    root
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
    tabs.append_page(&words_tab(state), Some(&Label::new(Some("Dictionary"))));
    tabs.append_page(&snippets_tab(state), Some(&Label::new(Some("Snippets"))));
    tabs.set_vexpand(true);
    col.append(&tabs);

    root
}

//! Sync page: device identity, trusted peers, manual sync.
//!
//! Fully wired to the store: device name/port persist, peers add,
//! trust-toggle and remove, Sync-now runs the TCP exchange on a worker
//! thread and reports back. The listener itself starts in `main`.

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Button, Entry, Label, ListBox, Orientation, Switch};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::app::AppState;
use super::app_settings::persist_prefs;
use crate::i18n::{tr, trf};

std::thread_local! {
    static BUSY: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

fn valid_device_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}

fn valid_address(value: &str) -> bool {
    value.is_empty()
        || value.rsplit_once(':').is_some_and(|(host, port)| {
            !host.trim().is_empty()
                && !host.chars().any(char::is_whitespace)
                && port.parse::<u16>().is_ok_and(|port| port > 0)
        })
}

fn list_signature(state: &AppState) -> String {
    let mut discovered: Vec<_> = state.discovered.lock().unwrap().values().cloned().collect();
    discovered.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    let mut busy = BUSY.with(|busy| busy.borrow().iter().cloned().collect::<Vec<_>>());
    busy.sort();
    format!(
        "{:?}{:?}{:?}{:?}",
        state.store.list_peers(),
        state.store.list_pending(),
        discovered,
        busy
    )
}

fn short_id(node_id: &str) -> String {
    crate::device::short_id(node_id)
}

fn ago(epoch_secs: i64) -> String {
    if epoch_secs <= 0 {
        return tr("sync.never").to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(epoch_secs);
    let dt = now - epoch_secs;
    if dt < 60 {
        tr("sync.ago.now").to_string()
    } else if dt < 3600 {
        trf("sync.ago.min", &[("n", &(dt / 60).to_string())])
    } else if dt < 86_400 {
        trf("sync.ago.h", &[("n", &(dt / 3600).to_string())])
    } else {
        trf("sync.ago.d", &[("n", &(dt / 86_400).to_string())])
    }
}

fn persist_sync(state: &Rc<AppState>) {
    let settings = state.settings.borrow();
    if let Err(e) = state.store.save_settings(&settings) {
        eprintln!("[app] sync settings save failed: {e}");
        super::app::notify(tr("feedback.savefailed"));
    }
}

fn clear_list(list: &ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn fill_peers(list: &ListBox, state: &Rc<AppState>, status: &Label) {
    // Rows never own the list that owns them.
    let list = list.clone();
    let status = status.clone();
    clear_list(&list);
    let peers = state.store.list_peers().unwrap_or_default();
    if peers.is_empty() {
        let l = Label::new(Some(tr("sync.devices.empty")));
        l.set_wrap(true);
        l.set_max_width_chars(55);
        l.add_css_class("dim");
        list.append(&l);
        return;
    }
    for p in peers {
        let list_remove = list.downgrade();
        let card = GtkBox::new(Orientation::Vertical, 4);
        let top = GtkBox::new(Orientation::Horizontal, 8);
        let display_name = if p.name.is_empty() {
            short_id(&p.node_id)
        } else {
            p.name.clone()
        };
        let name = Label::new(Some(&display_name));
        name.add_css_class("row-label");
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_wrap(true);
        name.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        top.append(&name);

        let trust = Switch::new();
        trust.set_active(p.trusted);
        trust.set_valign(gtk4::Align::Center);
        trust.set_tooltip_text(Some(tr("sync.trusted")));
        trust.update_property(&[gtk4::accessible::Property::Label(tr("sync.trusted"))]);
        {
            let state = state.clone();
            let node_id = p.node_id.clone();
            let peer_name = p.name.clone();
            let address = p.address.clone();
            trust.connect_state_set(move |_, active| {
                if state
                    .store
                    .upsert_peer(&node_id, &peer_name, &address, active)
                    .is_err()
                {
                    super::app::notify(tr("feedback.savefailed"));
                    return gtk4::glib::Propagation::Stop;
                }
                eprintln!("[app] sync: trust {} -> {active}", short_id(&node_id));
                gtk4::glib::Propagation::Proceed
            });
        }
        top.append(&trust);

        let introduction_row = GtkBox::new(Orientation::Horizontal, 8);
        let explanation = Label::new(Some(tr("sync.introducer")));
        explanation.set_wrap(true);
        explanation.set_hexpand(true);
        explanation.set_xalign(0.0);
        introduction_row.append(&explanation);
        let introduce = Switch::new();
        introduce.set_active(p.introducer);
        introduce.set_sensitive(p.trusted);
        introduce.set_tooltip_text(Some(tr("sync.introducer.tip")));
        introduce.update_property(&[gtk4::accessible::Property::Label(tr("sync.introducer"))]);
        {
            let state = state.clone();
            let id = p.node_id.clone();
            introduce.connect_state_set(move |_, enabled| {
                if state.store.set_peer_introducer(&id, enabled).is_err() {
                    super::app::notify(tr("feedback.savefailed"));
                    return gtk4::glib::Propagation::Stop;
                }
                gtk4::glib::Propagation::Proceed
            });
        }
        {
            let weak = introduce.downgrade();
            trust.connect_active_notify(move |trust| {
                if let Some(introduce) = weak.upgrade() {
                    introduce.set_sensitive(trust.is_active());
                    if !trust.is_active() {
                        introduce.set_active(false);
                    }
                }
            });
        }
        introduction_row.append(&introduce);
        card.append(&introduction_row);

        let syncing = BUSY.with(|busy| busy.borrow().contains(&p.node_id));
        let sync_now = Button::with_label(if syncing {
            tr("sync.working")
        } else {
            tr("action.syncnow")
        });
        sync_now.set_sensitive(!syncing && p.trusted && !p.dial_targets().is_empty());
        {
            let state = state.clone();
            let status = status.clone();
            let peer = p.clone();
            sync_now.connect_clicked(move |_| {
                spawn_sync(&state, peer.clone(), &status);
            });
        }
        top.append(&sync_now);

        let remove = Button::with_label(tr("action.remove"));
        {
            let state = state.clone();
            let list = list_remove.clone();
            let status = status.clone();
            let node_id = p.node_id.clone();
            super::app::connect_confirmed(
                &remove,
                tr("sync.remove.title"),
                tr("sync.remove.body"),
                tr("action.remove"),
                move |_| {
                    let Some(list) = list.upgrade() else {
                        return;
                    };
                    if state.store.remove_peer(&node_id).is_err() {
                        super::app::notify(tr("feedback.deletefailed"));
                        return;
                    }
                    super::app::notify(tr("feedback.deleted"));
                    fill_peers(&list, &state, &status);
                    eprintln!("[app] sync: removed peer {}", short_id(&node_id));
                },
            );
        }
        top.append(&remove);
        card.append(&top);
        card.set_margin_top(8);
        card.set_margin_bottom(8);

        let meta = Label::new(Some(
            format!(
                "{} · {} · {}",
                short_id(&p.node_id),
                if p.address.is_empty() {
                    tr("sync.noaddr").to_string()
                } else {
                    p.address.clone()
                },
                trf("sync.lastsync", &[("ago", &ago(p.last_sync))])
            )
            .as_str(),
        ));
        meta.set_xalign(0.0);
        meta.set_wrap(true);
        meta.add_css_class("dim");
        card.append(&meta);
        list.append(&card);
    }
}

fn device_name(state: &Rc<AppState>) -> String {
    let settings = state.settings.borrow();
    crate::device::display_name(&settings.sync.device_name, &state.node_id)
}

/// Nearby LAN devices from mDNS that are not known yet. `refresh`
/// repaints the lists after an add (the 2s poll also covers it).
fn fill_nearby(list: &ListBox, state: &Rc<AppState>, status: &Label) {
    let list = list.clone();
    let status = status.clone();
    clear_list(&list);
    let mut found: Vec<crate::net::Discovered> = state
        .discovered
        .lock()
        .unwrap()
        .values()
        .filter(|d| d.node_id != state.node_id)
        .cloned()
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    // Live presence is the freshest naming/address source: refresh
    // known peers straight from discovery (renames propagate within
    // seconds, no sync round needed).
    if let Ok(known) = state.store.list_peers() {
        for d in &found {
            if let Some(peer) = known.iter().find(|p| p.node_id == d.node_id) {
                // Targeted name update only: never disturb trust,
                // pending or introducer flags.
                if !d.name.is_empty() && d.name != peer.name {
                    let _ = state.store.rename_peer(&d.node_id, &d.name);
                }
                let _ = state.store.set_peer_addresses(&d.node_id, &d.addresses);
            }
        }
    }
    let known: Vec<String> = state
        .store
        .list_peers()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.node_id)
        .collect();
    found.retain(|d| !known.contains(&d.node_id));
    if found.is_empty() {
        let l = Label::new(Some(tr("sync.nearby.empty")));
        l.set_wrap(true);
        l.set_max_width_chars(55);
        l.add_css_class("dim");
        list.append(&l);
        return;
    }
    for d in found {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        let l = Label::new(Some(
            format!(
                "{} · {}",
                if d.name.is_empty() {
                    short_id(&d.node_id)
                } else {
                    d.name.clone()
                },
                short_id(&d.node_id)
            )
            .as_str(),
        ));
        l.set_xalign(0.0);
        l.set_wrap(true);
        l.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        l.set_hexpand(true);
        row.append(&l);
        let add = Button::with_label(tr("action.sendrequest"));
        {
            let state = state.clone();
            let status = status.clone();
            add.connect_clicked(move |_| {
                if state
                    .store
                    .upsert_peer(&d.node_id, &d.name, "", true)
                    .and_then(|_| state.store.set_peer_addresses(&d.node_id, &d.addresses))
                    .is_err()
                {
                    super::app::notify(tr("sync.err.save"));
                    return;
                }
                if let Ok(Some(peer)) = state.store.get_peer(&d.node_id) {
                    spawn_sync(&state, peer, &status);
                }
                status
                    .set_text(trf("sync.request.sent", &[("id", &short_id(&d.node_id))]).as_str());
                eprintln!(
                    "[app] sync: pairing requested with {}",
                    short_id(&d.node_id)
                );
            });
        }
        row.append(&add);
        list.append(&row);
    }
}

/// Incoming pairing requests with accept/decline.
fn fill_requests(list: &ListBox, state: &Rc<AppState>, status: &Label) {
    let list = list.clone();
    let status = status.clone();
    clear_list(&list);
    let pending = state.store.list_pending().unwrap_or_default();
    if pending.is_empty() {
        let l = Label::new(Some(tr("sync.requests.empty")));
        l.set_wrap(true);
        l.set_max_width_chars(55);
        l.add_css_class("dim");
        list.append(&l);
        return;
    }
    for p in pending {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        let l = Label::new(Some(
            trf(
                "sync.requests.row",
                &[
                    (
                        "name",
                        &if p.name.is_empty() {
                            short_id(&p.node_id)
                        } else {
                            p.name.clone()
                        },
                    ),
                    ("id", &short_id(&p.node_id)),
                    ("seen", &ago(p.last_seen)),
                ],
            )
            .as_str(),
        ));
        l.set_xalign(0.0);
        l.set_wrap(true);
        l.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        l.set_hexpand(true);
        row.append(&l);
        let accept = Button::with_label(tr("action.accept"));
        accept.add_css_class("suggested-action");
        {
            let state = state.clone();
            let status = status.clone();
            let node_id = p.node_id.clone();
            accept.connect_clicked(move |_| {
                if state.store.decide_request(&node_id, true).is_err() {
                    status.set_text(tr("sync.err.accept"));
                    super::app::notify(tr("sync.err.accept"));
                    return;
                }
                status.set_text(tr("sync.accepted"));
                super::app::notify(tr("sync.accepted"));
                if let Ok(Some(peer)) = state.store.get_peer(&node_id) {
                    spawn_sync(&state, peer, &status);
                }
            });
        }
        row.append(&accept);
        let decline = Button::with_label(tr("action.decline"));
        {
            let state = state.clone();
            let status = status.clone();
            let node_id = p.node_id.clone();
            decline.connect_clicked(move |_| {
                if state.store.decide_request(&node_id, false).is_err() {
                    super::app::notify(tr("sync.err.save"));
                    return;
                }
                status.set_text(tr("sync.declined"));
                super::app::notify(tr("sync.declined"));
            });
        }
        row.append(&decline);
        list.append(&row);
    }
}

/// Run a sync round on a worker thread, then report + refresh.
/// `after` repaints whatever lists the caller owns (always runs, so
/// buttons re-enable even on early exits).
fn spawn_sync(state: &Rc<AppState>, peer: crate::store::Peer, status: &Label) {
    if !peer.trusted {
        status.set_text(tr("sync.err.untrusted"));
        return;
    }
    let targets = peer.dial_targets();
    if targets.is_empty() {
        status.set_text(tr("sync.err.noaddr"));
        return;
    }
    if !BUSY.with(|busy| busy.borrow_mut().insert(peer.node_id.clone())) {
        return;
    }
    let busy_id = peer.node_id.clone();
    status.set_text(trf("sync.syncing", &[("id", &short_id(&peer.node_id))]).as_str());
    // Plain data only across the thread boundary (AppState is !Send).
    let db_path = state.db_path.clone();
    let creds = state.creds.clone();
    let name = device_name(state);
    let expect = peer.node_id.clone();
    let result_tx: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let result_poll = result_tx.clone();
    std::thread::spawn(move || {
        let msg = match crate::net::sync_now(&targets, &db_path, &creds, &name, &expect) {
            Ok(r) => trf(
                "sync.synced",
                &[
                    ("id", &short_id(&r.peer_node)),
                    ("words", &r.words_in.to_string()),
                    ("snippets", &r.snippets_in.to_string()),
                    ("history", &r.history_in.to_string()),
                ],
            ),
            Err(e) => trf("sync.failed", &[("err", &e.to_string())]),
        };
        *result_tx.lock().unwrap() = Some(msg);
    });
    let status = status.clone();
    gtk4::glib::timeout_add_local(Duration::from_millis(300), move || {
        match result_poll.lock().unwrap().take() {
            Some(msg) => {
                status.set_text(&msg);
                BUSY.with(|busy| {
                    busy.borrow_mut().remove(&busy_id);
                });
                super::app::notify(&msg);
                gtk4::glib::ControlFlow::Break
            }
            None => gtk4::glib::ControlFlow::Continue,
        }
    });
}

/// Live listener port from the service heartbeat (0 = off /
/// service not running). The page never trusts a stale snapshot.
fn live_sync_port() -> u16 {
    let state_dir = crate::paths::AppPaths::create()
        .map(|p| p.state_dir)
        .unwrap_or_default();
    crate::service::read_heartbeat(&state_dir)
        .and_then(|hb| hb.get("sync_port")?.as_u64())
        .unwrap_or(0) as u16
}

fn listen_text(port: u16) -> String {
    if port == 0 {
        tr("sync.listener.off").to_string()
    } else {
        trf("sync.listener.on", &[("port", &port.to_string())])
    }
}

/// Self-reachability probe, no privileges: dial the live listener
/// through the LAN address (never 127.0.0.1) + the allow-command
/// hint for the installed firewall, if any.
fn probe_msg() -> String {
    let port = live_sync_port();
    if port == 0 {
        return tr("sync.reach.off").to_string();
    }
    // The dial only proves the listener answers (local addresses
    // leave via OUTPUT, which firewalls allow) — never a firewall
    // verdict. Firewall state is NEVER queried.
    if crate::net::self_reachable(port).is_none() {
        return trf("sync.reach.down", &[("port", &port.to_string())]);
    }
    let base = trf("sync.reach.up", &[("port", &port.to_string())]);
    match crate::net::firewall_allow_hint(port) {
        Some(cmd) => format!("{base}\n{}", trf("sync.reach.hint", &[("cmd", &cmd)])),
        None => base,
    }
}

pub fn page(state: &Rc<AppState>) -> GtkBox {
    // Devices use the same page layout and navigation as dictation.
    let root = GtkBox::new(Orientation::Vertical, 0);
    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_hexpand(true);
    // Pages scroll vertically while the sidebar stays visible.
    scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scrolled.set_propagate_natural_height(false);
    root.append(&scrolled);
    let inner = GtkBox::new(Orientation::Vertical, 16);
    inner.add_css_class("page");
    inner.set_margin_bottom(16);
    scrolled.set_child(Some(&inner));
    let col = super::app::centered_column(&inner, 860);
    super::app::page_heading(&col, tr("sync.title"), tr("sync.subtitle"));

    let status = Label::new(Some(""));
    status.add_css_class("dim");
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.set_visible(false);

    // ---- Sync engine ----
    // systemd owns the service process; this toggle only decides
    // whether it runs the sync engine. The service watcher applies
    // changes automatically.
    let svc_card = GtkBox::new(Orientation::Vertical, 8);
    svc_card.add_css_class("card");
    let t = Label::new(Some(tr("sync.svc.title")));
    t.add_css_class("section-title");
    t.set_xalign(0.0);
    svc_card.append(&t);
    let svc_row = GtkBox::new(Orientation::Horizontal, 8);
    let svc_label = Label::new(Some(tr("sync.svc.switch")));
    svc_label.set_xalign(0.0);
    svc_label.set_hexpand(true);
    svc_row.append(&svc_label);
    let svc_switch = Switch::new();
    svc_switch.update_property(&[gtk4::accessible::Property::Label(tr("sync.svc.switch"))]);
    svc_switch.set_active(state.settings.borrow().sync.enabled);
    svc_switch.set_valign(gtk4::Align::Center);
    {
        let state = state.clone();
        svc_switch.connect_state_set(move |_, active| {
            state.settings.borrow_mut().sync.enabled = active;
            persist_prefs(state.clone())();
            gtk4::glib::Propagation::Proceed
        });
    }
    svc_row.append(&svc_switch);
    svc_card.append(&svc_row);
    let adv_row = GtkBox::new(Orientation::Horizontal, 8);
    let adv_label = Label::new(Some(tr("sync.svc.advertise")));
    adv_label.set_xalign(0.0);
    adv_label.set_hexpand(true);
    adv_row.append(&adv_label);
    let adv_switch = Switch::new();
    adv_switch.update_property(&[gtk4::accessible::Property::Label(tr("sync.svc.advertise"))]);
    adv_switch.set_active(state.settings.borrow().sync.advertise);
    adv_switch.set_sensitive(state.settings.borrow().sync.enabled);
    {
        let adv_switch = adv_switch.clone();
        svc_switch
            .connect_active_notify(move |switch| adv_switch.set_sensitive(switch.is_active()));
    }
    adv_switch.set_valign(gtk4::Align::Center);
    {
        let state = state.clone();
        adv_switch.connect_state_set(move |_, active| {
            state.settings.borrow_mut().sync.advertise = active;
            persist_prefs(state.clone())();
            gtk4::glib::Propagation::Proceed
        });
    }
    adv_row.append(&adv_switch);
    svc_card.append(&adv_row);
    let svc_note = Label::new(Some(tr("sync.svc.note")));
    svc_note.add_css_class("dim");
    svc_note.set_xalign(0.0);
    svc_note.set_wrap(true);
    svc_card.append(&svc_note);
    col.append(&svc_card);

    // ---- This device ----
    let card = GtkBox::new(Orientation::Vertical, 8);
    card.add_css_class("card");
    let t = Label::new(Some(tr("sync.device.title")));
    t.add_css_class("section-title");
    t.set_xalign(0.0);
    card.append(&t);

    let name_row = GtkBox::new(Orientation::Horizontal, 8);
    let name_label = Label::new(Some(tr("sync.device.name")));
    name_label.add_css_class("row-label");
    name_label.set_xalign(0.0);
    name_label.set_size_request(170, -1);
    name_row.append(&name_label);
    let name_entry = Entry::new();
    name_entry.update_property(&[gtk4::accessible::Property::Label(tr("sync.device.name"))]);
    {
        let settings = state.settings.borrow();
        name_entry.set_text(&settings.sync.device_name);
    }
    name_entry.set_placeholder_text(Some(&crate::device::default_device_name(&state.node_id)));
    name_entry.set_hexpand(true);
    name_row.append(&name_entry);
    let name_save = Button::with_label(tr("action.save"));
    {
        let state = state.clone();
        name_save.connect_clicked(move |_| {
            let name = name_entry.text().trim().to_string();
            state.settings.borrow_mut().sync.device_name = name;
            persist_sync(&state);
            super::app::notify(tr("feedback.saved"));
            eprintln!("[app] sync: device name saved");
        });
    }
    name_row.append(&name_save);
    card.append(&name_row);

    let id_row = GtkBox::new(Orientation::Horizontal, 8);
    let id_label = Label::new(Some(tr("sync.device.id")));
    id_label.add_css_class("row-label");
    id_label.set_xalign(0.0);
    id_label.set_size_request(170, -1);
    id_row.append(&id_label);
    // Read-only identity field scrolls the full 64-character ID internally.
    let id_value = Entry::new();
    id_value.update_property(&[gtk4::accessible::Property::Label(tr("sync.device.id"))]);
    id_value.set_editable(false);
    id_value.set_can_focus(true);
    id_value.set_text(&state.node_id);
    id_value.set_width_chars(12);
    id_value.set_hexpand(true);
    id_row.append(&id_value);
    let copy = Button::with_label(tr("action.copy"));
    {
        let node_id = state.node_id.clone();
        copy.connect_clicked(move |_| {
            if let Some(display) = gtk4::gdk::Display::default() {
                display.clipboard().set_text(&node_id);
                super::app::notify(tr("feedback.copied"));
            }
        });
    }
    id_row.append(&copy);
    card.append(&id_row);

    let port_row = GtkBox::new(Orientation::Horizontal, 8);
    let port_label = Label::new(Some(tr("sync.device.port")));
    port_label.set_tooltip_text(Some(tr("sync.device.porthint")));
    port_label.add_css_class("row-label");
    port_label.set_xalign(0.0);
    port_label.set_size_request(170, -1);
    port_row.append(&port_label);
    let port_entry = Entry::new();
    port_entry.update_property(&[gtk4::accessible::Property::Label(tr("sync.device.port"))]);
    {
        let settings = state.settings.borrow();
        port_entry.set_text(&settings.sync.listen_port.to_string());
    }
    port_entry.set_hexpand(true);
    port_row.append(&port_entry);
    let port_save = Button::with_label(tr("action.save"));
    {
        let state = state.clone();
        port_save.connect_clicked(move |_| {
            if let Ok(port) = port_entry.text().trim().parse::<u16>() {
                state.settings.borrow_mut().sync.listen_port = port;
                persist_sync(&state);
                port_entry.remove_css_class("error");
                super::app::notify(tr("feedback.saved"));
                eprintln!("[app] sync: listen port saved ({port})");
            } else {
                port_entry.add_css_class("error");
                port_entry.grab_focus();
                super::app::notify(tr("sync.port.invalid"));
            }
        });
    }
    port_row.append(&port_save);
    card.append(&port_row);

    let listen = Label::new(Some(listen_text(live_sync_port()).as_str()));
    listen.set_widget_name("sync-listener-status");
    listen.add_css_class("dim");
    listen.set_xalign(0.0);
    card.append(&listen);
    // The service owns the listener: refresh its real port on a
    // timer while the service applies settings changes.
    {
        let listen = listen.downgrade();
        gtk4::glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
            let Some(listen) = listen.upgrade() else {
                return gtk4::glib::ControlFlow::Break;
            };
            listen.set_text(listen_text(live_sync_port()).as_str());
            gtk4::glib::ControlFlow::Continue
        });
    }

    // Self-reachability probe. Automatic (no button: nothing here
    // can pop polkit): on page build, then every 15 s. Off-GTK
    // thread (a filtered port can block up to the dial timeout).
    let reach_row = GtkBox::new(Orientation::Horizontal, 8);
    let reach = Label::new(Some(tr("sync.reach.unknown")));
    reach.set_widget_name("sync-reachability-status");
    reach.add_css_class("dim");
    reach.set_xalign(0.0);
    reach.set_wrap(true);
    reach.set_hexpand(true);
    reach_row.append(&reach);
    card.append(&reach_row);
    let run_probe: Rc<dyn Fn()> = Rc::new({
        let reach = reach.downgrade();
        let pending = Rc::new(std::cell::Cell::new(false));
        move || {
            if reach.upgrade().is_none() || pending.replace(true) {
                return;
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let task = match crate::runtime::spawn_session("sync-reachability", move || {
                let result = std::panic::catch_unwind(probe_msg)
                    .unwrap_or_else(|_| tr("sync.reach.unknown").into());
                let _ = tx.send(result);
            }) {
                Ok(task) => task,
                Err(_) => {
                    pending.set(false);
                    return;
                }
            };
            let reach = reach.clone();
            let pending = pending.clone();
            gtk4::glib::timeout_add_local(Duration::from_millis(300), move || {
                let _owned_task = &task;
                let Some(reach) = reach.upgrade() else {
                    pending.set(false);
                    return gtk4::glib::ControlFlow::Break;
                };
                match rx.try_recv() {
                    Ok(msg) => {
                        pending.set(false);
                        reach.set_text(&msg);
                        gtk4::glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => gtk4::glib::ControlFlow::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        pending.set(false);
                        gtk4::glib::ControlFlow::Break
                    }
                }
            });
        }
    });
    run_probe();
    {
        let run_probe = run_probe.clone();
        let reach = reach.downgrade();
        gtk4::glib::timeout_add_local(Duration::from_secs(15), move || {
            if reach.upgrade().is_none() {
                return gtk4::glib::ControlFlow::Break;
            }
            run_probe();
            gtk4::glib::ControlFlow::Continue
        });
    }
    let identity = gtk4::Expander::new(Some(tr("sync.device.title")));
    identity.add_css_class("settings-expander");
    if let Some(title) = card.first_child() {
        card.remove(&title);
    }
    card.remove_css_class("card");
    card.add_css_class("expanded-content");
    identity.set_child(Some(&card));
    col.append(&identity);

    // ---- Add device (node id only; addresses discovered) ----
    let card = GtkBox::new(Orientation::Vertical, 8);
    card.add_css_class("card");
    let t = Label::new(Some(tr("sync.add.title")));
    t.add_css_class("section-title");
    t.set_xalign(0.0);
    card.append(&t);
    let hint = Label::new(Some(tr("sync.add.hint")));
    hint.add_css_class("dim");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    card.append(&hint);

    let id_entry = Entry::new();
    id_entry.update_property(&[gtk4::accessible::Property::Label(tr("sync.add.idlabel"))]);
    id_entry.set_placeholder_text(Some(tr("sync.add.idhint")));
    // Fixed small minimum: the 64-char id scrolls inside instead of
    // forcing a horizontal scrollbar on the whole page.
    id_entry.set_width_chars(32);
    id_entry.set_hexpand(true);
    let id_label = Label::new(Some(tr("sync.add.idlabel")));
    id_label.set_xalign(0.0);
    id_label.add_css_class("row-label");
    card.append(&id_label);
    id_entry.update_property(&[gtk4::accessible::Property::Label(tr("sync.add.idlabel"))]);
    card.append(&id_entry);

    let addr_row = GtkBox::new(Orientation::Horizontal, 8);
    let addr_entry = Entry::new();
    addr_entry.update_property(&[gtk4::accessible::Property::Label(tr("sync.add.addrhint"))]);
    addr_entry.set_placeholder_text(Some(tr("sync.add.addrhint")));
    addr_entry.set_hexpand(true);
    addr_row.append(&addr_entry);
    let name_entry = Entry::new();
    name_entry.update_property(&[gtk4::accessible::Property::Label(tr("sync.add.namehint"))]);
    name_entry.set_placeholder_text(Some(tr("sync.add.namehint")));
    name_entry.set_max_width_chars(16);
    addr_row.append(&name_entry);
    let add = Button::with_label(tr("action.sendrequest"));
    let extras = gtk4::Expander::new(Some(tr("sync.add.optional")));
    extras.set_child(Some(&addr_row));
    card.append(&extras);
    addr_entry.update_property(&[gtk4::accessible::Property::Label(tr("sync.add.addrhint"))]);
    name_entry.update_property(&[gtk4::accessible::Property::Label(tr("sync.add.namehint"))]);
    add.set_halign(gtk4::Align::End);
    add.add_css_class("suggested-action");
    card.append(&add);
    card.append(&status);
    col.append(&card);

    // ---- Nearby (mDNS, same LAN, no configuration) ----
    let nearby_card = GtkBox::new(Orientation::Vertical, 8);
    nearby_card.add_css_class("card");
    let t = Label::new(Some(tr("sync.nearby.title")));
    t.add_css_class("section-title");
    t.set_xalign(0.0);
    nearby_card.append(&t);
    let nearby_list = ListBox::new();
    nearby_card.append(&nearby_list);
    col.insert_child_after(&nearby_card, Some(&svc_card));

    // ---- Requests (incoming pairing asks) ----
    let req_card = GtkBox::new(Orientation::Vertical, 8);
    req_card.add_css_class("card");
    let t = Label::new(Some(tr("sync.requests.title")));
    t.add_css_class("section-title");
    t.set_xalign(0.0);
    req_card.append(&t);
    let req_list = ListBox::new();
    req_card.append(&req_list);
    col.insert_child_after(&req_card, Some(&nearby_card));

    // ---- Devices ----
    let card = GtkBox::new(Orientation::Vertical, 8);
    card.add_css_class("card");
    let t = Label::new(Some(tr("sync.devices.title")));
    t.add_css_class("section-title");
    t.set_xalign(0.0);
    card.append(&t);
    let list = ListBox::new();
    card.append(&list);
    col.insert_child_after(&card, Some(&req_card));

    // One refresh pass for every live list.
    let refresh_all = Rc::new({
        let state = state.clone();
        let status = status.downgrade();
        move |nearby: &ListBox, reqs: &ListBox, peers: &ListBox| {
            let Some(status) = status.upgrade() else {
                return;
            };
            fill_nearby(nearby, &state, &status);
            fill_requests(reqs, &state, &status);
            fill_peers(peers, &state, &status);
        }
    });
    refresh_all(&nearby_list, &req_list, &list);

    // Background discovery + incoming requests repaint the lists.
    {
        let refresh_all = refresh_all.clone();
        let nearby_list = nearby_list.downgrade();
        let req_list = req_list.downgrade();
        let list = list.downgrade();
        let state = state.clone();
        let mut seen = list_signature(&state);
        let weak_root = root.downgrade();
        let mut discovery: Option<crate::net::MdnsHandles> = None;
        gtk4::glib::timeout_add_local(Duration::from_secs(2), move || {
            if weak_root.upgrade().is_none() {
                drop(discovery.take());
                return gtk4::glib::ControlFlow::Break;
            }
            let (Some(nearby_list), Some(req_list), Some(list)) =
                (nearby_list.upgrade(), req_list.upgrade(), list.upgrade())
            else {
                return gtk4::glib::ControlFlow::Break;
            };
            if state.settings.borrow().sync.enabled {
                if discovery.is_none() {
                    discovery = crate::net::start_mdns(
                        &state.node_id,
                        "",
                        0,
                        state.discovered.clone(),
                        false,
                    )
                    .ok();
                }
            } else if let Some(handles) = discovery.take() {
                drop(handles);
                state.discovered.lock().unwrap().clear();
            }
            let signature = list_signature(&state);
            if seen != signature {
                seen = signature;
                refresh_all(&nearby_list, &req_list, &list);
            }
            gtk4::glib::ControlFlow::Continue
        });
    }

    {
        let state = state.clone();
        let status = status.clone();
        let refresh_all = refresh_all.clone();
        let nearby_list = nearby_list.clone();
        let req_list = req_list.clone();
        let list = list.clone();
        add.connect_clicked(move |button| {
            status.set_visible(true);
            let node_id = id_entry.text().trim().to_ascii_lowercase();
            let address = addr_entry.text().trim().to_string();
            let name = name_entry.text().trim().to_string();
            if !valid_device_id(&node_id) {
                id_entry.add_css_class("error");
                id_entry.grab_focus();
                status.set_text(tr("sync.err.badid"));
                return;
            }
            if node_id == state.node_id {
                status.set_text(tr("sync.self"));
                return;
            }
            if !valid_address(&address) {
                status.set_text(tr("sync.address.invalid"));
                super::app::toast(button, tr("sync.address.invalid"));
                return;
            }
            id_entry.remove_css_class("error");
            if state
                .store
                .upsert_peer(&node_id, &name, &address, true)
                .is_err()
            {
                status.set_text(tr("sync.err.save"));
                return;
            }
            if !address.is_empty() {
                let _ = state.store.set_peer_addresses(&node_id, &[address]);
            }
            id_entry.set_text("");
            addr_entry.set_text("");
            name_entry.set_text("");
            refresh_all(&nearby_list, &req_list, &list);
            // If we already know an address, knock immediately: their
            // side records the pending request on contact, so Accept
            // appears without them typing anything.
            let targets = state
                .store
                .get_peer(&node_id)
                .ok()
                .flatten()
                .map(|p| p.dial_targets())
                .unwrap_or_default();
            if targets.is_empty() {
                status
                    .set_text(trf("sync.request.noaddr", &[("id", &short_id(&node_id))]).as_str());
            } else {
                status.set_text(
                    trf("sync.request.reaching", &[("id", &short_id(&node_id))]).as_str(),
                );
                let db_path = state.db_path.clone();
                let creds = state.creds.clone();
                let name = device_name(&state);
                let expect = node_id.clone();
                let result: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
                let result_tx = result.clone();
                std::thread::spawn(move || {
                    let msg = match crate::net::sync_now(&targets, &db_path, &creds, &name, &expect)
                    {
                        Ok(_) => tr("sync.request.delivered").to_string(),
                        Err(e) if e.contains("not trusted") => {
                            trf("sync.request.sent", &[("id", &short_id(&expect))])
                        }
                        Err(e) => trf("sync.request.unreachable", &[("err", &e.to_string())]),
                    };
                    *result_tx.lock().unwrap() = Some(msg);
                });
                let status = status.clone();
                gtk4::glib::timeout_add_local(Duration::from_millis(300), move || {
                    match result.lock().unwrap().take() {
                        Some(msg) => {
                            status.set_text(&msg);
                            gtk4::glib::ControlFlow::Break
                        }
                        None => gtk4::glib::ControlFlow::Continue,
                    }
                });
            }
            eprintln!("[app] sync: pairing requested with {}", short_id(&node_id));
        });
    }

    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_requires_a_full_hex_id() {
        assert!(valid_device_id(&"aF09".repeat(16)));
        assert!(!valid_device_id(&"g".repeat(64)));
        assert!(!valid_device_id(&"a".repeat(63)));
    }

    #[test]
    fn manual_addresses_require_a_host_and_nonzero_port() {
        for address in ["", "192.168.1.31:42973", "host.local:443", "[::1]:42973"] {
            assert!(valid_address(address), "{address}");
        }
        for address in [
            "host",
            ":443",
            "host:0",
            "host:65536",
            "host:no",
            "bad host:443",
        ] {
            assert!(!valid_address(address), "{address}");
        }
    }
}

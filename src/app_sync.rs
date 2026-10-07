//! Sync page: device identity, trusted peers, manual sync.
//!
//! Fully wired to the store: device name/port persist, peers add,
//! trust-toggle and remove, Sync-now runs the TCP exchange on a worker
//! thread and reports back. The listener itself starts in `main`.

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Button, Entry, Label, ListBox, Orientation, Switch};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::app::AppState;
use crate::i18n::{tr, trf};

fn short_id(node_id: &str) -> String {
    if node_id.len() > 16 {
        format!("{}…{}", &node_id[..8], &node_id[node_id.len() - 4..])
    } else {
        node_id.to_string()
    }
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
    }
}

fn clear_list(list: &ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn fill_peers(list: &ListBox, state: &Rc<AppState>, status: &Label) {
    // Owned clones: rows outlive this call through button closures.
    let list = list.clone();
    let status = status.clone();
    clear_list(&list);
    let peers = state.store.list_peers().unwrap_or_default();
    if peers.is_empty() {
        let l = Label::new(Some(tr("sync.devices.empty")));
        l.add_css_class("dim");
        list.append(&l);
        return;
    }
    for p in peers {
        let list_remove = list.clone();
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
        top.append(&name);

        let trust = Switch::new();
        trust.set_active(p.trusted);
        trust.set_valign(gtk4::Align::Center);
        trust.set_tooltip_text(Some(tr("sync.trusted")));
        {
            let state = state.clone();
            let node_id = p.node_id.clone();
            let peer_name = p.name.clone();
            let address = p.address.clone();
            trust.connect_state_set(move |_, active| {
                let _ = state
                    .store
                    .upsert_peer(&node_id, &peer_name, &address, active);
                eprintln!("[app] sync: trust {} -> {active}", short_id(&node_id));
                gtk4::glib::Propagation::Proceed
            });
        }
        top.append(&trust);

        let sync_now = Button::with_label(tr("action.syncnow"));
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
            remove.connect_clicked(move |_| {
                let _ = state.store.remove_peer(&node_id);
                fill_peers(&list, &state, &status);
                eprintln!("[app] sync: removed peer {}", short_id(&node_id));
            });
        }
        top.append(&remove);
        card.append(&top);

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
        l.set_hexpand(true);
        row.append(&l);
        let add = Button::with_label(tr("action.add"));
        {
            let state = state.clone();
            let status = status.clone();
            add.connect_clicked(move |_| {
                let _ = state.store.upsert_peer(&d.node_id, &d.name, "", true);
                let _ = state.store.set_peer_addresses(&d.node_id, &d.addresses);
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
        l.set_hexpand(true);
        row.append(&l);
        let accept = Button::with_label(tr("action.accept"));
        {
            let state = state.clone();
            let status = status.clone();
            let node_id = p.node_id.clone();
            accept.connect_clicked(move |_| {
                if state.store.decide_request(&node_id, true).is_err() {
                    status.set_text(tr("sync.err.accept"));
                    return;
                }
                status.set_text(tr("sync.accepted"));
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
                let _ = state.store.decide_request(&node_id, false);
                status.set_text(tr("sync.declined"));
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
                gtk4::glib::ControlFlow::Break
            }
            None => gtk4::glib::ControlFlow::Continue,
        }
    });
}

pub fn page(state: &Rc<AppState>, listening_port: u16) -> GtkBox {
    // Title lives in the modal header; content starts at the cards.
    let root = GtkBox::new(Orientation::Vertical, 0);
    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_hexpand(true);
    // Hard cap: the viewport must never outgrow the modal panel, or
    // scrolled content paints square over the panel's rounded border.
    scrolled.set_max_content_height(560);
    scrolled.set_propagate_natural_height(false);
    root.append(&scrolled);
    let inner = GtkBox::new(Orientation::Vertical, 16);
    inner.add_css_class("page");
    inner.set_margin_bottom(16);
    scrolled.set_child(Some(&inner));
    let col = super::app::centered_column(&inner, 840);

    let status = Label::new(Some(""));
    status.add_css_class("dim");
    status.set_xalign(0.0);
    status.set_wrap(true);

    // ---- Background service ----
    // Hotkeys, sync and the pill live in the service process only.
    // This card reports its state and starts/stops it on demand.
    let svc_card = GtkBox::new(Orientation::Vertical, 8);
    svc_card.add_css_class("card");
    let t = Label::new(Some(tr("sync.svc.title")));
    t.add_css_class("section-title");
    t.set_xalign(0.0);
    svc_card.append(&t);
    let svc_row = GtkBox::new(Orientation::Horizontal, 8);
    let svc_label = Label::new(Some(""));
    svc_label.set_xalign(0.0);
    svc_label.set_hexpand(true);
    svc_row.append(&svc_label);
    let svc_toggle = Button::with_label("");
    svc_row.append(&svc_toggle);
    svc_card.append(&svc_row);
    col.append(&svc_card);
    let refresh_svc: Rc<dyn Fn()> = Rc::new({
        let state = state.clone();
        let svc_label = svc_label.clone();
        let svc_toggle = svc_toggle.clone();
        move || {
            // Re-probe the lock each refresh: the service may have
            // been started/stopped outside this process.
            let alive = state
                .db_path
                .parent()
                .map(|d| crate::service::engine_running_paths(&d.join("engine.lock")))
                .unwrap_or(false);
            state.service_alive.set(alive);
            if alive {
                svc_label.set_text(tr("sync.svc.on"));
                svc_toggle.set_label(tr("action.stop"));
            } else {
                svc_label.set_text(tr("sync.svc.off"));
                svc_toggle.set_label(tr("action.start"));
            }
        }
    });
    refresh_svc();
    {
        let state = state.clone();
        let refresh_svc = refresh_svc.clone();
        svc_toggle.connect_clicked(move |_| {
            let want_running = !state.service_alive.get();
            match crate::service::set_service_running(want_running) {
                Ok(msg) => eprintln!("[app] {msg}"),
                Err(e) => eprintln!("[app] service toggle failed: {e}"),
            }
            // Re-probe after a beat; systemctl is async.
            let refresh_svc = refresh_svc.clone();
            gtk4::glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
                refresh_svc()
            });
        });
    }

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
    // Read-only field: a wrapping view measures huge before its first
    // layout and blows up the modal until it settles. An entry keeps
    // a fixed width and scrolls the 64 chars internally.
    let id_value = Entry::new();
    id_value.set_editable(false);
    id_value.set_can_focus(true);
    id_value.set_text(&state.node_id);
    id_value.set_width_chars(42);
    id_value.set_hexpand(true);
    id_row.append(&id_value);
    let copy = Button::with_label(tr("action.copy"));
    {
        let node_id = state.node_id.clone();
        copy.connect_clicked(move |_| {
            if let Some(display) = gtk4::gdk::Display::default() {
                display.clipboard().set_text(&node_id);
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
                eprintln!("[app] sync: listen port saved ({port}, restart applies)");
            }
        });
    }
    port_row.append(&port_save);
    card.append(&port_row);

    let listen = Label::new(Some(
        if listening_port == 0 {
            tr("sync.listener.off").to_string()
        } else {
            trf("sync.listener.on", &[("port", &listening_port.to_string())])
        }
        .as_str(),
    ));
    listen.add_css_class("dim");
    listen.set_xalign(0.0);
    card.append(&listen);

    // Self-reachability probe. Explicit button, never automatic:
    // the firewall query can pop a polkit auth dialog, so it only
    // runs when the user asks.
    let reach_row = GtkBox::new(Orientation::Horizontal, 8);
    let reach = Label::new(Some(tr("sync.reach.unknown")));
    reach.add_css_class("dim");
    reach.set_xalign(0.0);
    reach.set_wrap(true);
    reach.set_hexpand(true);
    reach_row.append(&reach);
    let check = Button::with_label(tr("action.check"));
    reach_row.append(&check);
    card.append(&reach_row);
    {
        check.connect_clicked(move |_| {
            reach.set_text(tr("sync.reach.checking"));
            let result: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
            let result_tx = result.clone();
            std::thread::spawn(move || {
                // The loopback dial only proves the listener answers
                // (it leaves via OUTPUT, which firewalls allow), so
                // report it and the firewall state as separate facts.
                let up = crate::net::self_reachable(listening_port).is_some();
                let msg = match (up, crate::net::firewall_state(listening_port)) {
                    (false, _) => trf("sync.reach.down", &[("port", &listening_port.to_string())]),
                    (true, Some((true, _))) => {
                        trf("sync.reach.open", &[("port", &listening_port.to_string())])
                    }
                    (true, Some((false, _))) => trf(
                        "sync.reach.blocked",
                        &[("port", &listening_port.to_string())],
                    ),
                    (true, None) => trf(
                        "sync.reach.unknownfw",
                        &[("port", &listening_port.to_string())],
                    ),
                };
                *result_tx.lock().unwrap() = Some(msg);
            });
            let reach = reach.clone();
            gtk4::glib::timeout_add_local(Duration::from_millis(300), move || {
                match result.lock().unwrap().take() {
                    Some(msg) => {
                        reach.set_text(&msg);
                        gtk4::glib::ControlFlow::Break
                    }
                    None => gtk4::glib::ControlFlow::Continue,
                }
            });
        });
    }
    col.append(&card);

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
    id_entry.set_placeholder_text(Some(tr("sync.add.idhint")));
    // Fixed small minimum: the 64-char id scrolls inside instead of
    // forcing a horizontal scrollbar on the whole page.
    id_entry.set_width_chars(32);
    id_entry.set_hexpand(true);
    card.append(&id_entry);

    let addr_row = GtkBox::new(Orientation::Horizontal, 8);
    let addr_entry = Entry::new();
    addr_entry.set_placeholder_text(Some(tr("sync.add.addrhint")));
    addr_entry.set_hexpand(true);
    addr_row.append(&addr_entry);
    let name_entry = Entry::new();
    name_entry.set_placeholder_text(Some(tr("sync.add.namehint")));
    name_entry.set_max_width_chars(16);
    addr_row.append(&name_entry);
    let add = Button::with_label(tr("action.sendrequest"));
    addr_row.append(&add);
    card.append(&addr_row);
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
    col.append(&nearby_card);

    // ---- Requests (incoming pairing asks) ----
    let req_card = GtkBox::new(Orientation::Vertical, 8);
    req_card.add_css_class("card");
    let t = Label::new(Some(tr("sync.requests.title")));
    t.add_css_class("section-title");
    t.set_xalign(0.0);
    req_card.append(&t);
    let req_list = ListBox::new();
    req_card.append(&req_list);
    col.append(&req_card);

    // ---- Devices ----
    let card = GtkBox::new(Orientation::Vertical, 8);
    card.add_css_class("card");
    let t = Label::new(Some(tr("sync.devices.title")));
    t.add_css_class("section-title");
    t.set_xalign(0.0);
    card.append(&t);
    let list = ListBox::new();
    card.append(&list);
    col.append(&card);
    col.append(&status);

    // One refresh pass for every live list.
    let refresh_all = Rc::new({
        let state = state.clone();
        let status = status.clone();
        move |nearby: &ListBox, reqs: &ListBox, peers: &ListBox| {
            fill_nearby(nearby, &state, &status);
            fill_requests(reqs, &state, &status);
            fill_peers(peers, &state, &status);
        }
    });
    refresh_all(&nearby_list, &req_list, &list);

    // Background discovery + incoming requests repaint the lists,
    // plus the service status banner.
    {
        let refresh_all = refresh_all.clone();
        let nearby_list = nearby_list.clone();
        let req_list = req_list.clone();
        let list = list.clone();
        let refresh_svc = refresh_svc.clone();
        gtk4::glib::timeout_add_local(Duration::from_secs(2), move || {
            refresh_all(&nearby_list, &req_list, &list);
            refresh_svc();
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
        add.connect_clicked(move |_| {
            let node_id = id_entry.text().trim().to_string();
            let address = addr_entry.text().trim().to_string();
            let name = name_entry.text().trim().to_string();
            if node_id.len() != 64 {
                status.set_text(tr("sync.err.badid"));
                return;
            }
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
                        Err(e) if e.contains("not trusted") => tr("sync.request.sent").to_string(),
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

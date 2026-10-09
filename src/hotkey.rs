//! Global hotkeys via direct evdev reads (OpenWhispr method).
//!
//! A background thread opens every `/dev/input/event*` keyboard
//! (EV_KEY + KEY_A capability, same filter as OpenWhispr's
//! `linux-key-listener.c`) read-only -- never grabbed, never
//! intercepted -- and tracks held keys. Each configured combo
//! resolves to `(modifiers, target)` exactly like the C listener:
//! press emits `KeyDown`, release emits `KeyUp`.
//!
//! Actions are an enum (`DictationToggle` today) so future actions
//! reuse the same manager. Bindings are a `Vec<String>` per action:
//! any combo may fire it.

use evdev::{Device, EventSummary, EventType, KeyCode};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::FileTypeExt;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

/// Extensible action set. Only dictation exists today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    DictationToggle,
}

/// Parsed combo: required modifiers + one target key.
/// Modifier-only combos (e.g. "Control+Super") set `target = None`.
#[derive(Debug, Clone)]
pub struct Combo {
    pub raw: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_: bool,
    pub target: Option<KeyCode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyStatus {
    Listening,
    NoPermission,
    NoDevices,
}

/// Key-down / key-up for one action binding. The mode travels with
/// the event so each binding behaves per its own setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Down(HotkeyAction, crate::settings::ActivationMode),
    Up(HotkeyAction, crate::settings::ActivationMode),
    /// Escape pressed while a dictation is live: cancel midway.
    Cancel,
}

pub type HotkeyCallback = Arc<dyn Fn(HotkeyEvent) + Send + Sync + 'static>;

/// Generation counter: bumped on every (re)start so a stale loop
/// exits instead of double-firing after settings change.
static HOTKEY_GEN: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------- parsing

fn key_by_name(name: &str) -> Option<KeyCode> {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "F1" => Some(KeyCode::KEY_F1),
        "F2" => Some(KeyCode::KEY_F2),
        "F3" => Some(KeyCode::KEY_F3),
        "F4" => Some(KeyCode::KEY_F4),
        "F5" => Some(KeyCode::KEY_F5),
        "F6" => Some(KeyCode::KEY_F6),
        "F7" => Some(KeyCode::KEY_F7),
        "F8" => Some(KeyCode::KEY_F8),
        "F9" => Some(KeyCode::KEY_F9),
        "F10" => Some(KeyCode::KEY_F10),
        "F11" => Some(KeyCode::KEY_F11),
        "F12" => Some(KeyCode::KEY_F12),
        "F13" => Some(KeyCode::KEY_F13),
        "F14" => Some(KeyCode::KEY_F14),
        "F15" => Some(KeyCode::KEY_F15),
        "F16" => Some(KeyCode::KEY_F16),
        "F17" => Some(KeyCode::KEY_F17),
        "F18" => Some(KeyCode::KEY_F18),
        "F19" => Some(KeyCode::KEY_F19),
        "F20" => Some(KeyCode::KEY_F20),
        "F21" => Some(KeyCode::KEY_F21),
        "F22" => Some(KeyCode::KEY_F22),
        "F23" => Some(KeyCode::KEY_F23),
        "F24" => Some(KeyCode::KEY_F24),
        "SPACE" => Some(KeyCode::KEY_SPACE),
        "ESC" | "ESCAPE" => Some(KeyCode::KEY_ESC),
        "TAB" => Some(KeyCode::KEY_TAB),
        "ENTER" | "RETURN" => Some(KeyCode::KEY_ENTER),
        "BACKSPACE" => Some(KeyCode::KEY_BACKSPACE),
        "INSERT" => Some(KeyCode::KEY_INSERT),
        "DELETE" => Some(KeyCode::KEY_DELETE),
        "HOME" => Some(KeyCode::KEY_HOME),
        "END" => Some(KeyCode::KEY_END),
        "PAGEUP" => Some(KeyCode::KEY_PAGEUP),
        "PAGEDOWN" => Some(KeyCode::KEY_PAGEDOWN),
        "UP" => Some(KeyCode::KEY_UP),
        "DOWN" => Some(KeyCode::KEY_DOWN),
        "LEFT" => Some(KeyCode::KEY_LEFT),
        "RIGHT" => Some(KeyCode::KEY_RIGHT),
        _ => {
            if name.len() == 1 {
                let c = upper.chars().next().unwrap();
                match c {
                    'A' => Some(KeyCode::KEY_A),
                    'B' => Some(KeyCode::KEY_B),
                    'C' => Some(KeyCode::KEY_C),
                    'D' => Some(KeyCode::KEY_D),
                    'E' => Some(KeyCode::KEY_E),
                    'F' => Some(KeyCode::KEY_F),
                    'G' => Some(KeyCode::KEY_G),
                    'H' => Some(KeyCode::KEY_H),
                    'I' => Some(KeyCode::KEY_I),
                    'J' => Some(KeyCode::KEY_J),
                    'K' => Some(KeyCode::KEY_K),
                    'L' => Some(KeyCode::KEY_L),
                    'M' => Some(KeyCode::KEY_M),
                    'N' => Some(KeyCode::KEY_N),
                    'O' => Some(KeyCode::KEY_O),
                    'P' => Some(KeyCode::KEY_P),
                    'Q' => Some(KeyCode::KEY_Q),
                    'R' => Some(KeyCode::KEY_R),
                    'S' => Some(KeyCode::KEY_S),
                    'T' => Some(KeyCode::KEY_T),
                    'U' => Some(KeyCode::KEY_U),
                    'V' => Some(KeyCode::KEY_V),
                    'W' => Some(KeyCode::KEY_W),
                    'X' => Some(KeyCode::KEY_X),
                    'Y' => Some(KeyCode::KEY_Y),
                    'Z' => Some(KeyCode::KEY_Z),
                    '0' => Some(KeyCode::KEY_0),
                    '1' => Some(KeyCode::KEY_1),
                    '2' => Some(KeyCode::KEY_2),
                    '3' => Some(KeyCode::KEY_3),
                    '4' => Some(KeyCode::KEY_4),
                    '5' => Some(KeyCode::KEY_5),
                    '6' => Some(KeyCode::KEY_6),
                    '7' => Some(KeyCode::KEY_7),
                    '8' => Some(KeyCode::KEY_8),
                    '9' => Some(KeyCode::KEY_9),
                    _ => None,
                }
            } else {
                None
            }
        }
    }
}

/// Inverse of `key_by_name`: evdev code -> combo token.
/// Used by GTK press-to-record capture, which sees hardware keycodes
/// (XKB keycode = evdev code + 8) while GDK keyvals go through the
/// layout and may surface as XF86 symbols (e.g. a key wired as F13
/// reporting `Tools`). Keycodes keep capture and listener in exactly
/// one vocabulary.
pub fn token_for_evdev_code(code: u16) -> Option<&'static str> {
    Some(match code {
        1 => "Escape",
        2 => "1",
        3 => "2",
        4 => "3",
        5 => "4",
        6 => "5",
        7 => "6",
        8 => "7",
        9 => "8",
        10 => "9",
        11 => "0",
        12 => "-",
        13 => "=",
        14 => "Backspace",
        15 => "Tab",
        16 => "Q",
        17 => "W",
        18 => "E",
        19 => "R",
        20 => "T",
        21 => "Y",
        22 => "U",
        23 => "I",
        24 => "O",
        25 => "P",
        28 => "Enter",
        30 => "A",
        48 => "B",
        54 => "C",
        56 => "Alt",
        57 => "Space",
        103 => "Up",
        104 => "PageUp",
        105 => "Left",
        106 => "Right",
        107 => "End",
        108 => "Down",
        109 => "PageDown",
        110 => "Insert",
        111 => "Delete",
        183 => "F13",
        184 => "F14",
        185 => "F15",
        186 => "F16",
        187 => "F17",
        188 => "F18",
        189 => "F19",
        190 => "F20",
        191 => "F21",
        192 => "F22",
        193 => "F23",
        194 => "F24",
        _ => return None,
    })
}
pub fn parse_combo(raw: &str) -> Option<Combo> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut super_ = false;
    let mut target = None;
    let mut any = false;
    for token in raw.split('+') {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        any = true;
        let u = t.to_ascii_uppercase();
        match u.as_str() {
            "CTRL" | "CONTROL" | "COMMANDORCONTROL" | "CMDORCTRL" => ctrl = true,
            "ALT" | "OPTION" => alt = true,
            "SHIFT" => shift = true,
            "SUPER" | "META" | "WIN" | "CMD" | "COMMAND" => super_ = true,
            _ => {
                if let Some(code) = key_by_name(t) {
                    target = Some(code);
                } else {
                    eprintln!("[hotkey] WARNING: unknown key {t:?} in {raw:?}");
                    return None;
                }
            }
        }
    }
    if !any {
        return None;
    }
    if target.is_none() && !(ctrl || alt || shift || super_) {
        return None;
    }
    Some(Combo {
        raw: raw.to_string(),
        ctrl,
        alt,
        shift,
        super_,
        target,
    })
}

// ---------------------------------------------------------------- devices

fn is_keyboard(dev: &Device) -> bool {
    if !dev.supported_events().contains(EventType::KEY) {
        return false;
    }
    match dev.supported_keys() {
        Some(keys) => keys.contains(KeyCode::KEY_A),
        None => false,
    }
}

fn open_keyboards() -> (Vec<(std::path::PathBuf, Device)>, bool) {
    let mut devices = Vec::new();
    let mut denied = false;
    let dir = match std::fs::read_dir("/dev/input") {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return (devices, true),
        Err(_) => return (devices, false),
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("event") {
            continue;
        }
        let ft = match entry.file_type() {
            Ok(f) => f,
            Err(_) => continue,
        };
        if !ft.is_char_device() {
            continue;
        }
        match Device::open(entry.path()) {
            Ok(dev) => {
                if is_keyboard(&dev) {
                    // Nonblocking: `fetch_events` blocks by default,
                    // which would wedge the poll loop on the first
                    // quiet device while another holds new events.
                    let _ = dev.set_nonblocking(true);
                    devices.push((entry.path(), dev));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => denied = true,
            Err(_) => {}
        }
    }
    (devices, denied)
}

// ---------------------------------------------------------------- state

/// Merge a rescan into the live set: keep existing fds untouched
/// (their buffered events and held state survive), open only new
/// nodes, drop vanished ones.
fn merge_devices(
    devices: &mut Vec<(std::path::PathBuf, Device)>,
    found: Vec<(std::path::PathBuf, Device)>,
) {
    for (path, dev) in found {
        if !devices.iter().any(|(p, _)| p == &path) {
            devices.push((path, dev));
        }
    }
    devices.retain(|(p, _)| p.exists());
}

fn ctrl_held(held: &HashSet<KeyCode>) -> bool {
    held.contains(&KeyCode::KEY_LEFTCTRL) || held.contains(&KeyCode::KEY_RIGHTCTRL)
}

fn alt_held(held: &HashSet<KeyCode>) -> bool {
    held.contains(&KeyCode::KEY_LEFTALT) || held.contains(&KeyCode::KEY_RIGHTALT)
}

fn shift_held(held: &HashSet<KeyCode>) -> bool {
    held.contains(&KeyCode::KEY_LEFTSHIFT) || held.contains(&KeyCode::KEY_RIGHTSHIFT)
}

fn super_held(held: &HashSet<KeyCode>) -> bool {
    held.contains(&KeyCode::KEY_LEFTMETA) || held.contains(&KeyCode::KEY_RIGHTMETA)
}

fn mods_ok(combo: &Combo, held: &HashSet<KeyCode>) -> bool {
    (!combo.ctrl || ctrl_held(held))
        && (!combo.alt || alt_held(held))
        && (!combo.shift || shift_held(held))
        && (!combo.super_ || super_held(held))
}

fn is_modifier(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::KEY_LEFTCTRL
            | KeyCode::KEY_RIGHTCTRL
            | KeyCode::KEY_LEFTALT
            | KeyCode::KEY_RIGHTALT
            | KeyCode::KEY_LEFTSHIFT
            | KeyCode::KEY_RIGHTSHIFT
            | KeyCode::KEY_LEFTMETA
            | KeyCode::KEY_RIGHTMETA
    )
}

/// One watched binding: parsed combo + its action + its own mode +
/// edge state. `active_since` bounds a Push hold: if a release is
/// ever lost (fd torn down mid-press), the watchdog force-releases
/// instead of wedging dictation on.
struct Watched {
    action: HotkeyAction,
    combo: Combo,
    mode: crate::settings::ActivationMode,
    active: bool,
    active_since: Option<Instant>,
}

/// Push holds wedged active longer than this are force-released.
/// Tap bindings are exempt (toggled-on is a legitimate long state).
/// 15 minutes: long-form dictation holds for many minutes, and the
/// 60 s we had before amputated exactly those sessions (log-proven).
const MAX_PUSH_SECS: u64 = 900;

fn poll_events(
    devices: &mut Vec<(std::path::PathBuf, Device)>,
    held: &mut HashSet<KeyCode>,
    watched: &mut [Watched],
    on_event: &HotkeyCallback,
) {
    let mut dead = Vec::new();
    for (i, (_, dev)) in devices.iter_mut().enumerate() {
        let events: Vec<_> = match dev.fetch_events() {
            Ok(it) => it.collect(),
            // Nonblocking fds report WouldBlock when idle: not death.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(_) => {
                dead.push(i);
                continue;
            }
        };
        for ev in events {
            let EventSummary::Key(_, key, value) = ev.destructure() else {
                continue;
            };
            // value 0 = release, 1 = press, 2 = repeat (ignored).
            // Banner mute: when hotkeys are disabled, no event
            // reaches the pipeline (presses are swallowed here).
            if !crate::transcribe::hotkeys_listening() {
                continue;
            }
            match value {
                1 => {
                    held.insert(key);
                }
                0 => {
                    held.remove(&key);
                }
                _ => continue,
            };
            let pressed = value == 1;
            // Escape cancels a live utterance (midway cancel). Gated
            // on live state: idle Escape presses never emit.
            if pressed && key == KeyCode::KEY_ESC && crate::transcribe::has_live() {
                eprintln!("[hotkey] escape cancels live utterance");
                on_event(HotkeyEvent::Cancel);
            }
            for w in watched.iter_mut() {
                let c = &w.combo;
                if let Some(target) = c.target {
                    if key != target {
                        continue;
                    }
                    if pressed && !w.active && mods_ok(c, held) {
                        w.active = true;
                        w.active_since = Some(Instant::now());
                        on_event(HotkeyEvent::Down(w.action, w.mode));
                    } else if !pressed && w.active {
                        w.active = false;
                        w.active_since = None;
                        on_event(HotkeyEvent::Up(w.action, w.mode));
                    }
                } else {
                    // Modifier-only combo.
                    if !is_modifier(key) {
                        continue;
                    }
                    if mods_ok(c, held) && !w.active {
                        w.active = true;
                        w.active_since = Some(Instant::now());
                        on_event(HotkeyEvent::Down(w.action, w.mode));
                    } else if !mods_ok(c, held) && w.active {
                        w.active = false;
                        w.active_since = None;
                        on_event(HotkeyEvent::Up(w.action, w.mode));
                    }
                }
            }
        }
    }
    // Prune dead fds (unplugged mid-poll). Reverse order keeps
    // indexes valid.
    for i in dead.into_iter().rev() {
        eprintln!("[hotkey] device gone, dropping fd");
        devices.remove(i);
    }
}

/// Blocking listener. Rereads the device dir every few seconds for
/// hotplug (mirrors the C listener's inotify rescan, cheaply).
fn run_loop(
    generation: u64,
    mut watched: Vec<Watched>,
    on_event: HotkeyCallback,
    on_status: Arc<dyn Fn(HotkeyStatus) + Send + Sync + 'static>,
) {
    let mut held: HashSet<KeyCode> = HashSet::new();
    let mut devices: Vec<(std::path::PathBuf, Device)> = Vec::new();
    let mut reported = false;
    let mut rescan_in = 0u32;
    loop {
        if HOTKEY_GEN.load(Ordering::SeqCst) != generation {
            return; // superseded
        }
        if rescan_in == 0 {
            let (found, denied) = open_keyboards();
            // Merge by node path: keep existing fds (with their
            // kernel-buffered events and held state), add only new
            // nodes, drop vanished ones. Replacing the whole set
            // here used to lose a release landing mid-rescan and
            // wedge the pill on.
            merge_devices(&mut devices, found);
            if devices.is_empty() {
                if denied {
                    if !reported {
                        on_status(HotkeyStatus::NoPermission);
                        reported = true;
                    }
                } else if !reported {
                    on_status(HotkeyStatus::NoDevices);
                    reported = true;
                }
            } else if !reported {
                let names: Vec<String> = devices
                    .iter()
                    .map(|(_, d)| d.name().unwrap_or("?").to_string())
                    .collect();
                eprintln!(
                    "[hotkey] watching {} keyboard(s): {}",
                    devices.len(),
                    names.join(", ")
                );
                on_status(HotkeyStatus::Listening);
                reported = true;
            }
            rescan_in = 100; // ~5s at 50ms polls
        }
        rescan_in -= 1;
        // Seed held state from the kernel so a boot-time-held key
        // cannot wedge an edge.
        poll_events(&mut devices, &mut held, &mut watched, &on_event);
        // Watchdog: a Push hold wedged by a lost release is
        // force-released instead of sticking dictation on forever.
        for w in watched.iter_mut() {
            if w.active
                && w.mode == crate::settings::ActivationMode::Push
                && w.active_since
                    .map(|t| t.elapsed() > Duration::from_secs(MAX_PUSH_SECS))
                    .unwrap_or(false)
            {
                eprintln!("[hotkey] WARNING: push stuck >{MAX_PUSH_SECS}s, force release");
                w.active = false;
                w.active_since = None;
                on_event(HotkeyEvent::Up(w.action, w.mode));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------- manager

/// Start (or restart) the global listener. Bumps the generation so
/// any previous loop exits. `bindings` maps actions to raw combo
/// strings; unparsable entries are skipped with a warning.
pub fn start(
    bindings: HashMap<HotkeyAction, Vec<crate::settings::HotkeyBinding>>,
    on_event: HotkeyCallback,
    on_status: Arc<dyn Fn(HotkeyStatus) + Send + Sync + 'static>,
) {
    let generation = HOTKEY_GEN.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    HOTKEY_GEN.store(generation, Ordering::SeqCst);
    let mut watched = Vec::new();
    for (action, binds) in bindings {
        for b in binds {
            match parse_combo(&b.combo) {
                Some(combo) => {
                    eprintln!(
                        "[hotkey] watching {:?} ({:?}) for {action:?}",
                        b.combo, b.mode
                    );
                    watched.push(Watched {
                        action,
                        combo,
                        mode: b.mode,
                        active: false,
                        active_since: None,
                    });
                }
                None => eprintln!("[hotkey] WARNING: skipping invalid combo {:?}", b.combo),
            }
        }
    }
    if watched.is_empty() {
        eprintln!("[hotkey] no valid bindings, listener idle");
        return;
    }
    // Dedicated loop: Builder-based so saturation logs instead of
    // panicking the caller (which may be the GTK-adjacent watcher).
    if let Err(e) = crate::runtime::spawn_dedicated("hotkey-loop", move || {
        run_loop(generation, watched, on_event, on_status)
    }) {
        eprintln!("[hotkey] FATAL: {e} (hotkeys dead)");
    }
}

/// Stop all listeners.
pub fn stop() {
    HOTKEY_GEN.fetch_add(1, Ordering::SeqCst);
}

/// Dictation edge hook: must be `Send` (runs on the listener
/// thread). The pill lives on the GTK thread, so service callbacks
/// typically forward through a channel + `idle_add`.
pub type DictationHook = Arc<dyn Fn() + Send + Sync + 'static>;

/// Smart-mode tap ceiling default (ms): a press shorter than this
/// toggles, a longer one holds. Overridable per install via the
/// `smart_tap_ms` setting; releases log their duration for calibration.
pub const SMART_TAP_MS: u64 = 150;

/// Smart press outcome, pure for testability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmartDown {
    /// Was idle: dictation started, this press owns the session.
    Open,
    /// Was active: dictation stopped (toggle-off), press owns nothing.
    Close,
}

fn smart_down(dictating: bool) -> SmartDown {
    if dictating {
        SmartDown::Close
    } else {
        SmartDown::Open
    }
}

/// Whether a smart release stops dictation: only when the press that
/// opened the session was held past the tap ceiling.
fn smart_up_should_stop(opener: bool, held_ms: u64, tap_ms: u64) -> bool {
    opener && held_ms >= tap_ms
}

/// (Re)start the listener from live settings with explicit dictation
/// hooks. Each binding behaves per its own mode; the STT pipeline (or
/// the pill) hooks into `down` / `up`. Call on startup and after
/// every hotkey change.
pub fn start_dictation(
    bindings: Vec<crate::settings::HotkeyBinding>,
    tap_ms: u64,
    down: DictationHook,
    up: DictationHook,
    cancel: DictationHook,
) {
    use crate::settings::ActivationMode;
    let mut map = HashMap::new();
    map.insert(HotkeyAction::DictationToggle, bindings);
    // Shared toggle state across bindings of the same action; the
    // mode travels on the event so mixed Push+Tap bindings coexist.
    let dictating = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Smart presses: what the press did + when, decided at release.
    let smart_press: Arc<std::sync::Mutex<Option<(Instant, bool)>>> =
        Arc::new(std::sync::Mutex::new(None));
    let on_event: HotkeyCallback = Arc::new(move |ev| match ev {
        HotkeyEvent::Down(HotkeyAction::DictationToggle, ActivationMode::Push) => {
            dictating.store(true, Ordering::SeqCst);
            down();
        }
        HotkeyEvent::Up(HotkeyAction::DictationToggle, ActivationMode::Push) => {
            dictating.store(false, Ordering::SeqCst);
            up();
        }
        HotkeyEvent::Down(HotkeyAction::DictationToggle, ActivationMode::Tap) => {
            let was = dictating.fetch_xor(true, Ordering::SeqCst);
            if !was {
                down();
            } else {
                up();
            }
        }
        HotkeyEvent::Down(HotkeyAction::DictationToggle, ActivationMode::Smart) => {
            match smart_down(dictating.load(Ordering::SeqCst)) {
                SmartDown::Open => {
                    dictating.store(true, Ordering::SeqCst);
                    *smart_press.lock().unwrap() = Some((Instant::now(), true));
                    down();
                }
                SmartDown::Close => {
                    dictating.store(false, Ordering::SeqCst);
                    *smart_press.lock().unwrap() = Some((Instant::now(), false));
                    up();
                }
            }
        }
        HotkeyEvent::Up(HotkeyAction::DictationToggle, ActivationMode::Smart) => {
            if let Some((t0, opener)) = smart_press.lock().unwrap().take() {
                let held_ms = t0.elapsed().as_millis() as u64;
                eprintln!("[hotkey] smart release after {held_ms}ms (ceiling {tap_ms}ms)");
                if smart_up_should_stop(opener, held_ms, tap_ms) {
                    dictating.store(false, Ordering::SeqCst);
                    up();
                }
            }
        }
        HotkeyEvent::Cancel => {
            cancel();
        }
        _ => {}
    });
    let on_status: Arc<dyn Fn(HotkeyStatus) + Send + Sync + 'static> =
        Arc::new(|st| eprintln!("[hotkey] status: {st:?}"));
    start(map, on_event, on_status);
}

/// Log-only hooks for standalone GUI mode (no pill in this process).
fn log_hooks() -> (DictationHook, DictationHook, DictationHook) {
    (
        Arc::new(|| eprintln!("[hotkey] dictation START (pipeline hook)")),
        Arc::new(|| eprintln!("[hotkey] dictation STOP (pipeline hook)")),
        Arc::new(|| eprintln!("[hotkey] dictation CANCEL (pipeline hook)")),
    )
}

/// (Re)start the listener from live settings with log-only hooks.
/// Standalone GUI mode uses this; the service passes pill hooks to
/// `start_dictation` instead.
pub fn apply_from_settings(state: &crate::app::AppState) {
    let hotkeys = state.settings.borrow().hotkeys.clone();
    let (down, up, cancel) = log_hooks();
    start_dictation(
        hotkeys.dictation_hotkeys,
        hotkeys.smart_tap_ms,
        down,
        up,
        cancel,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_f13_and_combos() {
        let c = parse_combo("F13").unwrap();
        assert_eq!(c.target, Some(KeyCode::KEY_F13));
        assert!(!c.ctrl && !c.alt && !c.shift && !c.super_);
        let c = parse_combo("Ctrl+Shift+Space").unwrap();
        assert!(c.ctrl && c.shift && !c.alt);
        assert_eq!(c.target, Some(KeyCode::KEY_SPACE));
        assert!(parse_combo("").is_none());
        assert!(parse_combo("Nope+NotAKey").is_none());
    }

    #[test]
    fn smart_tap_holds_toggle() {
        // Tap (under the ceiling) toggles: opener release keeps on.
        assert_eq!(smart_down(false), SmartDown::Open);
        assert!(!smart_up_should_stop(true, SMART_TAP_MS - 1, SMART_TAP_MS));
        // Hold (at/over the ceiling) behaves like push: release stops.
        assert!(smart_up_should_stop(true, SMART_TAP_MS, SMART_TAP_MS));
        assert!(smart_up_should_stop(
            true,
            SMART_TAP_MS + 5000,
            SMART_TAP_MS
        ));
        // A press that closed (toggle-off) never stops on release.
        assert_eq!(smart_down(true), SmartDown::Close);
        assert!(!smart_up_should_stop(
            false,
            SMART_TAP_MS + 5000,
            SMART_TAP_MS
        ));
        // Custom ceiling honored.
        assert!(!smart_up_should_stop(true, 100, 150));
        assert!(smart_up_should_stop(true, 150, 150));
    }

    #[test]
    fn mods_gate_correctly() {
        let c = parse_combo("Ctrl+D").unwrap();
        let mut held = HashSet::new();
        held.insert(KeyCode::KEY_LEFTCTRL);
        assert!(mods_ok(&c, &held));
        held.clear();
        assert!(!mods_ok(&c, &held));
    }

    /// Rescans must merge, never replace: no path is ever opened
    /// twice (dup fds would split one device's event stream), and a
    /// merge is idempotent. Counts are not asserted: other tests
    /// create virtual keyboards concurrently, so the device set
    /// churns mid-suite by design.
    #[test]
    fn rescan_merge_keeps_fds() {
        use std::collections::HashSet;
        let mut live = Vec::new();
        for _ in 0..3 {
            let (found, _) = open_keyboards();
            merge_devices(&mut live, found);
            let paths: HashSet<_> = live.iter().map(|(p, _)| p).collect();
            assert_eq!(paths.len(), live.len(), "merge must never duplicate an fd");
        }
        assert!(!live.is_empty(), "needs a real keyboard to test against");
    }

    #[test]
    fn evdev_codes_roundtrip_through_parser() {
        // The user's F13 key: evdev 183 must survive to "F13".
        assert_eq!(token_for_evdev_code(183), Some("F13"));
        assert_eq!(token_for_evdev_code(30), Some("A"));
        assert_eq!(token_for_evdev_code(57), Some("Space"));
        assert_eq!(token_for_evdev_code(1), Some("Escape"));
        assert_eq!(token_for_evdev_code(9999), None);
        // Modifier codes stay out (handled as modifiers, not keys).
        assert_eq!(token_for_evdev_code(29), None);
        for code in [183u16, 30, 57, 1, 28, 48, 16] {
            let token = token_for_evdev_code(code).unwrap();
            assert!(
                parse_combo(token).is_some(),
                "roundtrip failed for code {code}"
            );
        }
    }

    /// End-to-end through a virtual uinput keyboard: press/release F13
    /// must surface as Down/Up. Needs /dev/uinput write access (the
    /// `input` group has it on this machine).
    #[test]
    fn virtual_keyboard_fires_down_up() {
        use evdev::uinput::VirtualDevice;
        use std::sync::Mutex;

        let mut keys = evdev::AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::KEY_A); // keyboard filter requires it
        keys.insert(KeyCode::KEY_F13);
        let mut virt = VirtualDevice::builder()
            .unwrap()
            .name("mywhisper-hotkey-test")
            .with_keys(&keys)
            .unwrap()
            .build()
            .unwrap();
        // Let udev settle so the device node exists before we start.
        std::thread::sleep(Duration::from_millis(500));

        let seen: Arc<Mutex<Vec<HotkeyEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_cb = seen.clone();
        let mut bindings = HashMap::new();
        bindings.insert(
            HotkeyAction::DictationToggle,
            vec![crate::settings::HotkeyBinding {
                combo: "F13".to_string(),
                mode: crate::settings::ActivationMode::Push,
            }],
        );
        start(
            bindings,
            Arc::new(move |ev| {
                seen_cb.lock().unwrap().push(ev);
            }),
            Arc::new(|_| {}),
        );
        std::thread::sleep(Duration::from_millis(500));

        let syn = evdev::InputEvent::new(0, 0, 0); // EV_SYN / SYN_REPORT
        let press = evdev::InputEvent::new(EventType::KEY.0, KeyCode::KEY_F13.0, 1);
        let release = evdev::InputEvent::new(EventType::KEY.0, KeyCode::KEY_F13.0, 0);
        virt.emit(&[press, syn]).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        virt.emit(&[release, syn]).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        stop();

        let seen = seen.lock().unwrap();
        assert!(
            seen.contains(&HotkeyEvent::Down(
                HotkeyAction::DictationToggle,
                crate::settings::ActivationMode::Push
            )),
            "no Down, got {seen:?}"
        );
        assert!(
            seen.contains(&HotkeyEvent::Up(
                HotkeyAction::DictationToggle,
                crate::settings::ActivationMode::Push
            )),
            "no Up, got {seen:?}"
        );
    }
}

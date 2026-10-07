//! Clipboard write + paste injection for dictation output.
//!
//! Copy side: `wl-copy` (`wl-clipboard` package), with `--sensitive`
//! when hiding from clipboard history. Paste side: `wtype`
//! (`wtype` package, virtual-keyboard protocol) or `ydotool`
//! (`ydotool` package + running `ydotoold` + input group). The chord
//! is configurable: Ctrl+Shift+V (default), Ctrl+V, Shift+Insert.

use crate::settings::{CopyBackend, PasteBackend, PasteCombo, Preferences};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

fn have(binary: &str) -> bool {
    Command::new("which")
        .arg(binary)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Dependency strings for the settings UI, per backend.
pub fn copy_backend_deps() -> &'static str {
    "wl-copy (wl-clipboard package)"
}

pub fn paste_backend_deps(backend: PasteBackend) -> &'static str {
    match backend {
        PasteBackend::Wtype => "wtype (wtype package; Wayland virtual-keyboard protocol)",
        PasteBackend::Ydotool => {
            "ydotool (ydotool package) + running ydotoold service + input group"
        }
    }
}

/// What's installed right now, for status lines in the UI.
#[derive(Debug, Clone, Copy)]
pub struct BackendStatus {
    pub wl_copy: bool,
    pub wtype: bool,
    pub ydotool: bool,
    pub ydotoold_socket: bool,
}

pub fn probe() -> BackendStatus {
    let socket = std::env::var("XDG_RUNTIME_DIR")
        .map(|d| std::path::PathBuf::from(format!("{d}/.ydotool_socket")))
        .map(|p| p.exists())
        .unwrap_or(false);
    BackendStatus {
        wl_copy: have("wl-copy"),
        wtype: have("wtype"),
        ydotool: have("ydotool"),
        ydotoold_socket: socket,
    }
}

/// Current clipboard content with its MIME type. Text and binary
/// (e.g. images) both survive the pipeline; anything beyond one
/// MIME type degrades to text/plain-or-first (wl-copy serves one
/// type per process, so multi-type offers can't be fully rebuilt).
#[derive(Debug, Clone)]
pub enum SavedClipboard {
    Text(String),
    Raw { mime: String, bytes: Vec<u8> },
}

/// Read helper with a timeout so a misbehaving selection owner
/// can't stall dictation.
fn paste_bytes(mime: Option<&str>) -> Option<Vec<u8>> {
    if !have("wl-paste") {
        return None;
    }
    let mut cmd = Command::new("timeout");
    cmd.args(["5", "wl-paste", "--no-newline"]);
    if let Some(m) = mime {
        cmd.args(["--type", m]);
    }
    let out = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    Some(out.stdout)
}

fn offered_types() -> Vec<String> {
    let out = Command::new("timeout")
        .args(["5", "wl-paste", "--list-types"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Snapshot the current clipboard (text or binary) for later restore.
pub fn save_clipboard() -> Option<SavedClipboard> {
    let types = offered_types();
    if types.is_empty() {
        return None;
    }
    // Prefer text/plain when offered: cheapest and exact.
    if types.iter().any(|t| t == "text/plain") {
        return paste_bytes(Some("text/plain"))
            .and_then(|b| String::from_utf8(b).ok())
            .map(SavedClipboard::Text);
    }
    // Single non-text offer (e.g. image/png): keep raw bytes.
    let mime = types.into_iter().next()?;
    paste_bytes(Some(&mime)).map(|bytes| SavedClipboard::Raw { mime, bytes })
}

/// Put a snapshot back. Restored content is never marked sensitive:
/// it was the user's normal clipboard before we borrowed it.
pub fn restore_clipboard(saved: &SavedClipboard) -> Result<(), String> {
    match saved {
        SavedClipboard::Text(t) => copy_with(CopyBackend::WlCopy, t, false),
        SavedClipboard::Raw { mime, bytes } => {
            if !have("wl-copy") {
                return Err("wl-copy not found (install wl-clipboard)".to_string());
            }
            let mut child = Command::new("wl-copy")
                .args(["--type", mime, "--"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("spawn wl-copy: {e}"))?;
            child
                .stdin
                .take()
                .ok_or_else(|| "wl-copy stdin unavailable".to_string())?
                .write_all(bytes)
                .map_err(|e| format!("write clipboard: {e}"))?;
            // wl-copy forks a server to hold the clipboard; the
            // direct child exits fast. Reap without a dedicated
            // thread per dictation (see `reap`).
            reap(child);
            Ok(())
        }
    }
}

/// Current clipboard text, if any. Used to restore the previous
/// content when the transcription shouldn't stay in the clipboard.
pub fn previous_text() -> Option<String> {
    match save_clipboard() {
        Some(SavedClipboard::Text(t)) => Some(t),
        _ => None,
    }
}

/// Write text to the Wayland clipboard.
pub fn copy_text(text: &str, hide_from_history: bool) -> Result<(), String> {
    copy_with(CopyBackend::WlCopy, text, hide_from_history)
}

fn copy_with(backend: CopyBackend, text: &str, hide: bool) -> Result<(), String> {
    match backend {
        CopyBackend::WlCopy => {
            if !have("wl-copy") {
                return Err("wl-copy not found (install wl-clipboard)".to_string());
            }
            let mut cmd = Command::new("wl-copy");
            if hide {
                // Sensitive hint: history managers skip such entries.
                cmd.arg("--sensitive");
            }
            cmd.arg("--")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut child = cmd.spawn().map_err(|e| format!("spawn wl-copy: {e}"))?;
            child
                .stdin
                .take()
                .ok_or_else(|| "wl-copy stdin unavailable".to_string())?
                .write_all(text.as_bytes())
                .map_err(|e| format!("write clipboard: {e}"))?;
            // wl-copy forks a server to hold the clipboard; the
            // direct child exits fast. Reap without a dedicated
            // thread per dictation (see `reap`).
            reap(child);
            Ok(())
        }
    }
}

/// Reap a short-lived child. Pooled normally; a saturated system
/// leaks one zombie (logged) instead of panicking.
fn reap(child: std::process::Child) {
    let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(child)));
    let s = slot.clone();
    if crate::runtime::submit("reap", move || {
        if let Ok(mut g) = s.lock() {
            if let Some(mut c) = g.take() {
                let _ = c.wait();
            }
        }
    })
    .is_err()
    {
        eprintln!("[clipboard] saturated; leaking one wl-copy zombie");
    }
}

/// Inject the configured paste chord into the focused window.
pub fn paste(backend: PasteBackend, combo: PasteCombo) -> Result<(), String> {
    match backend {
        PasteBackend::Wtype => {
            if !have("wtype") {
                return Err("wtype not found (install wtype)".to_string());
            }
            let args: &[&str] = match combo {
                PasteCombo::CtrlShiftV => &[
                    "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl",
                ],
                PasteCombo::CtrlV => &["-M", "ctrl", "-k", "v", "-m", "ctrl"],
                PasteCombo::ShiftInsert => &["-M", "shift", "-k", "Insert", "-m", "shift"],
            };
            Command::new("wtype")
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| format!("spawn wtype: {e}"))
                .and_then(|s| {
                    if s.success() {
                        Ok(())
                    } else {
                        Err("wtype failed".to_string())
                    }
                })
        }
        PasteBackend::Ydotool => {
            if !have("ydotool") {
                return Err("ydotool not found (install ydotool)".to_string());
            }
            // `ydotool key` takes KEYCODE:STATE pairs (see
            // input-event-codes.h), NOT names: 29 ctrl, 42 shift,
            // 47 v, 110 insert. Unknown tokens are silently
            // swallowed (exit 0, nothing injected), so keep these
            // numeric.
            let seq = match combo {
                PasteCombo::CtrlShiftV => "29:1 42:1 47:1 47:0 42:0 29:0",
                PasteCombo::CtrlV => "29:1 47:1 47:0 29:0",
                PasteCombo::ShiftInsert => "42:1 110:1 110:0 42:0",
            };
            let mut args = vec!["key"];
            args.extend(seq.split(' '));
            Command::new("ydotool")
                .args(&args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| format!("spawn ydotool: {e} (is ydotoold running?)"))
                .and_then(|s| {
                    if s.success() {
                        Ok(())
                    } else {
                        Err("ydotool failed (is ydotoold running?)".to_string())
                    }
                })
        }
    }
}

/// Full output pipeline for one transcription. Matrix:
/// - auto-paste off + keep off: clipboard untouched, nothing to do.
/// - auto-paste off + keep on: copy only, transcription stays.
/// - auto-paste on + keep on: copy, inject chord, transcription stays.
/// - auto-paste on + keep off: save previous clipboard, copy,
///   inject chord, restore previous (target app already consumed
///   the paste). Never fails dictation, only reports.
pub fn run_pipeline(text: &str, prefs: &Preferences) {
    if !prefs.auto_paste && !prefs.keep_transcription_in_clipboard {
        return;
    }
    let restore = if prefs.keep_transcription_in_clipboard {
        None
    } else {
        save_clipboard()
    };
    if let Err(e) = copy_text(text, prefs.hide_from_history) {
        eprintln!("[clipboard] copy failed: {e}");
        return;
    }
    eprintln!("[clipboard] copied ({} chars)", text.len());
    if !prefs.auto_paste {
        return;
    }
    // Let the hotkey release settle so its modifiers don't leak into
    // the injected chord.
    std::thread::sleep(Duration::from_millis(120));
    if let Err(e) = paste(prefs.paste_backend, prefs.paste_combo) {
        eprintln!("[clipboard] paste failed: {e} (text stays in clipboard)");
        return;
    }
    eprintln!("[clipboard] pasted via {:?}", prefs.paste_backend);
    if let Some(prev) = restore {
        // Target consumed the paste; give the user their clipboard back.
        std::thread::sleep(Duration::from_millis(250));
        if let Err(e) = restore_clipboard(&prev) {
            eprintln!("[clipboard] restore failed: {e}");
        } else {
            eprintln!("[clipboard] previous clipboard restored");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_dep_strings_present() {
        assert!(copy_backend_deps().contains("wl-copy"));
        assert!(paste_backend_deps(PasteBackend::Wtype).contains("wtype"));
        assert!(paste_backend_deps(PasteBackend::Ydotool).contains("ydotool"));
    }
}

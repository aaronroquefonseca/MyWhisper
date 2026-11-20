//! Clipboard write + paste injection for dictation output.
//!
//! Copy side: `wl-copy` (`wl-clipboard` package), with `--sensitive`
//! when hiding from clipboard history, or `xclip` (X11 CLIPBOARD
//! selection). Paste side: `wtype` (`wtype` package, virtual-keyboard
//! protocol), `ydotool` (`ydotool` package + running `ydotoold` +
//! input group) or `xdotool` (X11 XTEST). The chord is configurable:
//! Ctrl+Shift+V (default), Ctrl+V, Shift+Insert.

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
pub fn copy_backend_deps(backend: CopyBackend) -> &'static str {
    match backend {
        CopyBackend::WlCopy => "wl-copy (wl-clipboard package)",
        CopyBackend::Xclip => "xclip (xclip package; X11 selection, no history hiding)",
    }
}

pub fn paste_backend_deps(backend: PasteBackend) -> &'static str {
    match backend {
        PasteBackend::Wtype => "wtype (wtype package; Wayland virtual-keyboard protocol)",
        PasteBackend::Ydotool => {
            "ydotool (ydotool package) + running ydotoold service + input group"
        }
        PasteBackend::Xdotool => "xdotool (xdotool package; X11 sessions or XWayland windows)",
    }
}

/// What's installed right now, for status lines in the UI.
#[derive(Debug, Clone, Copy)]
pub struct BackendStatus {
    pub wl_copy: bool,
    pub xclip: bool,
    pub wtype: bool,
    pub ydotool: bool,
    pub ydotoold_socket: bool,
    pub xdotool: bool,
}

pub fn probe() -> BackendStatus {
    let socket = std::env::var("XDG_RUNTIME_DIR")
        .map(|d| std::path::PathBuf::from(format!("{d}/.ydotool_socket")))
        .map(|p| p.exists())
        .unwrap_or(false);
    BackendStatus {
        wl_copy: have("wl-copy"),
        xclip: have("xclip"),
        wtype: have("wtype"),
        ydotool: have("ydotool"),
        ydotoold_socket: socket,
        xdotool: have("xdotool"),
    }
}

/// One CLI call: program, args, stdin → stdout on success. Injected so
/// tests drive the pipeline with fakes instead of the real clipboard.
type Run<'a> = &'a mut dyn FnMut(&str, &[&str], Option<&[u8]>) -> Result<Vec<u8>, String>;

/// Real runner: bounded by `timeout` so a misbehaving selection owner
/// or input daemon can't stall dictation; fails unless the command
/// exits 0 (wl-copy forks its server only after taking the selection).
fn exec(program: &str, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>, String> {
    if !have(program) {
        let pkg = match program {
            "wl-copy" | "wl-paste" => "wl-clipboard",
            other => other,
        };
        return Err(format!("{program} not found (install {pkg})"));
    }
    let mut cmd = Command::new("timeout");
    cmd.arg("5").arg(program).args(args).stderr(Stdio::null());
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    // Only readers print; a piped stdout would also be inherited by
    // wl-copy's/xclip's forked server and block the read until it exits.
    let reads = program == "wl-paste" || (program == "xclip" && args.contains(&"-o"));
    cmd.stdout(if reads { Stdio::piped() } else { Stdio::null() });
    let mut child = cmd.spawn().map_err(|e| format!("spawn {program}: {e}"))?;
    // Pipe dropped at statement end → EOF; the child is reaped even if
    // the write failed.
    let wrote = match (stdin, child.stdin.take()) {
        (Some(bytes), Some(mut pipe)) => pipe
            .write_all(bytes)
            .map_err(|e| format!("write {program}: {e}")),
        (Some(_), None) => Err(format!("{program} stdin unavailable")),
        (None, _) => Ok(()),
    };
    let out = child
        .wait_with_output()
        .map_err(|e| format!("wait {program}: {e}"))?;
    wrote?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!("{program} failed ({})", out.status))
    }
}

/// MIME a password manager (or `wl-copy --sensitive`) adds so history
/// managers skip the entry.
const SENSITIVE_HINT: &str = "x-kde-passwordManagerHint";

/// Text offers in preference order (XWayland apps use the X atoms).
const TEXT_TYPES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

/// Clipboard content saved for restore. `mime: None` = text (wl-copy
/// re-offers every text alias). wl-copy serves one type per process,
/// so a multi-type offer keeps only its text (or first real) type.
#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    mime: Option<String>,
    bytes: Vec<u8>,
    sensitive: bool,
}

fn paste_type(run: Run, backend: CopyBackend, mime: &str) -> Option<Vec<u8>> {
    match backend {
        CopyBackend::WlCopy => run("wl-paste", &["--no-newline", "--type", mime], None),
        CopyBackend::Xclip => run(
            "xclip",
            &["-selection", "clipboard", "-o", "-t", mime],
            None,
        ),
    }
    .ok()
    .filter(|b| !b.is_empty())
}

/// Snapshot the current clipboard. `None` = empty or unreadable; the
/// restore then clears instead of leaving the transcript behind.
fn snapshot(run: Run, backend: CopyBackend) -> Option<Snapshot> {
    let listed = match backend {
        CopyBackend::WlCopy => run("wl-paste", &["--list-types"], None),
        CopyBackend::Xclip => run(
            "xclip",
            &["-selection", "clipboard", "-o", "-t", "TARGETS"],
            None,
        ),
    }
    .ok()?;
    let types: Vec<String> = String::from_utf8_lossy(&listed)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let sensitive = types.iter().any(|t| t == SENSITIVE_HINT);
    if let Some(t) = TEXT_TYPES.iter().find(|t| types.iter().any(|o| o == *t)) {
        let bytes = paste_type(run, backend, t)?;
        return Some(Snapshot {
            mime: None,
            bytes,
            sensitive,
        });
    }
    let mime = types
        .into_iter()
        .find(|t| t.contains('/') && t != SENSITIVE_HINT)?;
    let bytes = paste_type(run, backend, &mime)?;
    Some(Snapshot {
        mime: Some(mime),
        bytes,
        sensitive,
    })
}

fn set_clipboard(
    run: Run,
    backend: CopyBackend,
    mime: Option<&str>,
    bytes: &[u8],
    sensitive: bool,
) -> Result<(), String> {
    if backend == CopyBackend::Xclip {
        // xclip serves one target per process, so it cannot add the
        // sensitive hint: history hiding is wl-copy only.
        let mut args = vec!["-selection", "clipboard"];
        if let Some(m) = mime {
            args.extend(["-t", m]);
        }
        args.push("-i");
        return run("xclip", &args, Some(bytes)).map(|_| ());
    }
    let mut args = Vec::new();
    if sensitive {
        // Sensitive hint: history managers skip such entries.
        args.push("--sensitive");
    }
    if let Some(m) = mime {
        args.extend(["--type", m]);
    }
    args.push("--");
    run("wl-copy", &args, Some(bytes)).map(|_| ())
}

/// Put a snapshot back (sensitive stays sensitive); nothing saved →
/// clear, so the borrowed transcript never lingers.
fn restore(run: Run, backend: CopyBackend, saved: Option<&Snapshot>) -> Result<(), String> {
    match (saved, backend) {
        (Some(s), _) => set_clipboard(run, backend, s.mime.as_deref(), &s.bytes, s.sensitive),
        (None, CopyBackend::WlCopy) => run("wl-copy", &["--clear"], None).map(|_| ()),
        // xclip has no clear: own the selection with empty text instead.
        (None, CopyBackend::Xclip) => set_clipboard(run, backend, None, b"", false),
    }
}

/// Write text to the clipboard; Ok only once the backend owns it.
pub fn copy_text(text: &str, hide_from_history: bool, backend: CopyBackend) -> Result<(), String> {
    set_clipboard(&mut exec, backend, None, text.as_bytes(), hide_from_history)
}

/// Inject the configured paste chord into the focused window.
fn paste(run: Run, backend: PasteBackend, combo: PasteCombo) -> Result<(), String> {
    match backend {
        PasteBackend::Wtype => {
            let args: &[&str] = match combo {
                PasteCombo::CtrlShiftV => &[
                    "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl",
                ],
                PasteCombo::CtrlV => &["-M", "ctrl", "-k", "v", "-m", "ctrl"],
                PasteCombo::ShiftInsert => &["-M", "shift", "-k", "Insert", "-m", "shift"],
            };
            run("wtype", args, None).map(|_| ())
        }
        PasteBackend::Ydotool => {
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
            run("ydotool", &args, None)
                .map(|_| ())
                .map_err(|e| format!("{e} (is ydotoold running?)"))
        }
        PasteBackend::Xdotool => {
            let chord = match combo {
                PasteCombo::CtrlShiftV => "ctrl+shift+v",
                PasteCombo::CtrlV => "ctrl+v",
                PasteCombo::ShiftInsert => "shift+Insert",
            };
            // --clearmodifiers: a still-held hotkey modifier must not
            // leak into the chord.
            run("xdotool", &["key", "--clearmodifiers", chord], None).map(|_| ())
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
///
/// PRIMARY is never written, so there is nothing to restore there.
pub fn run_pipeline(text: &str, prefs: &Preferences) {
    match deliver_with(&mut exec, text, prefs) {
        Ok(msg) => eprintln!("[clipboard] {msg}"),
        Err(e) => eprintln!("[clipboard] {e}"),
    }
}

fn deliver_with(run: Run, text: &str, prefs: &Preferences) -> Result<&'static str, String> {
    if !prefs.auto_paste && !prefs.keep_transcription_in_clipboard {
        return Ok("nothing to do");
    }
    let borrowed = !prefs.keep_transcription_in_clipboard;
    let backend = prefs.copy_backend;
    let saved = if borrowed {
        snapshot(run, backend)
    } else {
        None
    };
    // A borrowed transcript is transient: keep it out of history even
    // when the user lets kept transcripts in.
    set_clipboard(
        run,
        backend,
        None,
        text.as_bytes(),
        prefs.hide_from_history || borrowed,
    )
    .map_err(|e| format!("copy failed: {e}"))?;
    eprintln!("[clipboard] copied ({} chars)", text.len());
    if !prefs.auto_paste {
        return Ok("kept in clipboard");
    }
    // Let the hotkey release settle so its modifiers don't leak into
    // the injected chord.
    std::thread::sleep(Duration::from_millis(120));
    paste(run, prefs.paste_backend, prefs.paste_combo)
        .map_err(|e| format!("paste failed: {e} (text stays in clipboard)"))?;
    eprintln!("[clipboard] pasted via {:?}", prefs.paste_backend);
    if !borrowed {
        return Ok("transcription kept in clipboard");
    }
    // shortcut: a fixed delay, not proof the target read the paste;
    // a consumption signal needs our own data-control client.
    std::thread::sleep(Duration::from_millis(250));
    // Restore only while we still own the clipboard: a newer copy wins.
    // shortcut: check-then-set, a copy inside that window is overwritten.
    let own_type = match backend {
        CopyBackend::WlCopy => "text/plain",
        CopyBackend::Xclip => "UTF8_STRING",
    };
    if paste_type(run, backend, own_type).as_deref() != Some(text.as_bytes()) {
        return Ok("clipboard changed since paste; not restoring");
    }
    restore(run, backend, saved.as_ref()).map_err(|e| format!("restore failed: {e}"))?;
    Ok("previous clipboard restored")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_dep_strings_present() {
        assert!(copy_backend_deps(CopyBackend::WlCopy).contains("wl-copy"));
        assert!(copy_backend_deps(CopyBackend::Xclip).contains("xclip"));
        assert!(paste_backend_deps(PasteBackend::Wtype).contains("wtype"));
        assert!(paste_backend_deps(PasteBackend::Ydotool).contains("ydotool"));
        assert!(paste_backend_deps(PasteBackend::Xdotool).contains("xdotool"));
    }

    /// In-memory clipboard standing in for wl-copy/wl-paste/wtype.
    #[derive(Default)]
    struct Fake {
        types: Vec<String>,
        bytes: Vec<u8>,
        calls: Vec<String>,
        fail: Option<&'static str>,
        /// User copy landing while the chord is injected.
        on_paste: Option<(Vec<String>, Vec<u8>)>,
    }

    impl Fake {
        fn holding(types: &[&str], bytes: &[u8]) -> Self {
            Self {
                types: types.iter().map(|t| t.to_string()).collect(),
                bytes: bytes.to_vec(),
                ..Self::default()
            }
        }

        fn run(
            &mut self,
            program: &str,
            args: &[&str],
            stdin: Option<&[u8]>,
        ) -> Result<Vec<u8>, String> {
            self.calls.push(format!("{program} {}", args.join(" ")));
            if self.fail == Some(program) {
                return Err(format!("{program} failed"));
            }
            match (program, args) {
                ("wl-paste", ["--list-types"]) if self.types.is_empty() => Err("empty".into()),
                ("wl-paste", ["--list-types"]) => Ok(self.types.join("\n").into_bytes()),
                ("wl-paste", [.., t]) if self.types.iter().any(|o| o == t) => {
                    Ok(self.bytes.clone())
                }
                ("wl-paste", _) => Err("type not offered".into()),
                ("wl-copy", ["--clear"]) => {
                    self.types.clear();
                    self.bytes.clear();
                    Ok(Vec::new())
                }
                ("xclip", [.., "-o", "-t", "TARGETS"]) if self.types.is_empty() => {
                    Err("empty".into())
                }
                ("xclip", [.., "-o", "-t", "TARGETS"]) => Ok(self.types.join("\n").into_bytes()),
                ("xclip", [.., "-o", "-t", t]) if self.types.iter().any(|o| o == t) => {
                    Ok(self.bytes.clone())
                }
                ("xclip", _) if args.contains(&"-o") => Err("type not offered".into()),
                ("xclip", _) => {
                    self.types = match args.iter().position(|a| *a == "-t") {
                        Some(i) => vec![args[i + 1].to_string()],
                        None => vec!["UTF8_STRING".into(), "STRING".into()],
                    };
                    self.bytes = stdin.expect("xclip -i reads stdin").to_vec();
                    Ok(Vec::new())
                }
                ("wl-copy", _) => {
                    self.types = match args.iter().position(|a| *a == "--type") {
                        Some(i) => vec![args[i + 1].to_string()],
                        None => vec!["text/plain;charset=utf-8".into(), "text/plain".into()],
                    };
                    if args.contains(&"--sensitive") {
                        self.types.push(SENSITIVE_HINT.into());
                    }
                    self.bytes = stdin.expect("wl-copy reads stdin").to_vec();
                    Ok(Vec::new())
                }
                _ => {
                    if let Some((types, bytes)) = self.on_paste.take() {
                        self.types = types;
                        self.bytes = bytes;
                    }
                    Ok(Vec::new())
                }
            }
        }

        fn deliver(&mut self, prefs: &Preferences) -> Result<&'static str, String> {
            deliver_with(&mut |p, a, s| self.run(p, a, s), "dictated", prefs)
        }

        fn pasted(&self) -> bool {
            self.calls.iter().any(|c| c.starts_with("wtype"))
        }
    }

    fn borrowed(hide_from_history: bool) -> Preferences {
        Preferences {
            hide_from_history,
            ..Preferences::default()
        }
    }

    #[test]
    fn failed_copy_never_pastes_the_old_clipboard() {
        let mut fake = Fake::holding(&["text/plain"], b"old");
        fake.fail = Some("wl-copy");
        let err = fake.deliver(&borrowed(true)).unwrap_err();
        assert!(err.starts_with("copy failed"), "{err}");
        assert!(!fake.pasted());
        assert_eq!(fake.bytes, b"old");
    }

    #[test]
    fn failed_paste_reports_error() {
        let mut fake = Fake::holding(&["text/plain"], b"old");
        fake.fail = Some("wtype");
        assert!(
            fake.deliver(&borrowed(true))
                .unwrap_err()
                .starts_with("paste failed")
        );
    }

    #[test]
    fn borrowed_transcript_is_sensitive_and_text_restored() {
        let mut fake = Fake::holding(&["text/html", "UTF8_STRING"], b"old");
        assert_eq!(
            fake.deliver(&borrowed(false)),
            Ok("previous clipboard restored")
        );
        // Copied with the hint even though history hiding is off.
        assert!(fake.calls.contains(&"wl-copy --sensitive --".to_string()));
        assert_eq!(fake.bytes, b"old");
        assert!(fake.types.contains(&"text/plain".to_string()));
        assert!(!fake.types.contains(&SENSITIVE_HINT.to_string()));
    }

    #[test]
    fn empty_clipboard_is_cleared_not_left_with_transcript() {
        let mut fake = Fake::default();
        assert_eq!(
            fake.deliver(&borrowed(true)),
            Ok("previous clipboard restored")
        );
        assert!(fake.calls.contains(&"wl-copy --clear".to_string()));
        assert!(fake.types.is_empty() && fake.bytes.is_empty());
    }

    #[test]
    fn binary_and_sensitive_offers_restore_as_they_were() {
        let mut fake = Fake::holding(&["image/png", SENSITIVE_HINT], b"\x89PNG");
        assert_eq!(
            fake.deliver(&borrowed(true)),
            Ok("previous clipboard restored")
        );
        assert_eq!(fake.types, ["image/png", SENSITIVE_HINT]);
        assert_eq!(fake.bytes, b"\x89PNG");
    }

    #[test]
    fn user_copy_during_delivery_is_not_overwritten() {
        let mut fake = Fake::holding(&["text/plain"], b"old");
        fake.on_paste = Some((vec!["text/plain".into()], b"newer".to_vec()));
        assert_eq!(
            fake.deliver(&borrowed(true)),
            Ok("clipboard changed since paste; not restoring")
        );
        assert_eq!(fake.bytes, b"newer");
    }

    #[test]
    fn x11_backends_paste_and_restore_the_previous_clipboard() {
        let prefs = Preferences {
            copy_backend: CopyBackend::Xclip,
            paste_backend: PasteBackend::Xdotool,
            ..Preferences::default()
        };
        let mut fake = Fake::holding(&["TARGETS", "UTF8_STRING"], b"old");
        assert_eq!(fake.deliver(&prefs), Ok("previous clipboard restored"));
        assert!(
            fake.calls
                .contains(&"xclip -selection clipboard -i".to_string())
        );
        assert!(
            fake.calls
                .contains(&"xdotool key --clearmodifiers ctrl+shift+v".to_string())
        );
        assert!(!fake.calls.iter().any(|c| c.starts_with("wl-")));
        assert_eq!(fake.bytes, b"old");
    }

    #[test]
    fn kept_transcript_follows_history_setting_and_stays() {
        let prefs = Preferences {
            keep_transcription_in_clipboard: true,
            hide_from_history: false,
            ..Preferences::default()
        };
        let mut fake = Fake::holding(&["text/plain"], b"old");
        assert_eq!(fake.deliver(&prefs), Ok("transcription kept in clipboard"));
        assert!(fake.calls.contains(&"wl-copy --".to_string()));
        assert_eq!(fake.bytes, b"dictated");
        assert!(!fake.calls.iter().any(|c| c.contains("--list-types")));
    }
}

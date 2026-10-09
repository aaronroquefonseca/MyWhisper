//! Temporary update check: once per GUI launch, ask `gh` for the
//! newest private-release tag and remember it when newer than this
//! build. Surfaced on the Home banner. Silent on any failure (no
//! gh, offline, bad tag). No auto-install: updates stay an explicit
//! `update.sh` run.

use std::sync::{Mutex, OnceLock};

static LATEST: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<String>> {
    LATEST.get_or_init(|| Mutex::new(None))
}

fn parse_ver(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let mut it = v.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next().unwrap_or("0").parse().ok()?;
    let pat = it.next().unwrap_or("0").parse().ok()?;
    Some((maj, min, pat))
}

/// Fire-and-forget check (GUI startup only, never the service).
pub fn check_for_updates() {
    std::thread::spawn(|| {
        let out = std::process::Command::new("gh")
            .args([
                "release",
                "list",
                "--repo",
                "aaronroquefonseca/MyWhisper",
                "--limit",
                "1",
                "--json",
                "tagName",
                "-q",
                ".[0].tagName",
            ])
            .output();
        let Ok(out) = out else { return };
        if !out.status.success() {
            return;
        }
        let tag = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let (Some(latest), Some(current)) = (parse_ver(&tag), parse_ver(env!("CARGO_PKG_VERSION")))
        else {
            return;
        };
        if latest > current {
            *slot().lock().unwrap() = Some(format!("v{}.{}.{}", latest.0, latest.1, latest.2));
            eprintln!("[update] newer release available: {tag}");
        }
    });
}

/// Newer release tag (e.g. "v0.9.1"), if the startup check found one.
pub fn available_update() -> Option<String> {
    slot().lock().unwrap().clone()
}

//! Read-only private-release checks. Stable tags are compared numerically;
//! drafts/prereleases and malformed tags never become automatic update offers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

static STATE: OnceLock<Mutex<Option<Option<String>>>> = OnceLock::new();
static CHECK_EPOCH: AtomicU64 = AtomicU64::new(0);
fn slot() -> &'static Mutex<Option<Option<String>>> {
    STATE.get_or_init(|| Mutex::new(None))
}

fn parse_ver(tag: &str) -> Option<(u64, u64, u64)> {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    let parts = version.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    let mut numbers = [0; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty()
            || part.len() > 20
            || !part.bytes().all(|b| b.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return None;
        }
        numbers[index] = part.parse().ok()?;
    }
    Some((numbers[0], numbers[1], numbers[2]))
}

fn newest_stable(bytes: &[u8]) -> Option<(u64, u64, u64)> {
    let releases: Vec<serde_json::Value> = serde_json::from_slice(bytes).ok()?;
    releases
        .iter()
        .filter(|release| {
            release["isDraft"].as_bool() == Some(false)
                && release["isPrerelease"].as_bool() == Some(false)
        })
        .filter_map(|release| parse_ver(release["tagName"].as_str()?))
        .max()
}

/// Bounded subprocess/queue ownership; an older check cannot replace a newer one.
pub fn check_for_updates() {
    let epoch = CHECK_EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
    *slot().lock().unwrap_or_else(|p| p.into_inner()) = None;
    if let Err(error) = crate::runtime::submit("update-check", move || {
        let mut command = std::process::Command::new("gh");
        command.args([
            "release",
            "list",
            "--repo",
            "aaronroquefonseca/MyWhisper",
            "--limit",
            "100",
            "--exclude-drafts",
            "--exclude-pre-releases",
            "--json",
            "tagName,isDraft,isPrerelease",
        ]);
        let Ok(bytes) = crate::child::run(
            &mut command,
            &[],
            std::time::Duration::from_secs(10),
            1024 * 1024,
        ) else {
            return;
        };
        let (Some(latest), Some(current)) =
            (newest_stable(&bytes), parse_ver(env!("CARGO_PKG_VERSION")))
        else {
            return;
        };
        if CHECK_EPOCH.load(Ordering::Acquire) != epoch {
            return;
        }
        let offer = (latest > current).then(|| format!("v{}.{}.{}", latest.0, latest.1, latest.2));
        *slot().lock().unwrap_or_else(|p| p.into_inner()) = Some(offer);
    }) {
        eprintln!("[update] check admission failed: {error}");
    }
}

pub fn available_update() -> Option<String> {
    slot()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .flatten()
}

pub enum UpdateState {
    Unknown,
    Current,
    Newer(String),
}

pub fn update_state() -> UpdateState {
    match slot().lock().unwrap_or_else(|p| p.into_inner()).clone() {
        None => UpdateState::Unknown,
        Some(None) => UpdateState::Current,
        Some(Some(version)) => UpdateState::Newer(version),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_selection_ignores_publication_order_prereleases_drafts_and_bad_tags() {
        let releases = br#"[
            {"tagName":"v9.0.0-rc.1","isDraft":false,"isPrerelease":true},
            {"tagName":"v8.0.0","isDraft":true,"isPrerelease":false},
            {"tagName":"v1.1.0","isDraft":false,"isPrerelease":false},
            {"tagName":"v1.10.0","isDraft":false,"isPrerelease":false},
            {"tagName":"v1.2.0","isDraft":false,"isPrerelease":false},
            {"tagName":"v2.0.0+build","isDraft":false,"isPrerelease":false},
            {"tagName":"v7.0.0"}
        ]"#;
        assert_eq!(newest_stable(releases), Some((1, 10, 0)));
        for bad in [
            "v1.2",
            "vv1.2.3",
            "1.2.3.4",
            "01.2.3",
            " 1.2.3",
            "1.2.3-rc.1",
            "1.2.3+build",
            "١.2.3",
            "18446744073709551616.2.3",
        ] {
            assert!(parse_ver(bad).is_none(), "{bad}");
        }
        assert_eq!(newest_stable(b"malformed"), None);
    }
}

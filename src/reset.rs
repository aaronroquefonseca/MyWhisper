//! Reset the database, then retire the local cache with a durable retry list.
use crate::settings::AppSettings;
use crate::store::{RESET_DIRS, Store};
use std::io;
use std::path::Path;

#[derive(Debug)]
pub(crate) enum Failure {
    Cache(io::Error),
    Database(rusqlite::Error),
}

pub(crate) struct Outcome {
    pub(crate) settings: Option<AppSettings>,
    pub(crate) incomplete: bool,
}

pub(crate) fn run(db: &Path, cleanup_only: bool) -> Result<Outcome, Failure> {
    let data = db
        .parent()
        .filter(|path| !path.as_os_str().is_empty() && db.is_file())
        .ok_or_else(|| Failure::Cache(io::Error::other("reset database unavailable")))?;
    let _cache = crate::stt::try_lock_download_cache(data).map_err(Failure::Cache)?;
    let store = Store::open(db).map_err(Failure::Database)?;
    let mut pending = store.pending_reset_dirs().map_err(Failure::Database)?;
    let settings = if pending.is_empty() && !cleanup_only {
        let mut fresh = AppSettings::default();
        fresh.sync.enabled = false;
        store.reset_all_data(&fresh).map_err(Failure::Database)?;
        pending.extend(RESET_DIRS);
        Some(fresh)
    } else {
        None
    };

    let mut incomplete = false;
    for dir in pending {
        let path = data.join(dir);
        let cleanup = match std::fs::remove_dir_all(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        };
        let result = cleanup
            .and_then(|()| crate::private_fs::directory(&path))
            .and_then(|()| std::fs::File::open(data)?.sync_all())
            .map_err(|e| e.to_string())
            .and_then(|()| store.complete_reset_dir(dir).map_err(|e| e.to_string()));
        if let Err(error) = result {
            incomplete = true;
            tracing::warn!(directory = dir, %error, "reset cleanup incomplete");
        }
    }
    Ok(Outcome {
        settings,
        incomplete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_retry_only_removes_pending_directories() {
        let scratch = tempfile::tempdir().unwrap();
        let data = scratch.path();
        let db = data.join("state.db");
        let store = Store::open(&db).unwrap();
        store.add_history("old", "old", 1, 1, "node").unwrap();
        std::fs::write(data.join("models"), b"fixture obstruction").unwrap();
        std::fs::create_dir(data.join("audio")).unwrap();
        std::fs::write(data.join("audio/old.wav"), b"fixture").unwrap();

        let result = run(&db, false).unwrap();
        assert!(result.incomplete);
        assert!(!result.settings.unwrap().sync.enabled);
        assert!(store.load_history(10, false).unwrap().is_empty());
        assert_eq!(store.pending_reset_dirs().unwrap(), ["models"]);
        assert!(!data.join("audio/old.wav").exists());

        store.add_history("new", "new", 1, 1, "node").unwrap();
        std::fs::write(data.join("audio/new.wav"), b"new fixture").unwrap();
        std::fs::remove_file(data.join("models")).unwrap();
        let retry = run(&db, true).unwrap();
        assert!(!retry.incomplete);
        assert!(retry.settings.is_none());
        assert!(store.pending_reset_dirs().unwrap().is_empty());
        assert_eq!(store.load_history(10, false).unwrap().len(), 1);
        assert!(data.join("audio/new.wav").is_file());
        assert!(data.join("models").is_dir());
    }

    #[test]
    fn busy_installer_leaves_history_and_preferences_intact() {
        let scratch = tempfile::tempdir().unwrap();
        let db = scratch.path().join("state.db");
        let store = Store::open(&db).unwrap();
        store.add_history("kept", "kept", 1, 1, "node").unwrap();
        let _lock = crate::stt::try_lock_download_cache(scratch.path()).unwrap();
        assert!(matches!(
            run(&db, false),
            Err(Failure::Cache(error)) if error.kind() == io::ErrorKind::WouldBlock
        ));
        assert!(store.pending_reset_dirs().unwrap().is_empty());
        assert_eq!(store.load_history(10, false).unwrap().len(), 1);
    }
}

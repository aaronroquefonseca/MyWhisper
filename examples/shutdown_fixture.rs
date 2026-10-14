//! Disposable settings/recovery companion for the actual binary shutdown gate.
use mywhisper::{paths::AppPaths, store::Store};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(
        std::env::var_os("MYWHISPER_AUDIT_ROOT").ok_or("disposable root required")?,
    );
    if !root.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .starts_with("mywhisper-shutdown-audit.")
    }) || !root.join(".disposable").is_file()
        || root.metadata()?.permissions().mode() & 0o777 != 0o700
        || root.metadata()?.uid() != unsafe { libc::geteuid() }
    {
        return Err("refusing a non-disposable root".into());
    }
    for key in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
    ] {
        let path = std::path::PathBuf::from(std::env::var_os(key).ok_or("isolated XDG required")?);
        if !path.starts_with(&root)
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err("refusing data outside the disposable root".into());
        }
    }
    let paths = AppPaths::create()?;
    let store = Store::open_with_key(&paths.db_file(), &[0; 32])?;
    match std::env::args().nth(1).as_deref() {
        Some("prepare") => {
            let mut settings = store.load_settings();
            settings.sync.enabled = false;
            settings.sync.advertise = false;
            settings.hotkeys.hotkeys_enabled = false;
            settings.stt.auto_start_server = false;
            store.save_settings(&settings)?;
            store.upsert_word("Synthetic shutdown recovery", "synthetic")?;
        }
        Some("verify") => {
            assert_eq!(store.load_words()?, vec!["Synthetic shutdown recovery"]);
            let settings = store.try_load_settings()?;
            assert!(
                !settings.sync.enabled
                    && !settings.sync.advertise
                    && !settings.hotkeys.hotkeys_enabled
            );
            assert!(!std::fs::read(paths.db_file())?.starts_with(b"SQLite format 3\0"));
        }
        _ => return Err("usage: shutdown_fixture prepare|verify".into()),
    }
    store.checkpoint()?;
    Ok(())
}

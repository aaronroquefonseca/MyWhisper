//! Filesystem locations (XDG), file logging, and node identity.
//!
//! Layout (`mywhisper` app dir, `$XDG_*_HOME` honored with `~` fallback):
//!
//! * state:    `$XDG_DATA_HOME/mywhisper/state.db` (settings, dictionary,
//!   history and sync metadata; active WAL files cannot be copied as a
//!   standalone backup and network sync exchanges authorized records)
//! * identity: `$XDG_CONFIG_HOME/mywhisper/device.key` (ed25519 node key)
//! * logs:     `$XDG_STATE_HOME/mywhisper/mywhisper.log.YYYY-MM-DD`
//!
//! Logs rotate daily and only the 5 most recent files are kept
//! (`tracing-appender` keeps unlimited files unless told otherwise,
//! so this retention is our explicit choice, not its default).

use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};

pub const APP_DIR: &str = "mywhisper";
pub const DB_FILE: &str = "state.db";
pub const DEVICE_KEY_FILE: &str = "device.key";
pub const LOG_PREFIX: &str = "mywhisper";
pub const LOG_KEEP: usize = 5;

pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
}

fn base_dir(env_var: &str, fallback: &str) -> PathBuf {
    if let Some(dir) = std::env::var_os(env_var) {
        PathBuf::from(dir)
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(fallback)
    } else {
        PathBuf::from("/tmp").join(APP_DIR)
    }
}

impl AppPaths {
    /// Resolve (and create) the three app directories.
    pub fn create() -> std::io::Result<Self> {
        let paths = Self {
            config_dir: base_dir("XDG_CONFIG_HOME", ".config").join(APP_DIR),
            data_dir: base_dir("XDG_DATA_HOME", ".local/share").join(APP_DIR),
            state_dir: base_dir("XDG_STATE_HOME", ".local/state").join(APP_DIR),
        };
        crate::private_fs::directory(&paths.config_dir)?;
        crate::private_fs::directory(&paths.data_dir)?;
        crate::private_fs::directory(&paths.state_dir)?;
        Ok(paths)
    }

    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join(DB_FILE)
    }

    pub fn device_key_file(&self) -> PathBuf {
        self.config_dir.join(DEVICE_KEY_FILE)
    }

    /// Engine lockfile: whoever holds an exclusive flock on this is
    /// the single engine owner (hotkeys, sync listener, pill). The
    /// GUI runs frontend-only while another process holds it.
    pub fn engine_lock_file(&self) -> PathBuf {
        self.data_dir.join("engine.lock")
    }
}

/// Install the global `tracing` subscriber writing to the daily-rotated
/// log file. The returned guard flushes on drop: the caller must hold
/// it for the whole process lifetime.
pub fn init_file_logging(state_dir: &std::path::Path) -> WorkerGuard {
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .filename_suffix("log")
        .max_log_files(LOG_KEEP)
        .build(state_dir)
        .expect("open log file");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("set log subscriber");
    guard
}

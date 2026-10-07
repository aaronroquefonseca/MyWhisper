pub mod app;
pub mod app_dictionary;
pub mod app_home;
pub mod app_insights;
pub mod app_settings;
pub mod app_sync;
pub mod audio;
pub mod clipboard;
pub mod device;
pub mod hotkey;
pub mod i18n;
pub mod media;
pub mod mic;
pub mod net;
pub mod overlay;
pub mod paths;
pub mod runtime;

/// Single source for the app version (Cargo.toml package
/// version): settings UI, `--version`, log lines all use this.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub mod service;
pub mod settings;
pub mod sound;
pub mod store;
pub mod stt;
pub mod transcribe;
pub mod viz;

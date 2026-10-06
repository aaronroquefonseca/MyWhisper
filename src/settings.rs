//! Application settings model.
//!
//! Plain data with serde support and opinionated defaults. The UI in
//! `app_settings` initializes its widgets from this model; nothing is
//! wired yet -- persistence, hotkey capture, device probing and all
//! backends come later. Keep every future-tweakable knob here so the
//! UI never grows parallel state.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub preferences: Preferences,
    pub hotkeys: HotkeySettings,
    pub stt: SttSettings,
    pub language_models: LanguageModelSettings,
    pub privacy: PrivacySettings,
    pub system: SystemSettings,
    pub dictionary: Dictionary,
    pub sync: SyncSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            preferences: Preferences::default(),
            hotkeys: HotkeySettings::default(),
            stt: SttSettings::default(),
            language_models: LanguageModelSettings::default(),
            privacy: PrivacySettings::default(),
            system: SystemSettings::default(),
            dictionary: Dictionary::default(),
            sync: SyncSettings::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Sync (node-based device sync, Syncthing-style trust)
// ---------------------------------------------------------------------------

/// Default TCP port for incoming sync connections.
pub const DEFAULT_SYNC_PORT: u16 = 42973;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSettings {
    /// This device's display name shown to peers.
    pub device_name: String,
    /// TCP port for incoming sync connections (0 = disabled).
    pub listen_port: u16,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            device_name: String::new(),
            listen_port: DEFAULT_SYNC_PORT,
        }
    }
}

// ---------------------------------------------------------------------------
// Preferences (general)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FloatingPosition {
    BottomRight,
    BottomCenter,
    BottomLeft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MicSelection {
    System,
    BuiltIn,
    Specific,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    pub theme: Theme,
    pub dictation_sounds: bool,
    pub pause_media_on_dictation: bool,
    pub auto_paste: bool,
    pub keep_transcription_in_clipboard: bool,
    pub floating_icon_auto_hide: bool,
    pub floating_icon_position: FloatingPosition,
    pub ui_language: String,
    pub transcription_language: String,
    pub launch_at_login: bool,
    pub start_minimized: bool,
    pub mic_selection: MicSelection,
    pub mic_device_id: String,
    pub mic_warm_hold_secs: u32,
    pub auto_learn_dictionary: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            dictation_sounds: true,
            pause_media_on_dictation: false,
            auto_paste: true,
            keep_transcription_in_clipboard: false,
            floating_icon_auto_hide: false,
            floating_icon_position: FloatingPosition::BottomRight,
            ui_language: "en".to_string(),
            transcription_language: "auto".to_string(),
            launch_at_login: false,
            start_minimized: false,
            mic_selection: MicSelection::System,
            mic_device_id: String::new(),
            mic_warm_hold_secs: 0,
            auto_learn_dictionary: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Hotkeys (dictation only for now)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivationMode {
    /// Press once to start/stop.
    Tap,
    /// Hold to talk.
    Push,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeySettings {
    /// Human-readable accelerator, e.g. "Super+Alt+D".
    pub dictation_hotkey: String,
    pub activation_mode: ActivationMode,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self {
            dictation_hotkey: "Super+Alt+D".to_string(),
            activation_mode: ActivationMode::Push,
        }
    }
}

// ---------------------------------------------------------------------------
// Speech to text
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SttMode {
    Local,
    Cloud,
    SelfHosted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalSttProvider {
    Whisper,
    Parakeet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttSettings {
    pub mode: SttMode,
    pub local_provider: LocalSttProvider,
    pub whisper_model: String,
    pub parakeet_model: String,
    pub cloud_provider: String,
    pub cloud_model: String,
    pub cloud_api_key: String,
    pub self_hosted_url: String,
    pub self_hosted_model: String,
    pub language: String,
    pub vad_enabled: bool,
    pub vad_threshold: f32,
    pub live_preview: bool,
}

impl Default for SttSettings {
    fn default() -> Self {
        Self {
            mode: SttMode::Local,
            local_provider: LocalSttProvider::Whisper,
            whisper_model: "base".to_string(),
            parakeet_model: "parakeet-tdt-0.6b-v3".to_string(),
            cloud_provider: "OpenAI".to_string(),
            cloud_model: "whisper-1".to_string(),
            cloud_api_key: String::new(),
            self_hosted_url: "http://localhost:8000".to_string(),
            self_hosted_model: String::new(),
            language: "auto".to_string(),
            vad_enabled: true,
            vad_threshold: 0.5,
            live_preview: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Language models (Ollama-first, local-first)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LmMode {
    Ollama,
    Local,
    Cloud,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageModelSettings {
    pub mode: LmMode,
    pub ollama_url: String,
    pub ollama_model: String,
    pub local_model: String,
    pub keep_loaded: bool,
    pub cloud_provider: String,
    pub cloud_model: String,
    pub cloud_api_key: String,
    pub cleanup_enabled: bool,
    pub cleanup_prompt: String,
}

impl Default for LanguageModelSettings {
    fn default() -> Self {
        Self {
            mode: LmMode::Ollama,
            ollama_url: "http://localhost:11434".to_string(),
            ollama_model: "llama3.1".to_string(),
            local_model: String::new(),
            keep_loaded: true,
            cloud_provider: "OpenAI".to_string(),
            cloud_model: "gpt-4o-mini".to_string(),
            cloud_api_key: String::new(),
            cleanup_enabled: true,
            cleanup_prompt: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Privacy & data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacySettings {
    /// Audio retention in days; 0 = disabled.
    pub audio_retention_days: u32,
    pub history_enabled: bool,
    /// Transcript auto-delete in days; 0 = keep forever.
    pub transcript_auto_delete_days: u32,
    pub save_discarded_transcriptions: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            audio_retention_days: 30,
            history_enabled: true,
            transcript_auto_delete_days: 0,
            save_discarded_transcriptions: false,
        }
    }
}

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSettings {
    pub model_cache_path: String,
}

impl Default for SystemSettings {
    fn default() -> Self {
        Self {
            model_cache_path: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Dictionary + snippets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub trigger: String,
    pub replacement: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Dictionary {
    pub words: Vec<String>,
    pub snippets: Vec<Snippet>,
}

// ---------------------------------------------------------------------------
// Runtime insights (computed, not persisted with settings)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct UsageStats {
    pub words_spoken: u64,
    pub words_per_minute: f64,
    pub dictations: u64,
    pub streak_days: u32,
}

#[derive(Debug, Clone, Default)]
pub struct HistoryEntry {
    pub text: String,
    pub date_label: String,
    pub word_count: u32,
    pub duration_secs: i64,
}

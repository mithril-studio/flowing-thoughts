use crate::db;
use rusqlite::Connection;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntry {
    pub session_id: u64,
    pub text: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GeneralSettings {
    pub window_movable: bool,
    pub launch_at_login: bool,
    pub show_in_dock: bool,
    pub window_position: String,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            window_movable: true,
            launch_at_login: false,
            show_in_dock: true,
            window_position: "center".to_string(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ShortcutsSettings {
    pub preset: String,
}

impl Default for ShortcutsSettings {
    fn default() -> Self {
        Self {
            preset: "fn".to_string(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MicrophoneSettings {
    pub input_device: String,
    pub noise_suppression_enabled: bool,
}

impl Default for MicrophoneSettings {
    fn default() -> Self {
        Self {
            input_device: "system_default".to_string(),
            noise_suppression_enabled: false,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LanguageSettings {
    pub mode: String,
}

impl Default for LanguageSettings {
    fn default() -> Self {
        Self {
            mode: "system".to_string(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SoundSettings {
    pub feedback_sounds_enabled: bool,
}

impl Default for SoundSettings {
    fn default() -> Self {
        Self {
            feedback_sounds_enabled: false,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ExtrasSettings {
    pub auto_add_to_dictionary: bool,
    pub smart_formatting: bool,
    pub dangerously_skip_permissions: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TranscriptionSettings {
    pub provider: String,
}

impl Default for TranscriptionSettings {
    fn default() -> Self {
        Self {
            provider: "api".to_string(),
        }
    }
}

impl Default for ExtrasSettings {
    fn default() -> Self {
        Self {
            auto_add_to_dictionary: false,
            smart_formatting: false,
            dangerously_skip_permissions: false,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct AppSettings {
    #[serde(default)]
    pub general: GeneralSettings,
    #[serde(default)]
    pub shortcuts: ShortcutsSettings,
    #[serde(default)]
    pub microphone: MicrophoneSettings,
    #[serde(default)]
    pub language: LanguageSettings,
    #[serde(default)]
    pub sound: SoundSettings,
    #[serde(default)]
    pub extras: ExtrasSettings,
    #[serde(default)]
    pub transcription: TranscriptionSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Groq,
    Openai,
}

impl Default for Provider {
    fn default() -> Self {
        Provider::Groq
    }
}

impl Provider {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "groq" => Some(Provider::Groq),
            "openai" => Some(Provider::Openai),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Groq => "groq",
            Provider::Openai => "openai",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PersistedState {
    pub license_key: Option<String>,
    pub onboarding_complete: bool,
    // Legacy `api_key` field (pre-provider split) is treated as a Groq key.
    #[serde(default, alias = "api_key")]
    pub groq_api_key: Option<String>,
    #[serde(default)]
    pub openai_api_key: Option<String>,
    #[serde(default)]
    pub active_provider: Provider,
    #[serde(default)]
    pub settings: AppSettings,
    pub history: Vec<HistoryEntry>,
}

const HISTORY_UI_CAP: i64 = 200;
const MIGRATION_FLAG_KEY: &str = "migrated_from_state_json";

fn persisted_file_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("FlowingThoughts")
        .join("state.json"))
}

fn logs_file_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("FlowingThoughts")
        .join("logs.txt"))
}

pub fn load(conn: &Connection) -> Result<PersistedState, String> {
    migrate_from_json_if_needed(conn)?;

    let mut state = PersistedState::default();

    if let Some(raw) = db::kv_get(conn, "license_key")? {
        state.license_key = serde_json::from_str(&raw).unwrap_or(None);
    }
    if let Some(raw) = db::kv_get(conn, "openai_api_key")? {
        state.openai_api_key = serde_json::from_str(&raw).unwrap_or(None);
    }
    if let Some(raw) = db::kv_get(conn, "groq_api_key")? {
        state.groq_api_key = serde_json::from_str(&raw).unwrap_or(None);
    }
    if let Some(raw) = db::kv_get(conn, "active_provider")? {
        state.active_provider = Provider::parse(raw.trim_matches('"')).unwrap_or_default();
    }
    if let Some(raw) = db::kv_get(conn, "onboarding_complete")? {
        state.onboarding_complete = raw == "true";
    }
    if let Some(raw) = db::kv_get(conn, "app_settings")? {
        state.settings = serde_json::from_str(&raw).unwrap_or_default();
    }
    state.history = db::list_history(conn, HISTORY_UI_CAP)?;

    Ok(state)
}

pub fn save(conn: &Connection, state: &PersistedState) -> Result<(), String> {
    db::kv_set(
        conn,
        "license_key",
        &serde_json::to_string(&state.license_key)
            .map_err(|e| format!("Failed to serialize license_key: {e}"))?,
    )?;
    db::kv_set(
        conn,
        "openai_api_key",
        &serde_json::to_string(&state.openai_api_key)
            .map_err(|e| format!("Failed to serialize openai_api_key: {e}"))?,
    )?;
    db::kv_set(
        conn,
        "groq_api_key",
        &serde_json::to_string(&state.groq_api_key)
            .map_err(|e| format!("Failed to serialize groq_api_key: {e}"))?,
    )?;
    db::kv_set(conn, "active_provider", state.active_provider.as_str())?;
    db::kv_set(
        conn,
        "onboarding_complete",
        if state.onboarding_complete {
            "true"
        } else {
            "false"
        },
    )?;
    db::kv_set(
        conn,
        "app_settings",
        &serde_json::to_string(&state.settings)
            .map_err(|e| format!("Failed to serialize app_settings: {e}"))?,
    )?;
    // History is append-only via record_history — not overwritten here.
    Ok(())
}

pub fn record_history(conn: &Connection, entry: &HistoryEntry) -> Result<(), String> {
    db::insert_history(conn, entry)
}

fn migrate_from_json_if_needed(conn: &Connection) -> Result<(), String> {
    if db::kv_get(conn, MIGRATION_FLAG_KEY)?.is_some() {
        return Ok(());
    }

    let path = persisted_file_path()?;
    if !path.exists() {
        db::kv_set(conn, MIGRATION_FLAG_KEY, "1")?;
        return Ok(());
    }

    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read legacy state.json: {e}"))?;
    let legacy: PersistedState = serde_json::from_str(&raw)
        .map_err(|e| format!("Failed to parse legacy state.json: {e}"))?;

    save(conn, &legacy)?;
    // History arrives newest-first in the JSON; reverse so SQLite AUTOINCREMENT
    // preserves chronological order (oldest id = oldest entry).
    for entry in legacy.history.iter().rev() {
        db::insert_history(conn, entry)?;
    }

    db::kv_set(conn, MIGRATION_FLAG_KEY, "1")?;

    let backup = path.with_extension("json.migrated");
    let _ = fs::rename(&path, &backup);

    Ok(())
}

pub fn append_log(level: &str, message: &str) -> Result<(), String> {
    let path = logs_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create app log directory: {e}"))?;
    }
    let timestamp = chrono::Utc::now().to_rfc3339();
    let line = format!("{timestamp} [{level}] {message}\n");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("Failed to open log file: {e}"))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("Failed to append log line: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PersistedState, Provider};

    #[test]
    fn load_legacy_json_without_settings_uses_defaults() {
        let raw = r#"{
            "license_key":"LICENSE-1234",
            "onboarding_complete":true,
            "api_key":"gsk-test",
            "history":[]
        }"#;
        let parsed: PersistedState = serde_json::from_str(raw).expect("should parse");
        assert!(parsed.settings.general.window_movable);
        assert_eq!(parsed.settings.general.window_position, "center");
        assert_eq!(parsed.settings.shortcuts.preset, "fn");
        assert_eq!(parsed.settings.language.mode, "system");
        assert!(!parsed.settings.extras.smart_formatting);
        assert!(!parsed.settings.extras.dangerously_skip_permissions);
        // Legacy `api_key` migrates into the Groq slot.
        assert_eq!(parsed.groq_api_key.as_deref(), Some("gsk-test"));
        assert_eq!(parsed.openai_api_key, None);
        assert_eq!(parsed.active_provider, Provider::Groq);
    }

    #[test]
    fn provider_parse_round_trips() {
        assert_eq!(Provider::parse("groq"), Some(Provider::Groq));
        assert_eq!(Provider::parse("Groq"), Some(Provider::Groq));
        assert_eq!(Provider::parse("openai"), Some(Provider::Openai));
        assert_eq!(Provider::parse("OpenAI"), Some(Provider::Openai));
        assert_eq!(Provider::parse("bogus"), None);
        assert_eq!(Provider::Groq.as_str(), "groq");
        assert_eq!(Provider::Openai.as_str(), "openai");
    }
}

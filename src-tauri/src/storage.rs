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
            preset: "cmd_shift_space".to_string(),
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

impl Default for ExtrasSettings {
    fn default() -> Self {
        Self {
            auto_add_to_dictionary: false,
            smart_formatting: true,
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
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PersistedState {
    pub license_key: Option<String>,
    pub onboarding_complete: bool,
    pub openai_api_key: Option<String>,
    #[serde(default)]
    pub settings: AppSettings,
    pub history: Vec<HistoryEntry>,
}

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

pub fn load() -> Result<PersistedState, String> {
    let path = persisted_file_path()?;
    if !path.exists() {
        return Ok(PersistedState::default());
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read persisted state file: {e}"))?;
    serde_json::from_str::<PersistedState>(&raw)
        .map_err(|e| format!("Failed to parse persisted state JSON: {e}"))
}

pub fn save(state: &PersistedState) -> Result<(), String> {
    let path = persisted_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create app state directory: {e}"))?;
    }
    let raw = serde_json::to_string_pretty(state)
        .map_err(|e| format!("Failed to serialize persisted state: {e}"))?;
    fs::write(path, raw).map_err(|e| format!("Failed to write persisted state file: {e}"))?;
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
    use super::PersistedState;

    #[test]
    fn load_legacy_json_without_settings_uses_defaults() {
        let raw = r#"{
            "license_key":"LICENSE-1234",
            "onboarding_complete":true,
            "openai_api_key":"sk-test",
            "history":[]
        }"#;
        let parsed: PersistedState = serde_json::from_str(raw).expect("should parse");
        assert!(parsed.settings.general.window_movable);
        assert_eq!(parsed.settings.general.window_position, "center");
        assert_eq!(parsed.settings.shortcuts.preset, "cmd_shift_space");
        assert_eq!(parsed.settings.language.mode, "system");
        assert!(parsed.settings.extras.smart_formatting);
        assert!(!parsed.settings.extras.dangerously_skip_permissions);
    }
}

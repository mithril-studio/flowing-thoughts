use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntry {
    pub session_id: u64,
    pub text: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PersistedState {
    pub license_key: Option<String>,
    pub onboarding_complete: bool,
    pub openai_api_key: Option<String>,
    pub history: Vec<HistoryEntry>,
}

fn persisted_file_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Open Voice Wispr")
        .join("state.json"))
}

fn logs_file_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Open Voice Wispr")
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

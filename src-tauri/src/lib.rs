use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    ActivationPolicy, AppHandle, Emitter, Manager, PhysicalPosition, Position, WebviewWindow,
};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Recordings shorter than this are treated as accidental hotkey taps and
/// discarded silently — no error toast, no pipeline run.
const MIN_DICTATION_MS: u64 = 300;

mod audio;
#[cfg(target_os = "macos")]
mod ax_snapshot;
mod corrections;
mod db;
mod hotkey;
mod local_transcribe;
#[cfg(target_os = "macos")]
mod macos_ax;
#[cfg(target_os = "macos")]
mod macos_hotkey;
mod model_manager;
mod storage;
mod text_inject;
mod transcribe;

enum SessionState {
    Idle,
    Recording { session_id: u64 },
    Transcribing { session_id: u64 },
    Injecting { session_id: u64 },
}

/// Text we just injected into the focused app, held briefly so we can diff
/// against the current focused-field contents when the user starts their next
/// dictation. Ignored after `PENDING_CAPTURE_TTL`.
#[derive(Debug, Clone)]
struct PendingCapture {
    session_id: u64,
    dictation_id: String,
    model: String,
    injected_text: String,
    captured_at: Instant,
}

const PENDING_CAPTURE_TTL: Duration = Duration::from_secs(60);
const CORRECTION_PROMPT_CHAR_CAP: usize = 800;
const CORRECTION_PROMPT_LIMIT: i64 = 40;

#[derive(Debug, Clone, serde::Serialize)]
struct RecordingState {
    is_recording: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RecordingAmplitudeEvent {
    session_id: u64,
    amplitude: f32,
}

#[derive(Debug, Clone, serde::Serialize)]
struct SessionPhaseEvent {
    phase: &'static str,
}

#[derive(Debug, Clone, serde::Serialize)]
struct TranscriptionCompleteEvent {
    session_id: u64,
    text: String,
    timestamp: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct PipelineErrorEvent {
    session_id: u64,
    stage: &'static str,
    message: String,
}

/// If a text injection is still pending from a previous dictation, inspect
/// the focused text field and learn any single-word correction the user made.
/// Runs at the start of each recording session — fire-and-forget, silent on
/// failure (non-AX apps, permission missing, multi-word edits).
fn maybe_learn_from_pending_capture(
    pending: &Arc<Mutex<Option<PendingCapture>>>,
    db: &Arc<Mutex<rusqlite::Connection>>,
    persisted: &Arc<Mutex<storage::PersistedState>>,
) {
    let capture = match pending.lock() {
        Ok(mut guard) => {
            let taken = guard.take();
            match taken {
                Some(c) if c.captured_at.elapsed() <= PENDING_CAPTURE_TTL => c,
                _ => return,
            }
        }
        Err(_) => return,
    };

    let auto_learn = persisted
        .lock()
        .ok()
        .map(|s| s.settings.extras.auto_learn_corrections)
        .unwrap_or(true);
    if !auto_learn {
        return;
    }

    #[cfg(target_os = "macos")]
    let focused = ax_snapshot::read_focused_text_value();
    #[cfg(not(target_os = "macos"))]
    let focused: Option<String> = None;

    let Some(focused_text) = focused else {
        return;
    };

    let injected_trimmed = capture.injected_text.trim();
    let focused_trimmed = focused_text.trim();
    if injected_trimmed == focused_trimmed {
        return;
    }
    // extract_single_word_correction itself rejects wildly-different texts
    // (different word count), so hand it the raw focused content.
    let Some((wrong, right)) =
        corrections::extract_single_word_correction(injected_trimmed, focused_trimmed)
    else {
        return;
    };

    let Ok(conn) = db.lock() else { return };
    let correction = db::Correction {
        id: uuid::Uuid::new_v4().to_string(),
        dictation_id: capture.dictation_id.clone(),
        model: capture.model.clone(),
        wrong_text: wrong,
        intended_text: right,
        context_snippet: Some(injected_trimmed.chars().take(200).collect()),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let _ = db::insert_correction(&conn, &correction);
    let _ = storage::append_log(
        "INFO",
        &format!(
            "Learned correction from session {}: '{}' -> '{}'",
            capture.session_id, correction.wrong_text, correction.intended_text
        ),
    );
}

fn apply_smart_formatting(text: &str) -> String {
    // Preserve all whitespace (spaces, tabs, newlines) — only capitalise the
    // first visible character. Whisper already returns proper punctuation, so
    // we don't force a trailing period.
    let trimmed_start = text.trim_start_matches(|c: char| c.is_whitespace());
    if trimmed_start.is_empty() {
        return text.to_string();
    }
    let leading_ws_len = text.len() - trimmed_start.len();
    let leading_ws = &text[..leading_ws_len];
    let mut chars = trimmed_start.chars();
    let first = chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default();
    let rest: String = chars.collect();
    format!("{leading_ws}{first}{rest}")
}

#[cfg(test)]
mod tests {
    use super::apply_smart_formatting;

    #[test]
    fn smart_formatting_preserves_whitespace_and_capitalises_first_letter() {
        assert_eq!(
            apply_smart_formatting("hello   world"),
            "Hello   world"
        );
        assert_eq!(
            apply_smart_formatting("line one\nline two"),
            "Line one\nline two"
        );
        assert_eq!(apply_smart_formatting("already done?"), "Already done?");
    }
}

#[tauri::command]
fn get_recording_state(state: tauri::State<'_, Arc<Mutex<SessionState>>>) -> bool {
    matches!(
        *state.inner().lock().unwrap(),
        SessionState::Recording { .. }
    )
}

#[tauri::command]
fn copy_to_clipboard(text: String) -> Result<(), String> {
    text_inject::write_clipboard(&text)
}

fn parse_provider_arg(provider: &str) -> Result<storage::Provider, String> {
    storage::Provider::parse(provider)
        .ok_or_else(|| format!("Unknown provider '{provider}'. Expected 'groq' or 'openai'."))
}

#[tauri::command]
fn set_api_key(
    provider: String,
    key: String,
    persisted: tauri::State<'_, Arc<Mutex<storage::PersistedState>>>,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<(), String> {
    let provider = parse_provider_arg(&provider)?;
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err("API key cannot be empty".to_string());
    }
    let mut state = persisted
        .inner()
        .lock()
        .map_err(|_| "Persisted state lock poisoned".to_string())?;
    match provider {
        storage::Provider::Groq => state.groq_api_key = Some(trimmed.to_string()),
        storage::Provider::Openai => state.openai_api_key = Some(trimmed.to_string()),
    }
    state.active_provider = provider;
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    storage::save(&conn, &state)?;
    Ok(())
}

#[tauri::command]
fn set_active_provider(
    provider: String,
    persisted: tauri::State<'_, Arc<Mutex<storage::PersistedState>>>,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<(), String> {
    let provider = parse_provider_arg(&provider)?;
    let mut state = persisted
        .inner()
        .lock()
        .map_err(|_| "Persisted state lock poisoned".to_string())?;
    state.active_provider = provider;
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    storage::save(&conn, &state)?;
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
struct PersistedStateView {
    onboarding_complete: bool,
    license_key: Option<String>,
    groq_api_key_configured: bool,
    openai_api_key_configured: bool,
    active_provider: String,
    history: Vec<storage::HistoryEntry>,
    settings: storage::AppSettings,
}

#[tauri::command]
fn get_persisted_state(
    persisted: tauri::State<'_, Arc<Mutex<storage::PersistedState>>>,
) -> Result<PersistedStateView, String> {
    let state = persisted
        .inner()
        .lock()
        .map_err(|_| "Persisted state lock poisoned".to_string())?;
    Ok(PersistedStateView {
        onboarding_complete: state.onboarding_complete,
        license_key: state.license_key.clone(),
        groq_api_key_configured: state.groq_api_key.is_some(),
        openai_api_key_configured: state.openai_api_key.is_some(),
        active_provider: state.active_provider.as_str().to_string(),
        history: state.history.clone(),
        settings: state.settings.clone(),
    })
}

#[derive(Debug, Clone, serde::Serialize)]
struct AppSettingsUpdateResult {
    settings: storage::AppSettings,
    warnings: Vec<String>,
}

fn sanitize_settings(settings: &mut storage::AppSettings) {
    if settings.shortcuts.preset != "cmd_shift_space" && settings.shortcuts.preset != "fn" {
        settings.shortcuts.preset = "fn".to_string();
    }
    let valid_language_modes = ["system", "en", "nl"];
    if !valid_language_modes.contains(&settings.language.mode.as_str()) {
        settings.language.mode = "system".to_string();
    }
    if settings.transcription.provider != "local" && settings.transcription.provider != "api" {
        settings.transcription.provider = "local".to_string();
    }
    if model_manager::ModelId::from_str(&settings.transcription.local_model).is_none() {
        settings.transcription.local_model = "whisper-small-q5".to_string();
    }
    if settings.microphone.input_device.trim().is_empty() {
        settings.microphone.input_device = "system_default".to_string();
    }
    let valid_positions = ["center", "top_left", "top_right", "bottom_left", "bottom_right"];
    if !valid_positions.contains(&settings.general.window_position.as_str()) {
        settings.general.window_position = "center".to_string();
    }
    let valid_themes = ["light", "dark", "system"];
    if !valid_themes.contains(&settings.general.theme.as_str()) {
        settings.general.theme = "light".to_string();
    }
}

fn apply_window_movable(window: &WebviewWindow, movable: bool) {
    let _ = window.eval(&format!(
        "window.dispatchEvent(new CustomEvent('ovw-window-movable', {{ detail: {{ movable: {} }} }}));",
        if movable { "true" } else { "false" }
    ));
}

fn apply_window_position(window: &WebviewWindow, position: &str) -> Result<(), String> {
    let monitor = window
        .current_monitor()
        .map_err(|e| format!("Failed to read current monitor: {e}"))?
        .ok_or_else(|| "No monitor available for positioning".to_string())?;
    let monitor_pos = monitor.position();
    let monitor_size = monitor.size();
    let window_size = window
        .outer_size()
        .map_err(|e| format!("Failed to read window size: {e}"))?;

    let margin = 24_i32;
    let monitor_w = monitor_size.width as i32;
    let monitor_h = monitor_size.height as i32;
    let win_w = window_size.width as i32;
    let win_h = window_size.height as i32;

    let (offset_x, offset_y) = match position {
        "top_left" => (margin, margin),
        "top_right" => ((monitor_w - win_w - margin).max(margin), margin),
        "bottom_left" => (margin, (monitor_h - win_h - margin).max(margin)),
        "bottom_right" => (
            (monitor_w - win_w - margin).max(margin),
            (monitor_h - win_h - margin).max(margin),
        ),
        _ => (
            ((monitor_w - win_w) / 2).max(0),
            ((monitor_h - win_h) / 2).max(0),
        ),
    };

    window
        .set_position(Position::Physical(PhysicalPosition {
            x: monitor_pos.x + offset_x,
            y: monitor_pos.y + offset_y,
        }))
        .map_err(|e| format!("Failed to set window position: {e}"))
}

#[cfg(target_os = "macos")]
fn apply_launch_at_login(enabled: bool) -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|e| format!("Failed to resolve executable path: {e}"))?;
    let exe_path = executable
        .to_str()
        .ok_or_else(|| "Executable path contains invalid UTF-8".to_string())?;
    let script = if enabled {
        format!(
            "tell application \"System Events\"\n\
             set existingItems to login items where name is \"FlowingThoughts\"\n\
             repeat with itemRef in existingItems\n\
             delete itemRef\n\
             end repeat\n\
             make login item at end with properties {{name:\"FlowingThoughts\", path:\"{exe_path}\", hidden:false}}\n\
             end tell"
        )
    } else {
        "tell application \"System Events\"\n\
         set existingItems to login items where name is \"FlowingThoughts\"\n\
         repeat with itemRef in existingItems\n\
         delete itemRef\n\
         end repeat\n\
         end tell"
            .to_string()
    };

    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| format!("Failed to apply launch-at-login setting: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Launch-at-login update failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(not(target_os = "macos"))]
fn apply_launch_at_login(_enabled: bool) -> Result<(), String> {
    Err("Launch at login setting is only supported on macOS".to_string())
}

fn apply_show_in_dock(app: &AppHandle, show_in_dock: bool) -> Result<(), String> {
    let policy = if show_in_dock {
        ActivationPolicy::Regular
    } else {
        ActivationPolicy::Accessory
    };
    app.set_activation_policy(policy)
        .map_err(|e| format!("Failed to apply dock visibility: {e}"))
}

#[derive(Debug, Clone, serde::Serialize)]
struct AppVersion {
    version: &'static str,
    commit: &'static str,
}

#[tauri::command]
fn get_app_version() -> AppVersion {
    AppVersion {
        version: env!("CARGO_PKG_VERSION"),
        commit: env!("GIT_COMMIT_SHA"),
    }
}

#[tauri::command]
fn get_app_settings(
    persisted: tauri::State<'_, Arc<Mutex<storage::PersistedState>>>,
) -> Result<storage::AppSettings, String> {
    let mut state = persisted
        .inner()
        .lock()
        .map_err(|_| "Persisted state lock poisoned".to_string())?;
    sanitize_settings(&mut state.settings);
    Ok(state.settings.clone())
}

#[tauri::command]
fn update_app_settings(
    settings: storage::AppSettings,
    app: tauri::AppHandle,
    persisted: tauri::State<'_, Arc<Mutex<storage::PersistedState>>>,
    hotkey_mode: tauri::State<'_, Arc<Mutex<hotkey::HotkeyMode>>>,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<AppSettingsUpdateResult, String> {
    let mut next_settings = settings;
    sanitize_settings(&mut next_settings);

    let previous_settings = {
        let mut state = persisted
            .inner()
            .lock()
            .map_err(|_| "Persisted state lock poisoned".to_string())?;
        let previous = state.settings.clone();
        state.settings = next_settings.clone();
        let conn = db_conn
            .inner()
            .lock()
            .map_err(|_| "DB lock poisoned".to_string())?;
        storage::save(&conn, &state)?;
        previous
    };

    if hotkey::mode_from_env().is_none() {
        if let Ok(mut mode) = hotkey_mode.inner().lock() {
            *mode = hotkey::HotkeyMode::from_preset(&next_settings.shortcuts.preset);
        }
    }

    // Only apply (and only warn about) side effects for settings the user
    // actually changed — re-applying everything on every toggle produced
    // spurious "settings not correct" warnings and re-centred the window.
    let mut warnings = Vec::new();
    if let Some(window) = app.get_webview_window("main") {
        if next_settings.general.window_movable != previous_settings.general.window_movable {
            apply_window_movable(&window, next_settings.general.window_movable);
        }
        if next_settings.general.window_position != previous_settings.general.window_position {
            if let Err(e) =
                apply_window_position(&window, &next_settings.general.window_position)
            {
                warnings.push(e);
            }
        }
    }
    if next_settings.general.show_in_dock != previous_settings.general.show_in_dock {
        if let Err(e) = apply_show_in_dock(&app, next_settings.general.show_in_dock) {
            warnings.push(e);
        }
    }
    if next_settings.general.launch_at_login != previous_settings.general.launch_at_login {
        if let Err(e) = apply_launch_at_login(next_settings.general.launch_at_login) {
            warnings.push(e);
        }
    }

    for message in &warnings {
        let _ = storage::append_log("WARN", message);
    }

    Ok(AppSettingsUpdateResult {
        settings: next_settings,
        warnings,
    })
}

#[tauri::command]
fn save_onboarding_state(
    license_key: Option<String>,
    onboarding_complete: bool,
    persisted: tauri::State<'_, Arc<Mutex<storage::PersistedState>>>,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<(), String> {
    let mut state = persisted
        .inner()
        .lock()
        .map_err(|_| "Persisted state lock poisoned".to_string())?;
    if let Some(key) = license_key {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            state.license_key = Some(trimmed.to_string());
        }
    }
    state.onboarding_complete = onboarding_complete;
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    storage::save(&conn, &state)?;
    Ok(())
}

#[tauri::command]
fn check_accessibility_permission(prompt: Option<bool>) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        // With `prompt: true` this registers FlowingThoughts in
        // System Settings > Privacy & Security > Accessibility and surfaces a
        // system dialog if not yet granted. `prompt: false` is a silent status
        // read, safe for UI polling.
        Ok(macos_ax::is_process_trusted(prompt.unwrap_or(true)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = prompt;
        Ok(true)
    }
}

#[tauri::command]
fn check_input_monitoring_permission(prompt: bool) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        if macos_hotkey::input_monitoring_granted() {
            return Ok(true);
        }
        if prompt {
            return Ok(macos_hotkey::request_input_monitoring());
        }
        Ok(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = prompt;
        Ok(true)
    }
}

#[tauri::command]
fn open_accessibility_settings() -> Result<(), String> {
    let targets = [
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        "x-apple.systempreferences:com.apple.preference.security?Privacy",
    ];
    for target in targets {
        let status = Command::new("open").arg(target).status();
        if let Ok(exit) = status {
            if exit.success() {
                return Ok(());
            }
        }
    }

    let fallback = Command::new("open")
        .args(["-b", "com.apple.systempreferences"])
        .status()
        .map_err(|e| format!("Failed to open System Settings: {e}"))?;
    if fallback.success() {
        Ok(())
    } else {
        Err(format!(
            "Unable to open Accessibility settings automatically. Open System Settings manually (Privacy & Security > Accessibility). Exit status: {fallback}"
        ))
    }
}

#[tauri::command]
fn run_injection_test() -> Result<(), String> {
    text_inject::inject_text("FlowingThoughts test successful.")
}

#[tauri::command]
fn open_logs_folder() -> Result<(), String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set".to_string())?;
    let logs_dir = std::path::PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("FlowingThoughts");
    let status = Command::new("open")
        .arg(logs_dir)
        .status()
        .map_err(|e| format!("Failed to open logs folder: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Open logs folder command failed with status: {status}"))
    }
}

#[tauri::command]
fn start_window_drag(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "Main window not found".to_string())?;
    window
        .start_dragging()
        .map_err(|e| format!("Failed to start window drag: {e}"))
}

#[tauri::command]
fn minimize_window(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "Main window not found".to_string())?;
    window
        .minimize()
        .map_err(|e| format!("Failed to minimize window: {e}"))
}

#[tauri::command]
fn hide_window(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "Main window not found".to_string())?;
    window
        .hide()
        .map_err(|e| format!("Failed to hide window: {e}"))
}

#[derive(Debug, Clone, serde::Serialize)]
struct AccessibilityHelpInfo {
    executable_path: String,
    is_dev_build: bool,
    note: String,
}

#[tauri::command]
fn get_accessibility_help_info() -> Result<AccessibilityHelpInfo, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Failed to resolve executable path: {e}"))?;
    let executable_path = exe
        .to_str()
        .ok_or_else(|| "Executable path contains invalid UTF-8".to_string())?
        .to_string();
    let is_dev_build = cfg!(debug_assertions);
    let note = if is_dev_build {
        "Dev build detected. In macOS Accessibility, allow Terminal/iTerm and the debug binary path."
            .to_string()
    } else {
        "Bundled app detected. Allow FlowingThoughts in macOS Accessibility.".to_string()
    };
    Ok(AccessibilityHelpInfo {
        executable_path,
        is_dev_build,
        note,
    })
}

#[tauri::command]
fn reveal_current_executable() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Failed to resolve executable path: {e}"))?;
    let status = Command::new("open")
        .arg("-R")
        .arg(exe)
        .status()
        .map_err(|e| format!("Failed to reveal executable in Finder: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Reveal executable command failed with status: {status}"
        ))
    }
}

#[tauri::command]
fn list_snippets(
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<Vec<db::Snippet>, String> {
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    db::list_snippets(&conn)
}

#[tauri::command]
fn save_snippet(
    snippet: db::Snippet,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<(), String> {
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    db::save_snippet(&conn, &snippet)
}

#[tauri::command]
fn delete_snippet(
    id: String,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<(), String> {
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    db::delete_snippet(&conn, &id)
}

#[tauri::command]
fn list_notes(
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<Vec<db::Note>, String> {
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    db::list_notes(&conn)
}

#[tauri::command]
fn save_note(
    note: db::Note,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<(), String> {
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    db::save_note(&conn, &note)
}

#[tauri::command]
fn delete_note(
    id: String,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<(), String> {
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    db::delete_note(&conn, &id)
}

#[tauri::command]
fn list_installed_models() -> Result<Vec<model_manager::InstalledModel>, String> {
    model_manager::list_installed()
}

#[tauri::command]
async fn download_model(app: AppHandle, model_id: String) -> Result<(), String> {
    let id = model_manager::ModelId::from_str(&model_id)
        .ok_or_else(|| format!("Unknown model id: {model_id}"))?;
    model_manager::download_model(app, id).await
}

#[tauri::command]
fn delete_model(model_id: String) -> Result<(), String> {
    let id = model_manager::ModelId::from_str(&model_id)
        .ok_or_else(|| format!("Unknown model id: {model_id}"))?;
    model_manager::delete_model(id)
}

#[derive(Debug, Clone, serde::Serialize)]
struct LabSessionSummary {
    dictation: db::Dictation,
    transcriptions: Vec<db::TranscriptionRow>,
    choice: Option<db::LabChoice>,
}

#[tauri::command]
fn list_lab_sessions(
    limit: Option<i64>,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<Vec<LabSessionSummary>, String> {
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    let dictations = db::list_recent_dictations(&conn, limit.unwrap_or(50))?;
    let mut out = Vec::with_capacity(dictations.len());
    for d in dictations {
        let transcriptions = db::list_transcriptions_for(&conn, &d.id)?;
        let choice = db::get_choice(&conn, &d.id)?;
        out.push(LabSessionSummary {
            dictation: d,
            transcriptions,
            choice,
        });
    }
    Ok(out)
}

#[tauri::command]
fn get_top_mistranscribed(
    model: Option<String>,
    limit: Option<i64>,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<Vec<db::MistranscribedWord>, String> {
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    db::top_mistranscribed_words(&conn, model.as_deref(), limit.unwrap_or(20))
}

#[tauri::command]
fn list_corrections(
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<Vec<db::Correction>, String> {
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    db::list_corrections(&conn)
}

#[tauri::command]
fn delete_correction(
    id: String,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<(), String> {
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    db::delete_correction(&conn, &id)
}

#[tauri::command]
fn update_history_text(
    session_id: u64,
    new_text: String,
    persisted: tauri::State<'_, Arc<Mutex<storage::PersistedState>>>,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<(), String> {
    {
        let conn = db_conn
            .inner()
            .lock()
            .map_err(|_| "DB lock poisoned".to_string())?;
        db::update_history_text(&conn, session_id, &new_text)?;
    }
    if let Ok(mut state) = persisted.inner().lock() {
        for entry in state.history.iter_mut() {
            if entry.session_id == session_id {
                entry.text = new_text.clone();
            }
        }
    }
    Ok(())
}

/// Save a correction derived from an in-app edit (Home page). Runs the same
/// single-word filter as the auto-learn path so users can't accidentally
/// store a whole-phrase rewrite as a "correction" that would misfire on
/// future transcriptions.
#[tauri::command]
fn save_correction_from_edit(
    session_id: u64,
    dictation_id: Option<String>,
    model: Option<String>,
    original: String,
    edited: String,
    persisted: tauri::State<'_, Arc<Mutex<storage::PersistedState>>>,
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<Option<db::Correction>, String> {
    let auto_learn = persisted
        .inner()
        .lock()
        .ok()
        .map(|s| s.settings.extras.auto_learn_corrections)
        .unwrap_or(true);
    if !auto_learn {
        return Ok(None);
    }
    let Some((wrong, right)) =
        corrections::extract_single_word_correction(&original, &edited)
    else {
        return Ok(None);
    };
    let correction = db::Correction {
        id: uuid::Uuid::new_v4().to_string(),
        dictation_id: dictation_id.unwrap_or_else(|| format!("history-session-{session_id}")),
        model: model.unwrap_or_else(|| "user-edit".to_string()),
        wrong_text: wrong,
        intended_text: right,
        context_snippet: Some(original.chars().take(200).collect()),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    db::insert_correction(&conn, &correction)?;
    Ok(Some(correction))
}

#[tauri::command]
fn open_input_monitoring_settings() -> Result<(), String> {
    let targets = [
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
        "x-apple.systempreferences:com.apple.preference.security?Privacy",
    ];
    for target in targets {
        let status = Command::new("open").arg(target).status();
        if let Ok(exit) = status {
            if exit.success() {
                return Ok(());
            }
        }
    }

    let fallback = Command::new("open")
        .args(["-b", "com.apple.systempreferences"])
        .status()
        .map_err(|e| format!("Failed to open System Settings: {e}"))?;
    if fallback.success() {
        Ok(())
    } else {
        Err(format!(
            "Unable to open Input Monitoring settings automatically. Open System Settings manually (Privacy & Security > Input Monitoring). Exit status: {fallback}"
        ))
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let session_state = Arc::new(Mutex::new(SessionState::Idle));
    let pending_capture: Arc<Mutex<Option<PendingCapture>>> = Arc::new(Mutex::new(None));
    let db_conn = Arc::new(Mutex::new(
        db::open().expect("Failed to initialize SQLite database"),
    ));
    let (persisted_state, first_session_id) = {
        let conn = db_conn.lock().expect("DB lock poisoned during startup");
        let mut loaded = storage::load(&conn).unwrap_or_default();
        sanitize_settings(&mut loaded.settings);
        // Session ids must stay unique across restarts — they key the
        // history UI and edits.
        let next_id = db::max_history_session_id(&conn).unwrap_or(0) + 1;
        (loaded, next_id)
    };
    let next_session_id = Arc::new(Mutex::new(first_session_id));
    let configured_hotkey_mode =
        hotkey::HotkeyMode::from_preset(&persisted_state.settings.shortcuts.preset);
    let runtime_hotkey_mode = hotkey::mode_from_env().unwrap_or(configured_hotkey_mode);
    let persisted = Arc::new(Mutex::new(persisted_state));
    let hotkey_mode = Arc::new(Mutex::new(runtime_hotkey_mode));

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(session_state.clone())
        .manage(persisted.clone())
        .manage(hotkey_mode.clone())
        .manage(db_conn.clone())
        .manage(pending_capture.clone())
        .setup(move |app| {
            // Build tray menu
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let show =
                MenuItem::with_id(app, "show", "Show Settings", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            // Create tray icon
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .icon_as_template(true)
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => {
                        app.exit(0);
                    }
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.unminimize();
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            let shared_db_conn = db_conn.clone();

            // Dock the floating indicator to the top-left of the primary
            // monitor on launch — under the macOS menu bar. If the user
            // previously dragged it elsewhere, restore that position (clamped
            // to the current monitor so a stored position from a now-missing
            // display can't strand the window off-screen).
            if let Some(indicator) = app.get_webview_window("indicator") {
                let _ = indicator.set_always_on_top(true);
                let _ = indicator.set_visible_on_all_workspaces(true);
                // Keep the pill visible over fullscreen apps too (Wispr-style):
                // canJoinAllSpaces (1<<0) | fullScreenAuxiliary (1<<8), raised
                // to status-window level.
                #[cfg(target_os = "macos")]
                if let Ok(ns_window) = indicator.ns_window() {
                    unsafe {
                        use objc2::msg_send;
                        use objc2::runtime::AnyObject;
                        let win = &*(ns_window as *const AnyObject);
                        let behavior: u64 = (1 << 0) | (1 << 8);
                        let level: isize = 25; // NSStatusWindowLevel
                        let _: () = msg_send![win, setCollectionBehavior: behavior];
                        let _: () = msg_send![win, setLevel: level];
                    }
                }
                if let Some(monitor) = indicator.current_monitor().ok().flatten() {
                    let size = indicator.outer_size().unwrap_or_default();
                    let mpos = monitor.position();
                    let msize = monitor.size();

                    let saved = {
                        let conn = shared_db_conn.lock().unwrap();
                        storage::load_indicator_position(&conn)
                    };

                    let (x, y) = match saved {
                        Some((sx, sy)) => {
                            let min_x = mpos.x;
                            let max_x = mpos.x + (msize.width as i32 - size.width as i32).max(0);
                            let min_y = mpos.y;
                            let max_y = mpos.y + (msize.height as i32 - size.height as i32).max(0);
                            (sx.clamp(min_x, max_x), sy.clamp(min_y, max_y))
                        }
                        // Default: bottom-center, floating just above the Dock
                        // (Wispr-style) so the emblem is always in view.
                        None => {
                            let dock_clearance = (msize.height as f64 * 0.06) as i32;
                            (
                                mpos.x + (msize.width as i32 - size.width as i32).max(0) / 2,
                                mpos.y
                                    + (msize.height as i32
                                        - size.height as i32
                                        - dock_clearance)
                                        .max(0),
                            )
                        }
                    };
                    let _ = indicator.set_position(Position::Physical(
                        PhysicalPosition { x, y },
                    ));
                }
                let _ = indicator.show();

                // Persist the indicator position on drag. `Moved` fires many
                // times during a drag; throttle to the last write plus a small
                // delta so we don't hammer SQLite.
                let persist_conn = shared_db_conn.clone();
                let last_saved: Arc<Mutex<Option<(i32, i32, Instant)>>> =
                    Arc::new(Mutex::new(None));
                indicator.on_window_event(move |event| {
                    if let tauri::WindowEvent::Moved(pos) = event {
                        let now = Instant::now();
                        let mut guard = last_saved.lock().unwrap();
                        let should_write = match *guard {
                            Some((lx, ly, last)) => {
                                let moved_enough =
                                    (pos.x - lx).abs() >= 2 || (pos.y - ly).abs() >= 2;
                                let elapsed_enough =
                                    now.duration_since(last) >= Duration::from_millis(300);
                                moved_enough && elapsed_enough
                            }
                            None => true,
                        };
                        if should_write {
                            *guard = Some((pos.x, pos.y, now));
                            drop(guard);
                            if let Ok(conn) = persist_conn.lock() {
                                let _ = storage::save_indicator_position(
                                    &conn,
                                    (pos.x, pos.y),
                                );
                            }
                        }
                    }
                });
            }

            // Register this process with the macOS Accessibility permission
            // database so it shows up in System Settings > Privacy & Security
            // > Accessibility with a toggle. Without this call, dev builds
            // launched via `tauri dev` never appear in that list.
            #[cfg(target_os = "macos")]
            {
                let _ = macos_ax::is_process_trusted(true);
            }

            // Start fn key listener
            let hotkey_rx = hotkey::start_listener(hotkey_mode.clone());
            let app_handle = app.handle().clone();
            let shared_session_state = session_state.clone();
            let shared_next_session_id = next_session_id.clone();
            let shared_persisted = persisted.clone();
            let shared_pending_capture = pending_capture.clone();

            thread::spawn(move || {
                let mut active_recording: Option<(u64, audio::ActiveRecording)> = None;
                let mut active_amplitude_stop: Option<Arc<std::sync::atomic::AtomicBool>> = None;

                loop {
                    // Start capturing the moment the hotkey goes down — no
                    // arming delay, so the first words are never cut off.
                    // Accidental taps are filtered after release by the
                    // MIN_DICTATION_MS duration guard instead.
                    let event = match hotkey_rx.recv() {
                        Ok(event) => event,
                        Err(_) => break,
                    };

                    match event {
                        hotkey::HotkeyEvent::RecordStart => {
                            // Learn from any edits the user made to the text we
                                // injected on the last session. Runs before we acquire
                            // the session lock so failures (AX denied, non-AX app)
                            // never delay recording.
                            maybe_learn_from_pending_capture(
                                &shared_pending_capture,
                                &shared_db_conn,
                                &shared_persisted,
                            );

                            let mut session_state_guard = shared_session_state.lock().unwrap();
                            if !matches!(*session_state_guard, SessionState::Idle) {
                                continue;
                            }

                            let mut id_guard = shared_next_session_id.lock().unwrap();
                            let session_id = *id_guard;
                            *id_guard += 1;
                            let _ = storage::append_log(
                                "INFO",
                                &format!("Session {session_id} starting audio capture"),
                            );

                            if let Some(stop) = active_amplitude_stop.take() {
                                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            let recording = match audio::start_recording() {
                                Ok(recording) => recording,
                                Err(message) => {
                                    drop(id_guard);
                                    drop(session_state_guard);
                                    let _ = app_handle.emit(
                                        "pipeline-error",
                                        PipelineErrorEvent {
                                            session_id,
                                            stage: "audio-start",
                                            message,
                                        },
                                    );
                                    let _ = storage::append_log(
                                        "ERROR",
                                        &format!("Session {session_id} failed at audio-start"),
                                    );
                                    let _ = app_handle.emit(
                                        "session-phase",
                                        SessionPhaseEvent { phase: "error" },
                                    );
                                    let _ = app_handle.emit(
                                        "session-phase",
                                        SessionPhaseEvent { phase: "idle" },
                                    );
                                    continue;
                                }
                            };
                            let amplitude_handle = recording.amplitude_handle();
                            active_recording = Some((session_id, recording));

                            *session_state_guard = SessionState::Recording { session_id };
                            drop(id_guard);
                            drop(session_state_guard);

                            let _ = app_handle.emit(
                                "recording-state",
                                RecordingState {
                                    is_recording: true,
                                },
                            );
                            let _ = app_handle.emit(
                                "session-phase",
                                SessionPhaseEvent { phase: "recording" },
                            );

                            let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
                            active_amplitude_stop = Some(stop_flag.clone());
                            let amplitude_app = app_handle.clone();
                            let amplitude_session = session_id;
                            thread::spawn(move || {
                                while !stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
                                    let amp = audio::read_amplitude(&amplitude_handle);
                                    let _ = amplitude_app.emit(
                                        "recording-amplitude",
                                        RecordingAmplitudeEvent {
                                            session_id: amplitude_session,
                                            amplitude: amp,
                                        },
                                    );
                                    thread::sleep(Duration::from_millis(60));
                                }
                            });
                        }
                        hotkey::HotkeyEvent::RecordStop => {
                            if let Some(stop) = active_amplitude_stop.take() {
                                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            let (session_id, recording) = {
                                let mut session_state_guard = shared_session_state.lock().unwrap();
                                let active_state = std::mem::replace(
                                    &mut *session_state_guard,
                                    SessionState::Idle,
                                );
                                match (active_state, active_recording.take()) {
                                    (
                                        SessionState::Recording { session_id },
                                        Some((recording_session_id, recording)),
                                    ) if recording_session_id == session_id => {
                                        *session_state_guard =
                                            SessionState::Transcribing { session_id };
                                        (session_id, recording)
                                    }
                                    _ => continue,
                                }
                            };

                            let capture = match audio::stop_and_finalize(recording, session_id) {
                                Ok(capture) => capture,
                                Err(message) => {
                                    let _ = app_handle.emit(
                                        "pipeline-error",
                                        PipelineErrorEvent {
                                            session_id,
                                            stage: "audio-finalize",
                                            message,
                                        },
                                    );
                                    let _ = app_handle.emit(
                                        "session-phase",
                                        SessionPhaseEvent { phase: "error" },
                                    );
                                    let _ = app_handle.emit(
                                        "session-phase",
                                        SessionPhaseEvent { phase: "idle" },
                                    );
                                    let _ = storage::append_log(
                                        "ERROR",
                                        &format!("Session {session_id} failed at audio-finalize"),
                                    );
                                    let mut session_state_guard =
                                        shared_session_state.lock().unwrap();
                                    if matches!(
                                        *session_state_guard,
                                        SessionState::Transcribing {
                                            session_id: current_id
                                        } if current_id == session_id
                                    ) {
                                        *session_state_guard = SessionState::Idle;
                                    }
                                    continue;
                                }
                            };
                            if capture.duration_ms < MIN_DICTATION_MS {
                                // Accidental tap — discard silently, no error UI.
                                let _ = app_handle.emit(
                                    "recording-state",
                                    RecordingState {
                                        is_recording: false,
                                    },
                                );
                                let _ = app_handle.emit(
                                    "session-phase",
                                    SessionPhaseEvent { phase: "idle" },
                                );
                                let _ = storage::append_log(
                                    "INFO",
                                    &format!(
                                        "Session {session_id} dropped as accidental tap ({}ms)",
                                        capture.duration_ms
                                    ),
                                );
                                let _ = std::fs::remove_file(&capture.wav_path);
                                let mut session_state_guard = shared_session_state.lock().unwrap();
                                if matches!(
                                    *session_state_guard,
                                    SessionState::Transcribing {
                                        session_id: current_id
                                    } if current_id == session_id
                                ) {
                                    *session_state_guard = SessionState::Idle;
                                }
                                continue;
                            }

                            let _ = app_handle.emit(
                                "recording-state",
                                RecordingState {
                                    is_recording: false,
                                },
                            );
                            let _ = app_handle.emit(
                                "session-phase",
                                SessionPhaseEvent {
                                    phase: "transcribing",
                                },
                            );

                            let app_handle_for_task = app_handle.clone();
                            let session_state_for_task = shared_session_state.clone();
                            let persisted_for_task = shared_persisted.clone();
                            let db_conn_for_task = shared_db_conn.clone();
                            let pending_capture_for_task = shared_pending_capture.clone();
                            let wav_path_for_task = capture.wav_path.clone();

                            tauri::async_runtime::spawn(async move {
                                let (
                                    provider,
                                    runtime_api_key,
                                    language_mode,
                                    smart_formatting,
                                    transcription_mode,
                                    local_model,
                                ) = persisted_for_task
                                    .lock()
                                    .ok()
                                    .map(|state| {
                                        let provider = state.active_provider;
                                        let key = match provider {
                                            storage::Provider::Groq => {
                                                state.groq_api_key.clone()
                                            }
                                            storage::Provider::Openai => {
                                                state.openai_api_key.clone()
                                            }
                                        };
                                        (
                                            provider,
                                            key,
                                            state.settings.language.mode.clone(),
                                            state.settings.extras.smart_formatting,
                                            state.settings.transcription.provider.clone(),
                                            state.settings.transcription.local_model.clone(),
                                        )
                                    })
                                    .unwrap_or((
                                        storage::Provider::Groq,
                                        None,
                                        "system".to_string(),
                                        true,
                                        "local".to_string(),
                                        "whisper-small-q5".to_string(),
                                    ));

                                let dictation_id = uuid::Uuid::new_v4().to_string();
                                let started_at = chrono::Utc::now().to_rfc3339();
                                if let Ok(conn) = db_conn_for_task.lock() {
                                    let _ = db::insert_dictation(
                                        &conn,
                                        &db::Dictation {
                                            id: dictation_id.clone(),
                                            session_id,
                                            started_at: started_at.clone(),
                                            wav_path: Some(
                                                wav_path_for_task
                                                    .to_string_lossy()
                                                    .into_owned(),
                                            ),
                                            duration_ms: Some(capture.duration_ms),
                                            sample_rate: None,
                                        },
                                    );
                                }

                                // Build a Whisper `prompt` from the intended terms the
                                // user has taught us, so the decoder biases toward
                                // them on ambiguous audio.
                                let correction_prompt: Option<String> = db_conn_for_task
                                    .lock()
                                    .ok()
                                    .and_then(|conn| {
                                        db::top_mistranscribed_words(
                                            &conn,
                                            None,
                                            CORRECTION_PROMPT_LIMIT,
                                        )
                                        .ok()
                                    })
                                    .and_then(|rows| {
                                        let terms: Vec<String> = rows
                                            .into_iter()
                                            .map(|r| r.intended_text)
                                            .collect();
                                        corrections::build_prompt_from_corrections(
                                            &terms,
                                            CORRECTION_PROMPT_CHAR_CAP,
                                        )
                                    });

                                // Run exactly one transcription — the model the user
                                // picked. (Earlier builds fanned out to 4 models per
                                // dictation, which saturated CPU/RAM and froze the UI.)
                                let local_model_id = model_manager::ModelId::from_str(&local_model);
                                let installed = local_model_id
                                    .and_then(|id| model_manager::model_path(id).ok())
                                    .map(|p| p.exists())
                                    .unwrap_or(false);
                                let use_local = transcription_mode == "local" && installed;

                                let started = Instant::now();
                                let (primary_label, transcript_result): (String, Result<String, String>) =
                                    if use_local {
                                        let id = local_model_id.unwrap();
                                        let result = local_transcribe::transcribe_local(
                                            id,
                                            &wav_path_for_task,
                                            &language_mode,
                                            correction_prompt.clone(),
                                        )
                                        .await
                                        .map(|(text, _latency)| text);
                                        (local_model.clone(), result)
                                    } else if transcription_mode == "local"
                                        && runtime_api_key.is_none()
                                    {
                                        (
                                            local_model.clone(),
                                            Err(format!(
                                                "Local model '{local_model}' is not downloaded yet. Open Settings → Transcription to download it."
                                            )),
                                        )
                                    } else {
                                        // Cloud path: chosen explicitly, or fallback
                                        // because the local model isn't installed but
                                        // an API key is configured.
                                        let label = match provider {
                                            storage::Provider::Groq => "groq-api".to_string(),
                                            storage::Provider::Openai => "openai-api".to_string(),
                                        };
                                        let result = transcribe::transcribe_audio(
                                            session_id,
                                            &wav_path_for_task,
                                            capture.duration_ms,
                                            provider,
                                            runtime_api_key.as_deref(),
                                            Some(&language_mode),
                                            correction_prompt.as_deref(),
                                        )
                                        .await;
                                        (label, result)
                                    };
                                let latency_ms = started.elapsed().as_millis() as u64;

                                // Persist the result row, then clean up the WAV.
                                if let Ok(conn) = db_conn_for_task.lock() {
                                    let _ = db::insert_transcription(
                                        &conn,
                                        &db::TranscriptionRow {
                                            id: uuid::Uuid::new_v4().to_string(),
                                            dictation_id: dictation_id.clone(),
                                            model: primary_label.clone(),
                                            text: transcript_result.as_ref().ok().cloned(),
                                            latency_ms: Some(latency_ms),
                                            error: transcript_result.as_ref().err().cloned(),
                                            created_at: chrono::Utc::now().to_rfc3339(),
                                        },
                                    );
                                    let _ = db::clear_wav_path(&conn, &dictation_id);
                                }
                                let _ = std::fs::remove_file(&wav_path_for_task);

                                // Stale session — skip injection; detached task still persists + cleans up.
                                {
                                    let session_state_guard =
                                        session_state_for_task.lock().unwrap();
                                    if !matches!(
                                        *session_state_guard,
                                        SessionState::Transcribing {
                                            session_id: current_id
                                        } if current_id == session_id
                                    ) {
                                        return;
                                    }
                                }

                                if let Ok(raw) = &transcript_result {
                                    let raw = raw.clone();
                                    // Apply learned corrections before smart formatting so
                                    // capitalisation rules run on the final word shape.
                                    let correction_pairs: Vec<(String, String)> =
                                        db_conn_for_task
                                            .lock()
                                            .ok()
                                            .and_then(|conn| {
                                                db::list_correction_pairs(&conn).ok()
                                            })
                                            .unwrap_or_default();
                                    let replaced = corrections::apply_replacements(
                                        &raw,
                                        &correction_pairs,
                                    );
                                    let text = if smart_formatting {
                                        apply_smart_formatting(&replaced)
                                    } else {
                                        replaced
                                    };
                                    {
                                        let mut session_state_guard =
                                            session_state_for_task.lock().unwrap();
                                        *session_state_guard =
                                            SessionState::Injecting { session_id };
                                    }
                                    let _ = app_handle_for_task.emit(
                                        "session-phase",
                                        SessionPhaseEvent { phase: "injecting" },
                                    );
                                    let inject_result = text_inject::inject_text(&text);
                                    if inject_result.is_ok() {
                                        if let Ok(mut guard) = pending_capture_for_task.lock() {
                                            *guard = Some(PendingCapture {
                                                session_id,
                                                dictation_id: dictation_id.clone(),
                                                model: primary_label.clone(),
                                                injected_text: text.clone(),
                                                captured_at: Instant::now(),
                                            });
                                        }
                                    }
                                    let _ = storage::append_log(
                                        "INFO",
                                        &format!(
                                            "Session {session_id} transcribed via {primary_label}, length {}",
                                            text.len()
                                        ),
                                    );
                                    let _ = app_handle_for_task.emit(
                                        "transcription-complete",
                                        TranscriptionCompleteEvent {
                                            session_id,
                                            text: text.clone(),
                                            timestamp: chrono::Utc::now().to_rfc3339(),
                                        },
                                    );
                                    let history_entry = storage::HistoryEntry {
                                        session_id,
                                        text: text.clone(),
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                    };
                                    if let Ok(mut state) = persisted_for_task.lock() {
                                        state.history.insert(0, history_entry.clone());
                                        if state.history.len() > 200 {
                                            state.history.truncate(200);
                                        }
                                    }
                                    if let Ok(conn) = db_conn_for_task.lock() {
                                        let _ = storage::record_history(&conn, &history_entry);
                                    }
                                    if let Err(message) = inject_result {
                                        let _ = app_handle_for_task.emit(
                                            "pipeline-error",
                                            PipelineErrorEvent {
                                                session_id,
                                                stage: "inject",
                                                message: message.clone(),
                                            },
                                        );
                                        let _ = app_handle_for_task.emit(
                                            "session-phase",
                                            SessionPhaseEvent { phase: "error" },
                                        );
                                        let _ = storage::append_log(
                                            "ERROR",
                                            &format!(
                                                "Session {session_id} failed at inject: {message}"
                                            ),
                                        );
                                    }
                                    let _ = app_handle_for_task.emit(
                                        "session-phase",
                                        SessionPhaseEvent { phase: "idle" },
                                    );
                                } else {
                                    let message = transcript_result.unwrap_err();
                                    let _ = app_handle_for_task.emit(
                                        "pipeline-error",
                                        PipelineErrorEvent {
                                            session_id,
                                            stage: "transcribe",
                                            message: message.clone(),
                                        },
                                    );
                                    let _ = app_handle_for_task.emit(
                                        "session-phase",
                                        SessionPhaseEvent { phase: "error" },
                                    );
                                    let _ = storage::append_log(
                                        "ERROR",
                                        &format!(
                                            "Session {session_id} failed at transcribe ({primary_label}): {message}"
                                        ),
                                    );
                                    let _ = app_handle_for_task.emit(
                                        "session-phase",
                                        SessionPhaseEvent { phase: "idle" },
                                    );
                                }

                                let mut session_state_guard =
                                    session_state_for_task.lock().unwrap();
                                if matches!(
                                    *session_state_guard,
                                    SessionState::Transcribing {
                                        session_id: current_id
                                    } if current_id == session_id
                                ) || matches!(
                                    *session_state_guard,
                                    SessionState::Injecting {
                                        session_id: current_id
                                    } if current_id == session_id
                                ) {
                                    *session_state_guard = SessionState::Idle;
                                }
                            });
                        }
                    }
                }
            });

            // Show window on first launch
            if let Some(window) = app.get_webview_window("main") {
                if let Ok(state) = persisted.lock() {
                    apply_window_movable(&window, state.settings.general.window_movable);
                    let _ = apply_window_position(
                        &window,
                        &state.settings.general.window_position,
                    );
                }
                let _ = window.show();
                let _ = window.set_focus();
            }
            if let Ok(state) = persisted.lock() {
                let _ = apply_show_in_dock(&app.handle().clone(), state.settings.general.show_in_dock);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_recording_state,
            set_api_key,
            set_active_provider,
            get_persisted_state,
            save_onboarding_state,
            check_accessibility_permission,
            check_input_monitoring_permission,
            open_accessibility_settings,
            run_injection_test,
            open_logs_folder,
            start_window_drag,
            minimize_window,
            hide_window,
            get_accessibility_help_info,
            reveal_current_executable,
            open_input_monitoring_settings,
            get_app_settings,
            update_app_settings,
            copy_to_clipboard,
            list_snippets,
            save_snippet,
            delete_snippet,
            list_notes,
            save_note,
            delete_note,
            list_installed_models,
            download_model,
            delete_model,
            list_lab_sessions,
            get_top_mistranscribed,
            list_corrections,
            delete_correction,
            update_history_text,
            save_correction_from_edit,
            get_app_version
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, event| {
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                // Terminate immediately with `_exit`, which skips atexit /
                // __cxa_finalize handlers. A normal `exit()` runs ggml's
                // C++ static destructors, and freeing the Metal device there
                // calls ggml_abort → "quit unexpectedly" dialog on every
                // shutdown. All state is persisted eagerly, so skipping
                // destructors is safe.
                let _ = storage::append_log("INFO", "Exit requested — shutting down");
                unsafe { libc::_exit(0) };
            }
        });
}

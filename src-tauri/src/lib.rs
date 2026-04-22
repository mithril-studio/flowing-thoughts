use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    ActivationPolicy, AppHandle, Emitter, Manager, PhysicalPosition, Position, WebviewWindow,
};
use std::process::Command;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const HOLD_TO_START_MS: u64 = 500;

mod audio;
mod db;
mod hotkey;
mod lab;
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
    if settings.language.mode != "system" && settings.language.mode != "en" {
        settings.language.mode = "system".to_string();
    }
    if settings.microphone.input_device.trim().is_empty() {
        settings.microphone.input_device = "system_default".to_string();
    }
    let valid_positions = ["center", "top_left", "top_right", "bottom_left", "bottom_right"];
    if !valid_positions.contains(&settings.general.window_position.as_str()) {
        settings.general.window_position = "center".to_string();
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

    {
        let mut state = persisted
            .inner()
            .lock()
            .map_err(|_| "Persisted state lock poisoned".to_string())?;
        state.settings = next_settings.clone();
        let conn = db_conn
            .inner()
            .lock()
            .map_err(|_| "DB lock poisoned".to_string())?;
        storage::save(&conn, &state)?;
    }

    if hotkey::mode_from_env().is_none() {
        if let Ok(mut mode) = hotkey_mode.inner().lock() {
            *mode = hotkey::HotkeyMode::from_preset(&next_settings.shortcuts.preset);
        }
    }

    let mut warnings = Vec::new();
    if let Some(window) = app.get_webview_window("main") {
        apply_window_movable(&window, next_settings.general.window_movable);
        if let Err(e) = apply_window_position(&window, &next_settings.general.window_position) {
            warnings.push(e);
        }
    }
    if let Err(e) = apply_show_in_dock(&app, next_settings.general.show_in_dock) {
        warnings.push(e);
    }
    if let Err(e) = apply_launch_at_login(next_settings.general.launch_at_login) {
        warnings.push(e);
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
fn check_accessibility_permission() -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        // `prompt: true` registers FlowingThoughts in
        // System Settings > Privacy & Security > Accessibility the first time
        // it's called, and surfaces a system dialog if the user hasn't toggled
        // us on yet. Calling it repeatedly is safe.
        Ok(macos_ax::is_process_trusted(true))
    }
    #[cfg(not(target_os = "macos"))]
    {
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
fn get_model_tally(
    db_conn: tauri::State<'_, Arc<Mutex<rusqlite::Connection>>>,
) -> Result<Vec<db::ModelTally>, String> {
    let conn = db_conn
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    db::model_tally(&conn)
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
    let next_session_id = Arc::new(Mutex::new(1_u64));
    let db_conn = Arc::new(Mutex::new(
        db::open().expect("Failed to initialize SQLite database"),
    ));
    let persisted_state = {
        let conn = db_conn.lock().expect("DB lock poisoned during startup");
        let mut loaded = storage::load(&conn).unwrap_or_default();
        sanitize_settings(&mut loaded.settings);
        loaded
    };
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
                        None => (mpos.x + 24, mpos.y + 48),
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

            thread::spawn(move || {
                let mut active_recording: Option<(u64, audio::ActiveRecording)> = None;
                let mut active_amplitude_stop: Option<Arc<std::sync::atomic::AtomicBool>> = None;
                let mut pending_start_deadline: Option<Instant> = None;

                loop {
                    let recv_result = match pending_start_deadline {
                        Some(deadline) => {
                            let wait = deadline.saturating_duration_since(Instant::now());
                            hotkey_rx.recv_timeout(wait)
                        }
                        None => hotkey_rx
                            .recv()
                            .map_err(|_| RecvTimeoutError::Disconnected),
                    };

                    let event = match recv_result {
                        Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {
                            // Hold threshold elapsed without a cancel — commit the start.
                            pending_start_deadline = None;
                            hotkey::HotkeyEvent::RecordStart
                        }
                        Ok(hotkey::HotkeyEvent::RecordStart) => {
                            // Debounce: arm a deadline instead of starting immediately.
                            if pending_start_deadline.is_none()
                                && matches!(
                                    *shared_session_state.lock().unwrap(),
                                    SessionState::Idle
                                )
                            {
                                pending_start_deadline = Some(
                                    Instant::now() + Duration::from_millis(HOLD_TO_START_MS),
                                );
                            }
                            continue;
                        }
                        Ok(hotkey::HotkeyEvent::RecordStop) => {
                            if pending_start_deadline.take().is_some() {
                                // Released before threshold → never entered recording.
                                continue;
                            }
                            hotkey::HotkeyEvent::RecordStop
                        }
                    };

                    match event {
                        hotkey::HotkeyEvent::RecordStart => {
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
                            if capture.duration_ms < 180 {
                                let _ = app_handle.emit(
                                    "pipeline-error",
                                    PipelineErrorEvent {
                                        session_id,
                                        stage: "audio-finalize",
                                        message: "Recording too short. Hold the hotkey longer and try again."
                                            .to_string(),
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
                                    "WARN",
                                    &format!(
                                        "Session {session_id} dropped due to short duration ({}ms)",
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
                            let wav_path_for_task = capture.wav_path.clone();

                            tauri::async_runtime::spawn(async move {
                                let (
                                    provider,
                                    runtime_api_key,
                                    language_mode,
                                    smart_formatting,
                                    transcription_mode,
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
                                        )
                                    })
                                    .unwrap_or((
                                        storage::Provider::Groq,
                                        None,
                                        "system".to_string(),
                                        true,
                                        "api".to_string(),
                                    ));

                                // Always fan out to all 4 models for data collection.
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

                                let primary_label: String = if transcription_mode == "local" {
                                    "distil-small-en".to_string()
                                } else {
                                    match provider {
                                        storage::Provider::Groq => "groq-api".to_string(),
                                        storage::Provider::Openai => "openai-api".to_string(),
                                    }
                                };

                                let (primary, pending) = lab::run_with_primary_first(
                                    session_id,
                                    wav_path_for_task.clone(),
                                    capture.duration_ms,
                                    provider,
                                    runtime_api_key.clone(),
                                    language_mode.clone(),
                                    primary_label.clone(),
                                )
                                .await;

                                // Detach the loser tasks: await them in the background,
                                // persist all 4 results to the DB, then clean up the WAV.
                                {
                                    let primary_for_detach = primary.clone();
                                    let db_conn_for_detach = db_conn_for_task.clone();
                                    let dictation_id_for_detach = dictation_id.clone();
                                    let wav_path_for_detach = wav_path_for_task.clone();
                                    tauri::async_runtime::spawn(async move {
                                        let losers = pending.join_all().await;
                                        if let Ok(conn) = db_conn_for_detach.lock() {
                                            let all_results = std::iter::once(&primary_for_detach)
                                                .chain(losers.iter());
                                            for r in all_results {
                                                let _ = db::insert_transcription(
                                                    &conn,
                                                    &db::TranscriptionRow {
                                                        id: uuid::Uuid::new_v4().to_string(),
                                                        dictation_id: dictation_id_for_detach
                                                            .clone(),
                                                        model: r.model.clone(),
                                                        text: r.text.clone(),
                                                        latency_ms: Some(r.latency_ms),
                                                        error: r.error.clone(),
                                                        created_at: chrono::Utc::now()
                                                            .to_rfc3339(),
                                                    },
                                                );
                                            }
                                            let _ = db::clear_wav_path(
                                                &conn,
                                                &dictation_id_for_detach,
                                            );
                                        }
                                        let _ = std::fs::remove_file(&wav_path_for_detach);
                                    });
                                }

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

                                if primary.text.is_some() {
                                    let raw = primary.text.clone().unwrap();
                                    let text = if smart_formatting {
                                        apply_smart_formatting(&raw)
                                    } else {
                                        raw
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
                                    let message = primary.error.clone().unwrap_or_else(|| {
                                        format!(
                                            "Primary model {primary_label} produced no output"
                                        )
                                    });
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
            open_accessibility_settings,
            run_injection_test,
            open_logs_folder,
            start_window_drag,
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
            get_model_tally,
            get_top_mistranscribed
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

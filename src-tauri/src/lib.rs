use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    ActivationPolicy, AppHandle, Emitter, Manager, PhysicalPosition, Position, WebviewWindow,
};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

mod audio;
mod hotkey;
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
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return compact;
    }
    let mut chars = compact.chars();
    let first = chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default();
    let rest: String = chars.collect();
    let mut output = format!("{first}{rest}");
    if !output.ends_with('.') && !output.ends_with('!') && !output.ends_with('?') {
        output.push('.');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::apply_smart_formatting;

    #[test]
    fn smart_formatting_normalizes_spacing_and_terminal_punctuation() {
        assert_eq!(
            apply_smart_formatting("hello   world"),
            "Hello world."
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
fn set_openai_api_key(
    key: String,
    persisted: tauri::State<'_, Arc<Mutex<storage::PersistedState>>>,
) -> Result<(), String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err("OpenAI API key cannot be empty".to_string());
    }
    let mut state = persisted
        .inner()
        .lock()
        .map_err(|_| "Persisted state lock poisoned".to_string())?;
    state.openai_api_key = Some(trimmed.to_string());
    storage::save(&state)?;
    Ok(())
}

#[tauri::command]
fn has_openai_api_key(
    persisted: tauri::State<'_, Arc<Mutex<storage::PersistedState>>>,
) -> Result<bool, String> {
    let state = persisted
        .inner()
        .lock()
        .map_err(|_| "Persisted state lock poisoned".to_string())?;
    Ok(state.openai_api_key.is_some())
}

#[derive(Debug, Clone, serde::Serialize)]
struct PersistedStateView {
    onboarding_complete: bool,
    license_key: Option<String>,
    has_openai_api_key: bool,
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
        has_openai_api_key: state.openai_api_key.is_some(),
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
        settings.shortcuts.preset = "cmd_shift_space".to_string();
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
             set existingItems to login items where name is \"Open Voice Wispr\"\n\
             repeat with itemRef in existingItems\n\
             delete itemRef\n\
             end repeat\n\
             make login item at end with properties {{name:\"Open Voice Wispr\", path:\"{exe_path}\", hidden:false}}\n\
             end tell"
        )
    } else {
        "tell application \"System Events\"\n\
         set existingItems to login items where name is \"Open Voice Wispr\"\n\
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
) -> Result<AppSettingsUpdateResult, String> {
    let mut next_settings = settings;
    sanitize_settings(&mut next_settings);

    {
        let mut state = persisted
            .inner()
            .lock()
            .map_err(|_| "Persisted state lock poisoned".to_string())?;
        state.settings = next_settings.clone();
        storage::save(&state)?;
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
    license_key: String,
    onboarding_complete: bool,
    persisted: tauri::State<'_, Arc<Mutex<storage::PersistedState>>>,
) -> Result<(), String> {
    let trimmed = license_key.trim();
    if trimmed.len() < 8 {
        return Err("Please provide a valid license key".to_string());
    }
    let mut state = persisted
        .inner()
        .lock()
        .map_err(|_| "Persisted state lock poisoned".to_string())?;
    state.license_key = Some(trimmed.to_string());
    state.onboarding_complete = onboarding_complete;
    storage::save(&state)?;
    Ok(())
}

#[tauri::command]
fn check_accessibility_permission() -> Result<bool, String> {
    let mut child = Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to return UI elements enabled")
        .spawn()
        .map_err(|e| format!("Failed to start accessibility check: {e}"))?;

    let started = Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(
                "Accessibility check timed out. Open Settings and enable access manually."
                    .to_string(),
            );
        }

        match child.try_wait() {
            Ok(Some(_)) => {
                let output = child
                    .wait_with_output()
                    .map_err(|e| format!("Failed to read accessibility check output: {e}"))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    let message = if stderr.is_empty() {
                        "Unknown AppleScript error".to_string()
                    } else {
                        stderr
                    };
                    return Err(format!("Accessibility check failed: {message}"));
                }
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                return Ok(stdout.trim().eq_ignore_ascii_case("true"));
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("Failed to poll accessibility check: {e}")),
        }
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
    text_inject::inject_text("Open Voice Wispr test successful.")
}

#[tauri::command]
fn open_logs_folder() -> Result<(), String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set".to_string())?;
    let logs_dir = std::path::PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Open Voice Wispr");
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let session_state = Arc::new(Mutex::new(SessionState::Idle));
    let next_session_id = Arc::new(Mutex::new(1_u64));
    let persisted_state = {
        let mut loaded = storage::load().unwrap_or_default();
        sanitize_settings(&mut loaded.settings);
        loaded
    };
    let configured_hotkey_mode =
        hotkey::HotkeyMode::from_preset(&persisted_state.settings.shortcuts.preset);
    let runtime_hotkey_mode = hotkey::mode_from_env().unwrap_or(configured_hotkey_mode);
    let persisted = Arc::new(Mutex::new(persisted_state));
    let hotkey_mode = Arc::new(Mutex::new(runtime_hotkey_mode));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(session_state.clone())
        .manage(persisted.clone())
        .manage(hotkey_mode.clone())
        .setup(move |app| {
            // Build tray menu
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let show =
                MenuItem::with_id(app, "show", "Show Settings", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;

            // Create tray icon
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => {
                        app.exit(0);
                    }
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            // Start fn key listener
            let hotkey_rx = hotkey::start_listener(hotkey_mode.clone());
            let app_handle = app.handle().clone();
            let shared_session_state = session_state.clone();
            let shared_next_session_id = next_session_id.clone();
            let shared_persisted = persisted.clone();

            thread::spawn(move || {
                let mut active_recording: Option<(u64, audio::ActiveRecording)> = None;

                while let Ok(event) = hotkey_rx.recv() {
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
                        }
                        hotkey::HotkeyEvent::RecordStop => {
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

                            tauri::async_runtime::spawn(async move {
                                let (runtime_api_key, language_mode, smart_formatting) =
                                    persisted_for_task
                                        .lock()
                                        .ok()
                                        .map(|state| {
                                            (
                                                state.openai_api_key.clone(),
                                                state.settings.language.mode.clone(),
                                                state.settings.extras.smart_formatting,
                                            )
                                        })
                                        .unwrap_or((None, "system".to_string(), true));

                                match transcribe::transcribe_audio(
                                    session_id,
                                    &capture.wav_path,
                                    capture.duration_ms,
                                    runtime_api_key.as_deref(),
                                    Some(&language_mode),
                                )
                                .await
                                {
                                    Ok(text) => {
                                        let text = if smart_formatting {
                                            apply_smart_formatting(&text)
                                        } else {
                                            text
                                        };
                                        {
                                            let mut session_state_guard =
                                                session_state_for_task.lock().unwrap();
                                            if !matches!(
                                                *session_state_guard,
                                                SessionState::Transcribing {
                                                    session_id: current_id
                                                } if current_id == session_id
                                            ) {
                                                // Late response from stale session, ignore.
                                                return;
                                            }
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
                                                "Session {session_id} transcribed successfully, length {}",
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
                                        if let Ok(mut state) = persisted_for_task.lock() {
                                            state.history.insert(
                                                0,
                                                storage::HistoryEntry {
                                                    session_id,
                                                    text: text.clone(),
                                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                                },
                                            );
                                            if state.history.len() > 200 {
                                                state.history.truncate(200);
                                            }
                                            let _ = storage::save(&state);
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
                                    }
                                    Err(message) => {
                                        let is_current = {
                                            let session_state_guard =
                                                session_state_for_task.lock().unwrap();
                                            matches!(
                                                *session_state_guard,
                                                SessionState::Transcribing {
                                                    session_id: current_id
                                                } if current_id == session_id
                                            )
                                        };
                                        if !is_current {
                                            return;
                                        }
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
                                                "Session {session_id} failed at transcribe: {message}"
                                            ),
                                        );
                                        let _ = app_handle_for_task.emit(
                                            "session-phase",
                                            SessionPhaseEvent { phase: "idle" },
                                        );
                                    }
                                }

                                let mut session_state_guard = session_state_for_task.lock().unwrap();
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
            set_openai_api_key,
            has_openai_api_key,
            get_persisted_state,
            save_onboarding_state,
            check_accessibility_permission,
            open_accessibility_settings,
            run_injection_test,
            open_logs_folder,
            get_app_settings,
            update_app_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

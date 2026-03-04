use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager,
};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

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
    let output = Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to return UI elements enabled")
        .output()
        .map_err(|e| format!("Failed to check accessibility permission: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(format!("Accessibility check failed: {stderr}"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(stdout.trim().eq_ignore_ascii_case("true"))
}

#[tauri::command]
fn open_accessibility_settings() -> Result<(), String> {
    let status = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .status()
        .map_err(|e| format!("Failed to open accessibility settings: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Open accessibility settings command failed with status: {status}"
        ))
    }
}

#[tauri::command]
fn run_injection_test() -> Result<(), String> {
    text_inject::inject_text("Open Voice Wispr test successful.")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let session_state = Arc::new(Mutex::new(SessionState::Idle));
    let next_session_id = Arc::new(Mutex::new(1_u64));
    let persisted = Arc::new(Mutex::new(storage::load().unwrap_or_default()));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(session_state.clone())
        .manage(persisted.clone())
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
            let hotkey_rx = hotkey::start_listener();
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
                                let runtime_api_key = persisted_for_task
                                    .lock()
                                    .ok()
                                    .and_then(|state| state.openai_api_key.clone());

                                match transcribe::transcribe_audio(
                                    session_id,
                                    &capture.wav_path,
                                    capture.duration_ms,
                                    runtime_api_key.as_deref(),
                                )
                                .await
                                {
                                    Ok(text) => {
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
                                                    message,
                                                },
                                            );
                                            let _ = app_handle_for_task.emit(
                                                "session-phase",
                                                SessionPhaseEvent { phase: "error" },
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
                                                message,
                                            },
                                        );
                                        let _ = app_handle_for_task.emit(
                                            "session-phase",
                                            SessionPhaseEvent { phase: "error" },
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
                let _ = window.show();
                let _ = window.set_focus();
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
            run_injection_test
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

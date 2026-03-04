use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager,
};
use std::sync::{Arc, Mutex};
use std::thread;

mod audio;
mod hotkey;
mod text_inject;
mod transcribe;

enum SessionState {
    Idle,
    Recording { session_id: u64 },
    Transcribing { session_id: u64 },
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
    message: String,
}

#[tauri::command]
fn get_recording_state(state: tauri::State<'_, Arc<Mutex<SessionState>>>) -> bool {
    matches!(
        *state.inner().lock().unwrap(),
        SessionState::Recording { .. }
    )
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let session_state = Arc::new(Mutex::new(SessionState::Idle));
    let next_session_id = Arc::new(Mutex::new(1_u64));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(session_state.clone())
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
                                        PipelineErrorEvent { session_id, message },
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
                                        PipelineErrorEvent { session_id, message },
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

                            tauri::async_runtime::spawn(async move {
                                match transcribe::transcribe_placeholder(
                                    session_id,
                                    &capture.wav_path,
                                    capture.duration_ms,
                                )
                                .await
                                {
                                    Ok(text) => {
                                        let _ = app_handle_for_task.emit(
                                            "transcription-complete",
                                            TranscriptionCompleteEvent {
                                                session_id,
                                                text,
                                                timestamp: chrono::Utc::now().to_rfc3339(),
                                            },
                                        );
                                        let _ = app_handle_for_task.emit(
                                            "session-phase",
                                            SessionPhaseEvent { phase: "idle" },
                                        );
                                    }
                                    Err(message) => {
                                        let _ = app_handle_for_task.emit(
                                            "pipeline-error",
                                            PipelineErrorEvent { session_id, message },
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
        .invoke_handler(tauri::generate_handler![get_recording_state])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

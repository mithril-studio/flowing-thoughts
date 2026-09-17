//! Every meeting `#[tauri::command]`. Owned by WP1 (scaffold) and final.
//!
//! Each command is a thin delegate to the module that owns the behaviour, so
//! no later package edits this file or `generate_handler!` in `lib.rs`:
//!
//! - `store` functions are plain SQL over `&Connection`: the command locks the
//!   managed connection, calls one function and emits `meeting-updated`.
//! - Everything else takes the `AppHandle` and does its own locking and
//!   emitting inside the owning module.
//!
//! Commands that can block (device setup, file deletion, a permission probe)
//! are `command(async)` so they stay off the main thread. No lock is held
//! across an `.await`.

use tauri::{AppHandle, State};

use super::types::{
    JobProgress, MeetingChange, MeetingDetail, MeetingExport, MeetingListItem, MeetingSummary,
    PermissionStatus, RecordingStatus, RetranscribeOptions, Segment, StartMeetingOptions,
};
use super::{capture, events, export, jobs, session, store, summary, DbState, PersistedHandle};

fn with_conn<T>(
    db: &State<'_, DbState>,
    f: impl FnOnce(&rusqlite::Connection) -> Result<T, String>,
) -> Result<T, String> {
    let conn = db
        .inner()
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())?;
    f(&conn)
}

// --- Availability and permission (capture, WP5) -----------------------------

/// The OS gate: process taps need macOS 14.4+. Dictation is unaffected.
#[tauri::command]
pub fn meetings_supported() -> Result<bool, String> {
    Ok(capture::is_supported())
}

#[tauri::command(async)]
pub fn check_system_audio_permission() -> Result<PermissionStatus, String> {
    Ok(capture::permission::check())
}

#[tauri::command]
pub fn open_system_audio_settings() -> Result<(), String> {
    capture::permission::open_settings()
}

// --- Recording (session, WP7) -----------------------------------------------

#[tauri::command(async)]
pub fn start_meeting(
    app: AppHandle,
    options: Option<StartMeetingOptions>,
) -> Result<RecordingStatus, String> {
    session::start(&app, options.unwrap_or_default())
}

#[tauri::command(async)]
pub fn pause_meeting(app: AppHandle) -> Result<RecordingStatus, String> {
    session::pause(&app)
}

#[tauri::command(async)]
pub fn resume_meeting(app: AppHandle) -> Result<RecordingStatus, String> {
    session::resume(&app)
}

#[tauri::command(async)]
pub fn stop_meeting(app: AppHandle) -> Result<RecordingStatus, String> {
    session::stop(&app)
}

#[tauri::command]
pub fn get_meeting_recording_status(app: AppHandle) -> Result<RecordingStatus, String> {
    session::status(&app)
}

/// Removes the rows and the audio. Refused while the meeting is recording.
#[tauri::command(async)]
pub fn delete_meeting(app: AppHandle, meeting_id: String) -> Result<(), String> {
    session::delete_meeting(&app, &meeting_id)
}

/// Removes the audio, keeps the transcript. Refused while the meeting is
/// recording or still has an unfinished transcription job.
#[tauri::command(async)]
pub fn delete_meeting_audio(app: AppHandle, meeting_id: String) -> Result<(), String> {
    session::delete_meeting_audio(&app, &meeting_id)
}

// --- Reading and editing (store, WP2) ---------------------------------------

#[tauri::command]
pub fn list_meetings(db: State<'_, DbState>) -> Result<Vec<MeetingListItem>, String> {
    with_conn(&db, store::list_meetings)
}

#[tauri::command]
pub fn get_meeting(db: State<'_, DbState>, meeting_id: String) -> Result<MeetingDetail, String> {
    with_conn(&db, |conn| store::get_meeting(conn, &meeting_id))?
        .ok_or_else(|| format!("Meeting '{meeting_id}' not found"))
}

#[tauri::command]
pub fn rename_meeting(
    app: AppHandle,
    db: State<'_, DbState>,
    meeting_id: String,
    title: String,
) -> Result<(), String> {
    with_conn(&db, |conn| store::rename_meeting(conn, &meeting_id, &title))?;
    events::emit_updated(&app, &meeting_id, MeetingChange::Renamed);
    Ok(())
}

/// Segments of one run, hidden ones included (the UI folds them away).
/// `run_id: None` means the meeting's active run.
#[tauri::command]
pub fn list_meeting_segments(
    db: State<'_, DbState>,
    meeting_id: String,
    run_id: Option<String>,
) -> Result<Vec<Segment>, String> {
    with_conn(&db, |conn| store::list_segments(conn, &meeting_id, run_id.as_deref()))
}

/// `text: None` (or blank) drops the edit and shows the decoded text again.
#[tauri::command]
pub fn edit_meeting_segment_text(
    app: AppHandle,
    db: State<'_, DbState>,
    segment_id: String,
    text: Option<String>,
) -> Result<Segment, String> {
    let text = text.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let segment = with_conn(&db, |conn| store::set_segment_text(conn, &segment_id, text))?;
    events::emit_updated(&app, &segment.meeting_id, MeetingChange::Segment);
    Ok(segment)
}

/// The user's own hide/show choice. It overrides the pipeline's flag both
/// ways: `false` on a flagged segment brings it back.
#[tauri::command]
pub fn set_meeting_segment_hidden(
    app: AppHandle,
    db: State<'_, DbState>,
    segment_id: String,
    hidden: bool,
) -> Result<Segment, String> {
    let segment = with_conn(&db, |conn| store::set_segment_hidden(conn, &segment_id, hidden))?;
    events::emit_updated(&app, &segment.meeting_id, MeetingChange::Segment);
    Ok(segment)
}

// --- Transcription jobs (jobs, WP6) -----------------------------------------

/// Queues a new run; the existing runs and their segments are kept.
#[tauri::command(async)]
pub fn retranscribe_meeting(
    app: AppHandle,
    meeting_id: String,
    options: Option<RetranscribeOptions>,
) -> Result<JobProgress, String> {
    jobs::retranscribe(&app, &meeting_id, options.unwrap_or_default())
}

// --- Summary and export (WP10) ----------------------------------------------

/// Opt-in, bring-your-own-key. Nothing leaves the machine unless the feature
/// is on in Settings, a key is set, and the user confirmed for *this* meeting.
#[tauri::command]
pub async fn generate_meeting_summary(
    app: AppHandle,
    persisted: State<'_, PersistedHandle>,
    meeting_id: String,
    confirmed: bool,
) -> Result<MeetingSummary, String> {
    if !confirmed {
        return Err(
            "A summary sends this meeting's transcript to OpenRouter. Confirm before generating one."
                .to_string(),
        );
    }
    // Read the key and config, then release the lock before the await.
    let (api_key, model) = {
        let state = persisted
            .inner()
            .lock()
            .map_err(|_| "Persisted state lock poisoned".to_string())?;
        if !state.settings.meetings.summary_enabled {
            return Err("Meeting summaries are turned off. Enable them in Settings → Meetings.".to_string());
        }
        let key = state
            .openrouter_api_key
            .clone()
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| {
                "No OpenRouter API key configured. Add one in Settings → Meetings.".to_string()
            })?;
        (key, state.settings.meetings.summary_model.clone())
    };

    let summary = summary::generate(&app, &meeting_id, &model, &api_key).await?;
    events::emit_updated(&app, &meeting_id, MeetingChange::Summary);
    Ok(summary)
}

/// The latest summary of the meeting, if one was ever generated.
#[tauri::command]
pub fn get_meeting_summary(
    db: State<'_, DbState>,
    meeting_id: String,
) -> Result<Option<MeetingSummary>, String> {
    with_conn(&db, |conn| summary::latest(conn, &meeting_id))
}

/// Returns the Markdown; the frontend decides where it goes.
#[tauri::command(async)]
pub fn export_meeting_markdown(
    db: State<'_, DbState>,
    meeting_id: String,
) -> Result<MeetingExport, String> {
    with_conn(&db, |conn| export::export_markdown(conn, &meeting_id))
}

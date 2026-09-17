//! OWNER: WP7 (session). The recording state machine, the tray title and
//! launch recovery.
//!
//! Every function below is called by `commands.rs` or `meetings/mod.rs` and
//! its signature is fixed. WP7 replaces the bodies and keeps its state in
//! Tauri managed state (`app.manage(..)` from `init`).
//!
//! Phases: `idle -> starting -> recording <-> paused -> stopping -> idle`
//! (`types::RecordingPhase`). One meeting at a time. Every transition emits
//! `meeting-state`; creating, finishing and deleting a meeting emit
//! `meeting-updated`. Release every lock before doing slow work.
//!
//! - `start`: refuse unless `settings.meetings.enabled`, `capture::is_supported()`
//!   and idle. Insert the meeting, its tracks and the "Me"/"Them" speakers,
//!   build the sources (`capture`) and recorders (`recording`), bridge
//!   `types::ChunkLedger` to `store`. No system audio means a mic-only
//!   meeting, never an error. Set `echo_risk` from `capture::device_watch`.
//! - `stop`: drain and close the chunks, set `ended_at` / `duration_ms`,
//!   status `queued`, then `jobs::enqueue_transcription`.
//! - Tray: while a meeting records, the title is the recording dot plus the
//!   duration for the whole meeting. Set it on the icon with id
//!   `crate::TRAY_ICON_ID`; dictation's own title wins while a dictation is
//!   active and falls back to `tray_title()` afterwards.
//! - Dictation keeps working during a meeting.

// Scaffold: remove once WP7 implements this file.
#![allow(dead_code)]

use tauri::AppHandle;

use super::not_implemented;
use super::types::{RecordingStatus, StartMeetingOptions};

const OWNER: &str = "WP7 session";

/// Launch recovery, before the worker starts: meetings left in `recording`
/// or `paused` become `interrupted`, their `open` chunks are repaired
/// (`recording::recovery`), and each gets a transcription job.
pub fn init(_app: &AppHandle) -> Result<(), String> {
    Ok(())
}

pub fn start(_app: &AppHandle, _options: StartMeetingOptions) -> Result<RecordingStatus, String> {
    not_implemented("Starting a meeting", OWNER)
}

pub fn pause(_app: &AppHandle) -> Result<RecordingStatus, String> {
    not_implemented("Pausing a meeting", OWNER)
}

pub fn resume(_app: &AppHandle) -> Result<RecordingStatus, String> {
    not_implemented("Resuming a meeting", OWNER)
}

pub fn stop(_app: &AppHandle) -> Result<RecordingStatus, String> {
    not_implemented("Stopping a meeting", OWNER)
}

/// The live status. Idle is a status, not an error.
pub fn status(_app: &AppHandle) -> Result<RecordingStatus, String> {
    Ok(RecordingStatus::idle())
}

/// Rows (cascade) and audio directory. Refused for the meeting being
/// recorded; cancels its unfinished job first.
pub fn delete_meeting(_app: &AppHandle, _meeting_id: &str) -> Result<(), String> {
    not_implemented("Deleting a meeting", OWNER)
}

/// Audio files only: chunks become `deleted`, `audio_deleted_at` is set, the
/// transcript stays. Refused while recording or while a job is unfinished.
pub fn delete_meeting_audio(_app: &AppHandle, _meeting_id: &str) -> Result<(), String> {
    not_implemented("Deleting a meeting's audio", OWNER)
}

/// Menu bar title while a meeting records (e.g. "● 12:34"), else `None`.
/// Called from the `session-phase` listener in `lib.rs`: cheap, non-blocking.
pub fn tray_title() -> Option<String> {
    None
}

/// Best-effort clean close right before `_exit(0)`. Must return within a
/// second. Correctness never depends on it: launch recovery handles a crash.
pub fn shutdown() {}

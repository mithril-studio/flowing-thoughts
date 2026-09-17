//! The three events the frontend renders. Owned by WP1 (scaffold).
//!
//! | Event | Payload | Emitted by |
//! |---|---|---|
//! | `meeting-state` | `RecordingStatus` | session (WP7), on every phase change |
//! | `meeting-job-progress` | `JobProgress` | worker (WP6), per window and on status change |
//! | `meeting-updated` | `MeetingUpdated` | whoever changed a meeting's rows |
//!
//! `meeting-state` is not a clock: the UI ticks the duration itself from
//! `elapsed_ms`. Emit failures are ignored on purpose — a closed window must
//! never fail a recording.

use tauri::{AppHandle, Emitter};

use super::types::{JobProgress, MeetingChange, RecordingStatus};

pub const MEETING_STATE: &str = "meeting-state";
pub const MEETING_JOB_PROGRESS: &str = "meeting-job-progress";
pub const MEETING_UPDATED: &str = "meeting-updated";

/// Payload of `meeting-updated`: refetch this meeting (or drop it, on
/// `deleted`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeetingUpdated {
    pub meeting_id: String,
    pub change: MeetingChange,
}

pub fn emit_state(app: &AppHandle, status: &RecordingStatus) {
    let _ = app.emit(MEETING_STATE, status);
}

pub fn emit_job_progress(app: &AppHandle, progress: &JobProgress) {
    let _ = app.emit(MEETING_JOB_PROGRESS, progress);
}

pub fn emit_updated(app: &AppHandle, meeting_id: &str, change: MeetingChange) {
    let _ = app.emit(
        MEETING_UPDATED,
        MeetingUpdated {
            meeting_id: meeting_id.to_string(),
            change,
        },
    );
}

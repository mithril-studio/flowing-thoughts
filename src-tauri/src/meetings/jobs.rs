//! OWNER: WP6 (jobs and worker). The SQLite-backed job queue.
//!
//! The function below is called by `commands.rs` and its signature is fixed.
//! Beyond it this file must provide:
//!
//! - `enqueue_transcription(conn, meeting_id, ..)` for the session (WP7) to
//!   call at stop and after launch recovery: creates the `transcript_runs`
//!   row (model and params recorded, so a re-run never destroys results) and
//!   the `jobs` row, both `queued`.
//! - Launch repair: every `running` job goes back to `queued`.
//! - Claiming the next job by `priority`, then `created_at`.
//!
//! Anything that adds a job calls `worker::wake()`.

// Scaffold: remove once WP6 implements this file.
#![allow(dead_code)]

use tauri::AppHandle;

use super::not_implemented;
use super::types::{JobProgress, RetranscribeOptions};

/// "Re-transcribe as…": a new run and job for a recorded meeting. Refused
/// while the meeting is recording, has an unfinished job, or has no audio
/// left. Emits `meeting-job-progress` and wakes the worker.
pub fn retranscribe(
    _app: &AppHandle,
    _meeting_id: &str,
    _options: RetranscribeOptions,
) -> Result<JobProgress, String> {
    not_implemented("Re-transcribing a meeting", "WP6 jobs")
}

//! OWNER: WP6 (jobs and worker). The one `meeting-worker` thread.
//!
//! The functions below are called by `meetings::init` and by `jobs.rs`; their
//! signatures are fixed.
//!
//! - One thread, its own connection from `db::open_connection()` (WAL and
//!   `busy_timeout` make that safe). It never takes the managed connection's
//!   mutex.
//! - Per job: plan windows (`longform.rs`) if the run has none, then decode
//!   the `pending` ones in order. A window is the unit of resume.
//! - Per window: `inference_gate::acquire_background()`, decode, drop the
//!   guard. The gate is held for one window, never for a job. When
//!   `should_preempt()` aborts the decode the window stays `pending` and the
//!   next `acquire_background()` waits until dictation is done.
//! - After both tracks are done: `echo::find_echo_segments`, write the flags,
//!   mark the run `done`, make it the meeting's active run, set the meeting
//!   `ready`.
//! - Emits `meeting-job-progress` per window and `meeting-updated` when the
//!   transcript or status changes.
//! - Applies `settings.meetings.auto_delete_audio_days`.
//!
//! Tests: job resume with a fake decoder (kill after N windows, restart,
//! nothing decoded twice); preemption leaves the window `pending`.

// Scaffold: remove once WP6 implements this file.
#![allow(dead_code)]

use tauri::AppHandle;

/// Spawns the worker thread. Called once at launch, after launch recovery.
pub fn start(_app: &AppHandle) -> Result<(), String> {
    Ok(())
}

/// Tells the worker there may be a new job. Cheap, never blocks, and a no-op
/// before `start`.
pub fn wake() {}

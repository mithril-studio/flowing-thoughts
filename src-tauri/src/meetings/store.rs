//! OWNER: WP2 (store). Typed access to every v3 table.
//!
//! Conventions (same as `db.rs`): plain `pub fn`s taking `&Connection`,
//! `Result<T, String>`, RFC3339 text timestamps, UUID text ids, enum columns
//! written with `as_str()` and read with `parse()`. No locking and no events
//! in here — callers own both. Every function must work on the UI connection
//! and on the worker's own connection alike.
//!
//! The functions below are called by `commands.rs` and their signatures are
//! fixed. WP2 replaces the bodies and adds the rest of the API that WP6, WP7
//! and WP10 build on. At minimum:
//!
//! - meetings: `insert_meeting`, `set_meeting_status`, `finish_meeting`
//!   (ended_at, duration_ms), `set_active_run`, `set_echo_risk`,
//!   `mark_audio_deleted`, `delete_meeting`, `list_meetings_with_status`
//! - tracks and chunks: `insert_track`, `add_track_overflow`, `insert_chunk`,
//!   `close_chunk`, `list_chunks(track_id)`, `list_open_chunks`,
//!   `mark_chunk_recovered`, `mark_chunks_deleted(meeting_id)` — shaped so a
//!   `types::ChunkLedger` is a few lines on top (WP7 writes that adapter)
//! - runs, windows, segments: `insert_run`, `set_run_status`,
//!   `insert_windows`, `list_pending_windows`, `complete_window` (the window
//!   row and its segments in one transaction — the unit of resume),
//!   `fail_window`, `set_suppressed_reason` (for `echo.rs` results)
//! - speakers: `seed_track_speakers` ("Me" for mic, "Them" for system,
//!   `source = 'track'`). A segment's label is its `segment_speakers` row if
//!   one exists, else the track speaker of its track; an assigned
//!   participant's name wins over the speaker's label
//! - jobs: `insert_job`, `next_queued_job`, `set_job_status`,
//!   `set_job_progress`, `requeue_running_jobs`, `job_progress(meeting_id)`
//! - summaries: `insert_summary` (with items and sources, one transaction),
//!   `latest_summary(meeting_id)`
//!
//! Displayed text is `COALESCE(edit.text, seg.text)`; `hidden` is
//! `COALESCE(edit.hidden, seg.suppressed_reason IS NOT NULL)`.
//! `transcript_segments` rows are immutable except for `suppressed_reason`.

// Scaffold: remove once WP2 implements this file.
#![allow(dead_code)]

use rusqlite::Connection;

use super::not_implemented;
use super::types::{MeetingDetail, MeetingListItem, Segment};

const OWNER: &str = "WP2 store";

/// Newest first. Each item carries its unfinished job, if any.
pub fn list_meetings(_conn: &Connection) -> Result<Vec<MeetingListItem>, String> {
    Ok(Vec::new())
}

/// `None` when there is no such meeting.
pub fn get_meeting(_conn: &Connection, _meeting_id: &str) -> Result<Option<MeetingDetail>, String> {
    Ok(None)
}

/// Trims the title; rejects an empty one and an unknown meeting.
pub fn rename_meeting(_conn: &Connection, _meeting_id: &str, _title: &str) -> Result<(), String> {
    not_implemented("Renaming a meeting", OWNER)
}

/// All segments of a run in timeline order, both tracks interleaved, hidden
/// ones included. `run_id: None` means the meeting's active run; a meeting
/// without one has no segments.
pub fn list_segments(
    _conn: &Connection,
    _meeting_id: &str,
    _run_id: Option<&str>,
) -> Result<Vec<Segment>, String> {
    Ok(Vec::new())
}

/// Upserts `segment_edits.text`. `None` clears the edit. Returns the segment
/// as it now displays.
pub fn set_segment_text(
    _conn: &Connection,
    _segment_id: &str,
    _text: Option<&str>,
) -> Result<Segment, String> {
    not_implemented("Editing a segment", OWNER)
}

/// Upserts `segment_edits.hidden`. Returns the segment as it now displays.
pub fn set_segment_hidden(
    _conn: &Connection,
    _segment_id: &str,
    _hidden: bool,
) -> Result<Segment, String> {
    not_implemented("Hiding a segment", OWNER)
}

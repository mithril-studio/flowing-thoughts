//! OWNER: WP10 (summary). Opt-in, bring-your-own-key cloud summary.
//!
//! The functions below are called by `commands.rs` and their signatures are
//! fixed. `commands.rs` has already checked the three opt-ins before
//! `generate` runs: `settings.meetings.summary_enabled`, a configured
//! OpenRouter key, and the user's confirmation for this meeting. This file
//! must never reach the network any other way — keep a test for that.
//!
//! Follow `coach.rs`: OpenRouter chat completions, shared `reqwest` client,
//! the key passed in and never logged.
//!
//! - Input is the active run's visible segments (`hidden = false`) with their
//!   ids and speaker labels.
//! - Long transcripts are summarized in sections and then consolidated.
//! - Every item must cite existing segment ids: drop citations that do not
//!   exist, drop items left without one. Unknown owners and due dates stay
//!   `None`; never invent them.
//! - Persist through `store` (summary, items and sources in one transaction).
//!   Take the managed connection from `app.state::<DbState>()` and release
//!   the lock before every `.await`.

// Scaffold: remove once WP10 implements this file.
#![allow(dead_code)]

use rusqlite::Connection;
use tauri::AppHandle;

use super::not_implemented;
use super::types::MeetingSummary;

pub async fn generate(
    _app: &AppHandle,
    _meeting_id: &str,
    _model: &str,
    _api_key: &str,
) -> Result<MeetingSummary, String> {
    not_implemented("Generating a meeting summary", "WP10 summary")
}

/// The meeting's most recent summary, with its items and their sources.
pub fn latest(_conn: &Connection, _meeting_id: &str) -> Result<Option<MeetingSummary>, String> {
    Ok(None)
}

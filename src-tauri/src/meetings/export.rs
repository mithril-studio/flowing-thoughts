//! OWNER: WP10 (summary and export). A meeting as Markdown.
//!
//! The function below is called by `commands.rs` and its signature is fixed.
//! It only reads, and returns the text: the frontend decides whether that
//! becomes a clipboard copy or a file.
//!
//! - Title, date, duration; the summary (overview, decisions, actions with
//!   owner and due date when known, topics) if there is one; then the
//!   transcript of the active run.
//! - Transcript lines: `**Me** [12:34] text`, consecutive segments of one
//!   speaker merged into a paragraph. Edited text wins. Hidden segments are
//!   left out.
//! - `file_name` is `<YYYY-MM-DD> <title>.md`, made safe for a file system.
//!
//! Keep the rendering a pure function of store DTOs so it tests without a DB.

// Scaffold: remove once WP10 implements this file.
#![allow(dead_code)]

use rusqlite::Connection;

use super::not_implemented;
use super::types::MeetingExport;

pub fn export_markdown(_conn: &Connection, _meeting_id: &str) -> Result<MeetingExport, String> {
    not_implemented("Exporting a meeting", "WP10 export")
}

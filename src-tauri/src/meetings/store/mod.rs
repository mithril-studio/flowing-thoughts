//! OWNER: WP2 (store). Typed access to every v3 table, and the only place
//! meeting SQL lives: every other package calls in here instead.
//!
//! Conventions (same as `db.rs`): plain `pub fn`s taking `&Connection`,
//! `Result<T, String>`, RFC3339 text timestamps, UUID text ids, enum columns
//! written with `as_str()` and read with `parse()`. No locking, no events and
//! no logging in here — callers own all three. Every function works on the UI
//! connection and on the worker's own connection alike.
//!
//! | File | Tables |
//! |---|---|
//! | `meetings.rs` | `meetings` |
//! | `audio.rs` | `meeting_tracks`, `meeting_audio_chunks` |
//! | `transcript.rs` | `transcript_runs`, `transcript_windows`, `transcript_segments`, `segment_edits` |
//! | `speakers.rs` | `speakers`, `speaker_turns`, `segment_speakers` |
//! | `people.rs` | `people`, `participants`, `speaker_assignments` |
//! | `summaries.rs` | `summaries`, `summary_items`, `summary_item_sources` |
//! | `jobs.rs` | `jobs` |
//!
//! Everything is re-exported, so callers write `store::insert_meeting(..)`.
//!
//! Rules the queries keep:
//! - Displayed text is `COALESCE(edit.text, seg.text)`; `hidden` is
//!   `COALESCE(edit.hidden, seg.suppressed_reason IS NOT NULL)`.
//!   `transcript_segments` rows are immutable except for `suppressed_reason`.
//! - A segment's speaker is its `segment_speakers` row if one exists, else the
//!   track speaker of its track ("Me" for mic, "Them" for system). A confirmed
//!   assignment's participant name wins over the speaker's own label.
//! - Multi-statement writes run inside `transaction`, which nests, so a caller
//!   can wrap several store calls into one unit of its own.

// Scaffold: most of this is first used by WP6, WP7 and WP10, and a re-export
// nobody uses yet counts as an unused import. Remove when they land.
#![allow(dead_code, unused_imports)]

mod audio;
mod jobs;
mod meetings;
mod people;
mod speakers;
mod summaries;
mod transcript;

#[cfg(test)]
mod tests;

pub use audio::*;
pub use jobs::*;
pub use meetings::*;
pub use people::*;
pub use speakers::*;
pub use summaries::*;
pub use transcript::*;

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Params, Row};

/// Runs `f` atomically: everything it writes is committed together, or rolled
/// back when it returns `Err`.
///
/// The outermost call is `BEGIN IMMEDIATE`, so the write lock is taken up
/// front and `busy_timeout` covers the wait for the other connection (a
/// deferred transaction that reads first can fail with SQLITE_BUSY without
/// waiting). Inside an open transaction it is a savepoint, so store functions
/// that use it compose into a caller's own `transaction`.
pub fn transaction<T>(
    conn: &Connection,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let (begin, commit, rollback) = if conn.is_autocommit() {
        ("BEGIN IMMEDIATE", "COMMIT", "ROLLBACK")
    } else {
        (
            "SAVEPOINT meetings_store",
            "RELEASE meetings_store",
            "ROLLBACK TO meetings_store; RELEASE meetings_store",
        )
    };
    conn.execute_batch(begin)
        .map_err(|e| format!("Failed to begin transaction: {e}"))?;
    let result = f().and_then(|value| {
        conn.execute_batch(commit)
            .map_err(|e| format!("Failed to commit transaction: {e}"))?;
        Ok(value)
    });
    if result.is_err() {
        // Best effort: the error worth reporting is the one that got us here.
        let _ = conn.execute_batch(rollback);
    }
    result
}

// --- Shared helpers ----------------------------------------------------------

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The name an assigned participant gives a speaker. Needs the `par` and
/// `per` aliases from `ASSIGNMENT_JOINS`.
const ASSIGNED_NAME_SQL: &str = "COALESCE(NULLIF(TRIM(par.name), ''), \
     NULLIF(TRIM(per.display_name), ''), NULLIF(TRIM(par.email), ''))";

/// Joins a speaker aliased `spk` to its participant and person. A suggested
/// assignment never names anyone: suggestions are shown, not applied.
const ASSIGNMENT_JOINS: &str = "LEFT JOIN speaker_assignments sa \
       ON sa.speaker_id = spk.id AND sa.source != 'suggested' \
     LEFT JOIN participants par ON par.id = sa.participant_id \
     LEFT JOIN people per ON per.id = par.person_id";

fn execute<P: Params>(conn: &Connection, what: &str, sql: &str, params: P) -> Result<usize, String> {
    conn.execute(sql, params)
        .map_err(|e| format!("Failed to {what}: {e}"))
}

/// For updates addressed at one row by id: no row changed means no such row.
fn expect_found(changed: usize, entity: &str, id: &str) -> Result<(), String> {
    if changed == 0 {
        return Err(format!("{entity} '{id}' not found"));
    }
    Ok(())
}

fn query_all<T, P: Params>(
    conn: &Connection,
    what: &str,
    sql: &str,
    params: P,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, String> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to prepare {what}: {e}"))?;
    let rows = stmt
        .query_map(params, map)
        .map_err(|e| format!("Failed to {what}: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Failed to {what}: {e}"))?);
    }
    Ok(out)
}

fn query_opt<T, P: Params>(
    conn: &Connection,
    what: &str,
    sql: &str,
    params: P,
    map: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Option<T>, String> {
    conn.query_row(sql, params, map)
        .optional()
        .map_err(|e| format!("Failed to {what}: {e}"))
}

/// Reads an enum column through the enum's own `parse`. An unknown value is a
/// conversion error, like any other malformed column.
fn enum_col<T>(row: &Row<'_>, idx: usize, parse: fn(&str) -> Option<T>) -> rusqlite::Result<T> {
    let value: String = row.get(idx)?;
    parse(&value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            Type::Text,
            format!("unknown value '{value}'").into(),
        )
    })
}

fn opt_enum_col<T>(
    row: &Row<'_>,
    idx: usize,
    parse: fn(&str) -> Option<T>,
) -> rusqlite::Result<Option<T>> {
    let value: Option<String> = row.get(idx)?;
    match value {
        None => Ok(None),
        Some(value) => parse(&value).map(Some).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                idx,
                Type::Text,
                format!("unknown value '{value}'").into(),
            )
        }),
    }
}

fn u64_col(row: &Row<'_>, idx: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(idx)?;
    Ok(value.max(0) as u64)
}

fn u32_col(row: &Row<'_>, idx: usize) -> rusqlite::Result<u32> {
    let value: i64 = row.get(idx)?;
    Ok(value.clamp(0, u32::MAX as i64) as u32)
}

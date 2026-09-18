//! `summaries`, `summary_items` and `summary_item_sources`.
//!
//! The flow is two steps so a request in flight is on disk before the network
//! call: `insert_summary` writes a `pending` row, then `complete_summary`
//! (overview, items and their sources, one transaction) or `fail_summary`.

use rusqlite::{params, Connection, Row};

use super::super::types::{MeetingSummary, SummaryItem, SummaryItemKind, SummaryStatus};
use super::{enum_col, execute, expect_found, new_id, now, query_all, query_opt, transaction};

#[derive(Debug, Clone)]
pub struct NewSummary {
    pub meeting_id: String,
    /// The run whose segments were summarized.
    pub run_id: String,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct NewSummaryItem {
    pub kind: SummaryItemKind,
    pub text: String,
    /// Unknown stays `None`; never invented.
    pub owner: Option<String>,
    /// The attendee behind `owner`, when it could be matched.
    pub owner_participant_id: Option<String>,
    pub due_date: Option<String>,
    /// Segments the item was drawn from. Must not be empty, and every id must
    /// exist: drop unknown citations before calling.
    pub source_segment_ids: Vec<String>,
}

/// Inserts a `pending` summary. Returns its id.
pub fn insert_summary(conn: &Connection, summary: &NewSummary) -> Result<String, String> {
    let id = new_id();
    execute(
        conn,
        "insert summary",
        "INSERT INTO summaries (id, meeting_id, run_id, provider, model, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            summary.meeting_id,
            summary.run_id,
            summary.provider,
            summary.model,
            SummaryStatus::Pending.as_str(),
            now(),
        ],
    )?;
    Ok(id)
}

/// Appends items after the ones the summary already has, with their sources,
/// in one transaction. Returns the item ids in input order.
pub fn add_summary_items(
    conn: &Connection,
    summary_id: &str,
    items: &[NewSummaryItem],
) -> Result<Vec<String>, String> {
    transaction(conn, || {
        let first_position: i64 = query_opt(
            conn,
            "read summary item position",
            "SELECT COALESCE(MAX(position) + 1, 0) FROM summary_items WHERE summary_id = ?1",
            params![summary_id],
            |row| row.get(0),
        )?
        .unwrap_or(0);

        let mut ids = Vec::with_capacity(items.len());
        for (offset, item) in items.iter().enumerate() {
            if item.source_segment_ids.is_empty() {
                return Err(format!("Summary item {offset} cites no transcript segment"));
            }
            let id = new_id();
            execute(
                conn,
                "insert summary item",
                "INSERT INTO summary_items
                   (id, summary_id, kind, position, text, owner, owner_participant_id, due_date)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id,
                    summary_id,
                    item.kind.as_str(),
                    first_position + offset as i64,
                    item.text,
                    item.owner,
                    item.owner_participant_id,
                    item.due_date,
                ],
            )?;
            for segment_id in &item.source_segment_ids {
                // OR IGNORE: citing the same segment twice is one source.
                execute(
                    conn,
                    "insert summary item source",
                    "INSERT OR IGNORE INTO summary_item_sources (item_id, segment_id) VALUES (?1, ?2)",
                    params![id, segment_id],
                )?;
            }
            ids.push(id);
        }
        Ok(ids)
    })
}

/// The content arrived: overview, items and sources are written and the
/// summary becomes `done`, all in one transaction.
pub fn complete_summary(
    conn: &Connection,
    summary_id: &str,
    overview: &str,
    items: &[NewSummaryItem],
) -> Result<(), String> {
    transaction(conn, || {
        let changed = execute(
            conn,
            "complete summary",
            "UPDATE summaries SET status = ?2, overview = ?3, error = NULL WHERE id = ?1",
            params![summary_id, SummaryStatus::Done.as_str(), overview],
        )?;
        expect_found(changed, "Summary", summary_id)?;
        add_summary_items(conn, summary_id, items)?;
        Ok(())
    })
}

pub fn fail_summary(conn: &Connection, summary_id: &str, error: &str) -> Result<(), String> {
    let changed = execute(
        conn,
        "fail summary",
        "UPDATE summaries SET status = ?2, error = ?3 WHERE id = ?1",
        params![summary_id, SummaryStatus::Failed.as_str(), error],
    )?;
    expect_found(changed, "Summary", summary_id)
}

const SUMMARY_SELECT: &str =
    "SELECT s.id, s.meeting_id, s.run_id, s.provider, s.model, s.status, s.overview, s.error,
            s.created_at
       FROM summaries s";

fn summary_from_row(row: &Row<'_>) -> rusqlite::Result<MeetingSummary> {
    Ok(MeetingSummary {
        id: row.get(0)?,
        meeting_id: row.get(1)?,
        run_id: row.get(2)?,
        provider: row.get(3)?,
        model: row.get(4)?,
        status: enum_col(row, 5, SummaryStatus::parse)?,
        overview: row.get(6)?,
        error: row.get(7)?,
        created_at: row.get(8)?,
        items: Vec::new(),
    })
}

fn load_items(conn: &Connection, summary_id: &str) -> Result<Vec<SummaryItem>, String> {
    let mut items = query_all(
        conn,
        "list summary items",
        "SELECT id, kind, text, owner, due_date FROM summary_items
          WHERE summary_id = ?1 ORDER BY position ASC",
        params![summary_id],
        |row| {
            Ok(SummaryItem {
                id: row.get(0)?,
                kind: enum_col(row, 1, SummaryItemKind::parse)?,
                text: row.get(2)?,
                owner: row.get(3)?,
                due_date: row.get(4)?,
                source_segment_ids: Vec::new(),
            })
        },
    )?;
    for item in &mut items {
        // Sources in transcript order, which is how a reader follows them.
        item.source_segment_ids = query_all(
            conn,
            "list summary item sources",
            "SELECT src.segment_id
               FROM summary_item_sources src
               JOIN transcript_segments seg ON seg.id = src.segment_id
              WHERE src.item_id = ?1
              ORDER BY seg.start_ms ASC, seg.rowid ASC",
            params![item.id],
            |row| row.get(0),
        )?;
    }
    Ok(items)
}

fn with_items(
    conn: &Connection,
    summary: Option<MeetingSummary>,
) -> Result<Option<MeetingSummary>, String> {
    let Some(mut summary) = summary else {
        return Ok(None);
    };
    summary.items = load_items(conn, &summary.id)?;
    Ok(Some(summary))
}

pub fn get_summary(conn: &Connection, summary_id: &str) -> Result<Option<MeetingSummary>, String> {
    let summary = query_opt(
        conn,
        "read summary",
        &format!("{SUMMARY_SELECT} WHERE s.id = ?1"),
        params![summary_id],
        summary_from_row,
    )?;
    with_items(conn, summary)
}

/// The summary to show for a meeting: the newest `done` one, so a failed
/// retry does not hide the summary the user already had. Without any `done`
/// one it is the newest row, whatever its status.
pub fn latest_summary(
    conn: &Connection,
    meeting_id: &str,
) -> Result<Option<MeetingSummary>, String> {
    let summary = query_opt(
        conn,
        "read latest summary",
        &format!(
            "{SUMMARY_SELECT} WHERE s.meeting_id = ?1
              ORDER BY (s.status = 'done') DESC, s.created_at DESC, s.rowid DESC LIMIT 1"
        ),
        params![meeting_id],
        summary_from_row,
    )?;
    with_items(conn, summary)
}

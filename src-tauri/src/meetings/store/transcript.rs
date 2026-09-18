//! Runs, windows, segments and the edit layer.
//!
//! A run is one transcription of a meeting with one model and language; a
//! re-run never touches the old rows. A window is the unit of resume: its row
//! and its segments are written in one transaction, so it is either `done`
//! with its segments or still `pending` with none.

use rusqlite::{params, Connection, Row};

use super::super::types::{
    MeetingLanguage, RunStatus, Segment, SuppressedReason, TrackKind, TranscriptRun, WindowStatus,
};
use super::{
    enum_col, execute, expect_found, new_id, now, opt_enum_col, query_all, query_opt, transaction,
    u32_col, u64_col, ASSIGNED_NAME_SQL, ASSIGNMENT_JOINS,
};

// --- Runs --------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NewRun {
    pub meeting_id: String,
    pub model: String,
    pub language: MeetingLanguage,
    /// Decode parameters as JSON, so the run can be reproduced. `None` is `{}`.
    pub params_json: Option<String>,
}

/// Inserts a `queued` run. Returns its id.
pub fn insert_run(conn: &Connection, run: &NewRun) -> Result<String, String> {
    let id = new_id();
    execute(
        conn,
        "insert run",
        "INSERT INTO transcript_runs (id, meeting_id, model, language, params_json, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            run.meeting_id,
            run.model,
            run.language.as_str(),
            run.params_json.as_deref().unwrap_or("{}"),
            RunStatus::Queued.as_str(),
            now(),
        ],
    )?;
    Ok(id)
}

/// Sets the status and replaces the error. `running` stamps `started_at` the
/// first time; a final status stamps `finished_at`.
pub fn set_run_status(
    conn: &Connection,
    run_id: &str,
    status: RunStatus,
    error: Option<&str>,
) -> Result<(), String> {
    let changed = execute(
        conn,
        "set run status",
        "UPDATE transcript_runs
            SET status = ?2,
                error = ?3,
                started_at = CASE WHEN ?2 = 'running' AND started_at IS NULL
                                  THEN ?4 ELSE started_at END,
                finished_at = CASE WHEN ?2 IN ('done', 'failed', 'cancelled') THEN ?4 END
          WHERE id = ?1",
        params![run_id, status.as_str(), error, now()],
    )?;
    expect_found(changed, "Run", run_id)
}

const RUN_SELECT: &str =
    "SELECT r.id, r.model, r.language, r.status, r.error, r.created_at, r.finished_at
       FROM transcript_runs r";

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<TranscriptRun> {
    Ok(TranscriptRun {
        id: row.get(0)?,
        model: row.get(1)?,
        language: enum_col(row, 2, MeetingLanguage::parse)?,
        status: enum_col(row, 3, RunStatus::parse)?,
        error: row.get(4)?,
        created_at: row.get(5)?,
        finished_at: row.get(6)?,
    })
}

pub fn get_run(conn: &Connection, run_id: &str) -> Result<Option<TranscriptRun>, String> {
    query_opt(
        conn,
        "read run",
        &format!("{RUN_SELECT} WHERE r.id = ?1"),
        params![run_id],
        run_from_row,
    )
}

/// The run's decode parameters, as stored by `insert_run`.
#[cfg(test)] // only the tests read the recorded parameters back
pub fn get_run_params(conn: &Connection, run_id: &str) -> Result<Option<String>, String> {
    query_opt(
        conn,
        "read run params",
        "SELECT params_json FROM transcript_runs WHERE id = ?1",
        params![run_id],
        |row| row.get(0),
    )
}

/// Oldest first.
pub fn list_runs(conn: &Connection, meeting_id: &str) -> Result<Vec<TranscriptRun>, String> {
    query_all(
        conn,
        "list runs",
        &format!("{RUN_SELECT} WHERE r.meeting_id = ?1 ORDER BY r.created_at ASC, r.rowid ASC"),
        params![meeting_id],
        run_from_row,
    )
}

// --- Windows -------------------------------------------------------------------

/// A planned window, as `longform.rs` produces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWindow {
    pub track_id: String,
    /// Position within the track, from 0.
    pub seq: u32,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRow {
    pub id: String,
    pub run_id: String,
    pub track_id: String,
    pub track_kind: TrackKind,
    pub seq: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub status: WindowStatus,
    /// The language it was decoded in. Set when the window is done.
    pub language: Option<String>,
    pub attempts: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowCounts {
    pub done: u32,
    pub failed: u32,
    pub total: u32,
}

impl WindowCounts {
    /// Nothing left to decode. A run without windows (no speech) is settled.
    #[cfg(test)] // the worker reads the counts itself
    pub fn is_settled(&self) -> bool {
        self.done + self.failed >= self.total
    }
}

/// Inserts a run's plan, all `pending`, in one transaction: a run has either
/// its whole plan or none. Returns the window ids in input order.
pub fn insert_windows(
    conn: &Connection,
    run_id: &str,
    windows: &[NewWindow],
) -> Result<Vec<String>, String> {
    transaction(conn, || {
        let mut stmt = conn
            .prepare(
                "INSERT INTO transcript_windows (id, run_id, track_id, seq, start_ms, end_ms, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .map_err(|e| format!("Failed to prepare insert windows: {e}"))?;
        let mut ids = Vec::with_capacity(windows.len());
        for window in windows {
            if window.end_ms < window.start_ms {
                return Err(format!(
                    "Window {} ends before it starts ({} < {})",
                    window.seq, window.end_ms, window.start_ms
                ));
            }
            let id = new_id();
            stmt.execute(params![
                id,
                run_id,
                window.track_id,
                window.seq as i64,
                window.start_ms as i64,
                window.end_ms as i64,
                WindowStatus::Pending.as_str(),
            ])
            .map_err(|e| format!("Failed to insert window {}: {e}", window.seq))?;
            ids.push(id);
        }
        Ok(ids)
    })
}

const WINDOW_SELECT: &str =
    "SELECT w.id, w.run_id, w.track_id, t.kind, w.seq, w.start_ms, w.end_ms, w.status,
            w.language, w.attempts, w.error
       FROM transcript_windows w
       JOIN meeting_tracks t ON t.id = w.track_id";

/// Timeline order, mic before system at the same position: the transcript
/// fills in from the start of the meeting.
const WINDOW_ORDER: &str = "ORDER BY w.start_ms ASC, t.kind ASC, w.seq ASC";

fn window_from_row(row: &Row<'_>) -> rusqlite::Result<WindowRow> {
    Ok(WindowRow {
        id: row.get(0)?,
        run_id: row.get(1)?,
        track_id: row.get(2)?,
        track_kind: enum_col(row, 3, TrackKind::parse)?,
        seq: u32_col(row, 4)?,
        start_ms: u64_col(row, 5)?,
        end_ms: u64_col(row, 6)?,
        status: enum_col(row, 7, WindowStatus::parse)?,
        language: row.get(8)?,
        attempts: u32_col(row, 9)?,
        error: row.get(10)?,
    })
}

pub fn list_windows(conn: &Connection, run_id: &str) -> Result<Vec<WindowRow>, String> {
    query_all(
        conn,
        "list windows",
        &format!("{WINDOW_SELECT} WHERE w.run_id = ?1 {WINDOW_ORDER}"),
        params![run_id],
        window_from_row,
    )
}

#[cfg(test)] // the worker takes them one at a time (`next_pending_window`)
pub fn list_pending_windows(conn: &Connection, run_id: &str) -> Result<Vec<WindowRow>, String> {
    query_all(
        conn,
        "list pending windows",
        &format!("{WINDOW_SELECT} WHERE w.run_id = ?1 AND w.status = 'pending' {WINDOW_ORDER}"),
        params![run_id],
        window_from_row,
    )
}

/// The next window to decode, `None` when the run has none left.
pub fn next_pending_window(conn: &Connection, run_id: &str) -> Result<Option<WindowRow>, String> {
    query_opt(
        conn,
        "read next pending window",
        &format!(
            "{WINDOW_SELECT} WHERE w.run_id = ?1 AND w.status = 'pending' {WINDOW_ORDER} LIMIT 1"
        ),
        params![run_id],
        window_from_row,
    )
}

pub fn window_counts(conn: &Connection, run_id: &str) -> Result<WindowCounts, String> {
    let counts = query_opt(
        conn,
        "count windows",
        "SELECT COALESCE(SUM(status = 'done'), 0), COALESCE(SUM(status = 'failed'), 0), COUNT(*)
           FROM transcript_windows WHERE run_id = ?1",
        params![run_id],
        |row| {
            Ok(WindowCounts {
                done: u32_col(row, 0)?,
                failed: u32_col(row, 1)?,
                total: u32_col(row, 2)?,
            })
        },
    )?;
    Ok(counts.unwrap_or_default())
}

/// The language a track was decoded in during this run, if any window of it
/// is done. It is what makes `auto` detection resumable: detect once, then
/// every later window of the track reads it back from here.
pub fn track_language(
    conn: &Connection,
    run_id: &str,
    track_id: &str,
) -> Result<Option<String>, String> {
    query_opt(
        conn,
        "read track language",
        "SELECT language FROM transcript_windows
          WHERE run_id = ?1 AND track_id = ?2 AND language IS NOT NULL
          ORDER BY seq ASC LIMIT 1",
        params![run_id, track_id],
        |row| row.get(0),
    )
}

/// A decode attempt failed. The window stays `pending` for another try until
/// it has used `max_attempts`, then it becomes `failed`. Returns the status it
/// ended up in. A preempted decode is not a failure: leave the window alone.
pub fn fail_window(
    conn: &Connection,
    window_id: &str,
    error: &str,
    max_attempts: u32,
) -> Result<WindowStatus, String> {
    let status = query_opt(
        conn,
        "fail window",
        "UPDATE transcript_windows
            SET attempts = attempts + 1,
                error = ?2,
                status = CASE WHEN attempts + 1 >= ?3 THEN 'failed' ELSE 'pending' END
          WHERE id = ?1 AND status = 'pending'
          RETURNING status",
        params![window_id, error, max_attempts as i64],
        |row| enum_col(row, 0, WindowStatus::parse),
    )?;
    status.ok_or_else(|| format!("Window '{window_id}' is not pending"))
}

// --- Segments ------------------------------------------------------------------

/// One decoded segment, already mapped to the meeting timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub lang: Option<String>,
    pub no_speech_prob: Option<f32>,
    pub avg_logprob: Option<f32>,
    /// Suspect segments are flagged, never dropped.
    pub suppressed_reason: Option<SuppressedReason>,
}

/// Marks the window `done` and inserts its segments in one transaction, the
/// unit of resume: a failure anywhere leaves the window `pending` with no
/// segments, so nothing is decoded twice and nothing is half-written.
/// `language` is the language the window was decoded in. Returns the segment
/// ids in input order. A window that is not `pending` is refused.
pub fn complete_window(
    conn: &Connection,
    window_id: &str,
    language: Option<&str>,
    segments: &[NewSegment],
) -> Result<Vec<String>, String> {
    transaction(conn, || {
        let owner = query_opt(
            conn,
            "complete window",
            "UPDATE transcript_windows
                SET status = 'done', language = COALESCE(?2, language),
                    attempts = attempts + 1, error = NULL, decoded_at = ?3
              WHERE id = ?1 AND status = 'pending'
              RETURNING run_id, track_id",
            params![window_id, language, now()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let Some((run_id, track_id)) = owner else {
            return Err(format!("Window '{window_id}' is not pending"));
        };

        let mut stmt = conn
            .prepare(
                "INSERT INTO transcript_segments
                   (id, run_id, window_id, track_id, seq, start_ms, end_ms, text, lang,
                    no_speech_prob, avg_logprob, suppressed_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )
            .map_err(|e| format!("Failed to prepare insert segments: {e}"))?;
        let mut ids = Vec::with_capacity(segments.len());
        for (seq, segment) in segments.iter().enumerate() {
            let id = new_id();
            stmt.execute(params![
                id,
                run_id,
                window_id,
                track_id,
                seq as i64,
                segment.start_ms as i64,
                segment.end_ms as i64,
                segment.text,
                segment.lang,
                segment.no_speech_prob.map(f64::from),
                segment.avg_logprob.map(f64::from),
                segment.suppressed_reason.map(SuppressedReason::as_str),
            ])
            .map_err(|e| format!("Failed to insert segment {seq} of window '{window_id}': {e}"))?;
            ids.push(id);
        }
        Ok(ids)
    })
}

fn segment_select() -> String {
    format!(
        "SELECT seg.id, run.meeting_id, seg.run_id, seg.track_id, trk.kind,
                seg.start_ms, seg.end_ms,
                COALESCE(ed.text, seg.text),
                CASE WHEN ed.text IS NOT NULL THEN seg.text END,
                seg.lang,
                spk.id,
                COALESCE({ASSIGNED_NAME_SQL}, spk.label,
                         CASE trk.kind WHEN 'mic' THEN 'Me' ELSE 'Them' END),
                seg.suppressed_reason,
                COALESCE(ed.hidden, seg.suppressed_reason IS NOT NULL)
           FROM transcript_segments seg
           JOIN transcript_runs run ON run.id = seg.run_id
           JOIN meeting_tracks trk ON trk.id = seg.track_id
           LEFT JOIN segment_edits ed ON ed.segment_id = seg.id
           LEFT JOIN speakers spk ON spk.id = COALESCE(
                  (SELECT ss.speaker_id FROM segment_speakers ss
                    WHERE ss.segment_id = seg.id
                    ORDER BY ss.confidence IS NULL DESC, ss.confidence DESC LIMIT 1),
                  (SELECT ts.id FROM speakers ts
                    WHERE ts.track_id = seg.track_id AND ts.source = 'track'
                    ORDER BY ts.created_at ASC LIMIT 1))
           {ASSIGNMENT_JOINS}"
    )
}

fn segment_from_row(row: &Row<'_>) -> rusqlite::Result<Segment> {
    Ok(Segment {
        id: row.get(0)?,
        meeting_id: row.get(1)?,
        run_id: row.get(2)?,
        track_id: row.get(3)?,
        track_kind: enum_col(row, 4, TrackKind::parse)?,
        start_ms: u64_col(row, 5)?,
        end_ms: u64_col(row, 6)?,
        text: row.get(7)?,
        original_text: row.get(8)?,
        lang: row.get(9)?,
        speaker_id: row.get(10)?,
        speaker_label: row.get(11)?,
        suppressed_reason: opt_enum_col(row, 12, SuppressedReason::parse)?,
        hidden: row.get(13)?,
    })
}

/// All segments of a run in timeline order, both tracks interleaved, hidden
/// ones included. `run_id: None` means the meeting's active run; a meeting
/// without one has no segments.
pub fn list_segments(
    conn: &Connection,
    meeting_id: &str,
    run_id: Option<&str>,
) -> Result<Vec<Segment>, String> {
    query_all(
        conn,
        "list segments",
        &format!(
            "{} WHERE run.meeting_id = ?1
                  AND seg.run_id = COALESCE(?2, (SELECT active_run_id FROM meetings WHERE id = ?1))
                ORDER BY seg.start_ms ASC, seg.end_ms ASC, trk.kind ASC, seg.rowid ASC",
            segment_select()
        ),
        params![meeting_id, run_id],
        segment_from_row,
    )
}

/// The segment as it displays, `None` when there is no such segment.
pub fn get_segment(conn: &Connection, segment_id: &str) -> Result<Option<Segment>, String> {
    query_opt(
        conn,
        "read segment",
        &format!("{} WHERE seg.id = ?1", segment_select()),
        params![segment_id],
        segment_from_row,
    )
}

fn require_segment(conn: &Connection, segment_id: &str) -> Result<Segment, String> {
    get_segment(conn, segment_id)?.ok_or_else(|| format!("Segment '{segment_id}' not found"))
}

/// Segments of the run the pipeline did not flag, hidden by the user or not.
/// `run_id: None` means the active run.
#[cfg(test)] // only the tests count
pub fn count_segments(
    conn: &Connection,
    meeting_id: &str,
    run_id: Option<&str>,
) -> Result<u32, String> {
    let count = query_opt(
        conn,
        "count segments",
        "SELECT COUNT(*)
           FROM transcript_segments seg
           JOIN transcript_runs run ON run.id = seg.run_id
          WHERE run.meeting_id = ?1
            AND seg.run_id = COALESCE(?2, (SELECT active_run_id FROM meetings WHERE id = ?1))
            AND seg.suppressed_reason IS NULL",
        params![meeting_id, run_id],
        |row| u32_col(row, 0),
    )?;
    Ok(count.unwrap_or(0))
}

/// Flags many segments at once (echo detection's result), in one transaction.
/// A segment that already carries a reason keeps it: the first finding wins.
/// Returns how many were newly flagged.
pub fn flag_segments(
    conn: &Connection,
    segment_ids: &[String],
    reason: SuppressedReason,
) -> Result<usize, String> {
    transaction(conn, || {
        let mut stmt = conn
            .prepare(
                "UPDATE transcript_segments SET suppressed_reason = ?2
                  WHERE id = ?1 AND suppressed_reason IS NULL",
            )
            .map_err(|e| format!("Failed to prepare flag segments: {e}"))?;
        let mut flagged = 0;
        for id in segment_ids {
            flagged += stmt
                .execute(params![id, reason.as_str()])
                .map_err(|e| format!("Failed to flag segment: {e}"))?;
        }
        Ok(flagged)
    })
}

// --- Segment edits ---------------------------------------------------------------

/// A row that overrides nothing is removed, so "has an edit" stays meaningful.
fn drop_empty_edit(conn: &Connection, segment_id: &str) -> Result<(), String> {
    execute(
        conn,
        "clean up segment edit",
        "DELETE FROM segment_edits WHERE segment_id = ?1 AND text IS NULL AND hidden IS NULL",
        params![segment_id],
    )?;
    Ok(())
}

/// Upserts `segment_edits.text`; the decoded text is never touched. `None`
/// clears the edit, and so does text equal to the decoded text. Returns the
/// segment as it now displays.
pub fn set_segment_text(
    conn: &Connection,
    segment_id: &str,
    text: Option<&str>,
) -> Result<Segment, String> {
    transaction(conn, || {
        let raw_text: Option<String> = query_opt(
            conn,
            "read segment",
            "SELECT text FROM transcript_segments WHERE id = ?1",
            params![segment_id],
            |row| row.get(0),
        )?;
        let Some(raw_text) = raw_text else {
            return Err(format!("Segment '{segment_id}' not found"));
        };
        let text = text.filter(|t| *t != raw_text);
        execute(
            conn,
            "edit segment text",
            "INSERT INTO segment_edits (segment_id, text, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(segment_id) DO UPDATE
               SET text = excluded.text, updated_at = excluded.updated_at",
            params![segment_id, text, now()],
        )?;
        drop_empty_edit(conn, segment_id)?;
        require_segment(conn, segment_id)
    })
}

/// Upserts `segment_edits.hidden`: the user's own choice, which overrides the
/// pipeline's flag both ways. Returns the segment as it now displays.
pub fn set_segment_hidden(
    conn: &Connection,
    segment_id: &str,
    hidden: bool,
) -> Result<Segment, String> {
    transaction(conn, || {
        require_segment(conn, segment_id)?;
        execute(
            conn,
            "set segment hidden",
            "INSERT INTO segment_edits (segment_id, hidden, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(segment_id) DO UPDATE
               SET hidden = excluded.hidden, updated_at = excluded.updated_at",
            params![segment_id, hidden, now()],
        )?;
        require_segment(conn, segment_id)
    })
}

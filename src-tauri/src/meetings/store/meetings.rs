//! `meetings`: one row per recorded meeting.

use rusqlite::{params, Connection, ToSql};

use super::super::types::{MeetingDetail, MeetingLanguage, MeetingListItem, MeetingStatus};
use super::{
    audio, execute, expect_found, jobs, new_id, now, query_all, query_opt, speakers, transaction,
    transcript, enum_col, u64_col,
};

#[derive(Debug, Clone)]
pub struct NewMeeting {
    pub title: String,
    pub language: MeetingLanguage,
    /// The model the first run will use; shown until a run is active.
    pub model: Option<String>,
    /// Mach host time of timeline position 0, once known.
    pub origin_host_ns: Option<u64>,
    pub calendar_event_id: Option<String>,
}

/// Inserts a meeting that starts now, in status `recording`. Returns its id.
pub fn insert_meeting(conn: &Connection, meeting: &NewMeeting) -> Result<String, String> {
    let title = meeting.title.trim();
    if title.is_empty() {
        return Err("A meeting needs a title".to_string());
    }
    let id = new_id();
    let now = now();
    execute(
        conn,
        "insert meeting",
        "INSERT INTO meetings
           (id, title, status, started_at, language, model, origin_host_ns,
            calendar_event_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?4, ?4)",
        params![
            id,
            title,
            MeetingStatus::Recording.as_str(),
            now,
            meeting.language.as_str(),
            meeting.model,
            meeting.origin_host_ns.map(|v| v as i64),
            meeting.calendar_event_id,
        ],
    )?;
    Ok(id)
}

const LIST_ITEM_SELECT: &str = "SELECT m.id, m.title, m.status, m.started_at, m.ended_at,
       m.duration_ms, m.language, m.echo_risk,
       (m.audio_deleted_at IS NULL AND EXISTS (
          SELECT 1 FROM meeting_audio_chunks c
          JOIN meeting_tracks t ON t.id = c.track_id
          WHERE t.meeting_id = m.id AND c.status != 'deleted')),
       EXISTS (SELECT 1 FROM summaries s WHERE s.meeting_id = m.id AND s.status = 'done')
     FROM meetings m";

/// `filter` is the SQL after the select: a WHERE clause and the ordering.
fn list_items(
    conn: &Connection,
    filter: &str,
    params: &[&dyn ToSql],
) -> Result<Vec<MeetingListItem>, String> {
    let mut items = query_all(
        conn,
        "list meetings",
        &format!("{LIST_ITEM_SELECT} {filter}"),
        params,
        |row| {
            Ok(MeetingListItem {
                id: row.get(0)?,
                title: row.get(1)?,
                status: enum_col(row, 2, MeetingStatus::parse)?,
                started_at: row.get(3)?,
                ended_at: row.get(4)?,
                duration_ms: u64_col(row, 5)?,
                language: enum_col(row, 6, MeetingLanguage::parse)?,
                echo_risk: row.get(7)?,
                has_audio: row.get(8)?,
                has_summary: row.get(9)?,
                job: None,
            })
        },
    )?;
    let mut unfinished = jobs::unfinished_jobs(conn)?;
    for item in &mut items {
        item.job = unfinished.remove(&item.id);
    }
    Ok(items)
}

/// Newest first. Each item carries its unfinished job, if any.
pub fn list_meetings(conn: &Connection) -> Result<Vec<MeetingListItem>, String> {
    list_items(conn, "ORDER BY m.started_at DESC, m.rowid DESC", &[])
}

/// Meetings in any of `statuses`, oldest first: launch recovery walks the
/// ones left in `recording` or `paused`.
pub fn list_meetings_with_status(
    conn: &Connection,
    statuses: &[MeetingStatus],
) -> Result<Vec<MeetingListItem>, String> {
    if statuses.is_empty() {
        return Ok(Vec::new());
    }
    // The values are the enum's own static strings, never caller text.
    let list = statuses
        .iter()
        .map(|s| format!("'{}'", s.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    list_items(
        conn,
        &format!("WHERE m.status IN ({list}) ORDER BY m.started_at ASC, m.rowid ASC"),
        &[],
    )
}

/// `None` when there is no such meeting.
pub fn get_meeting(conn: &Connection, meeting_id: &str) -> Result<Option<MeetingDetail>, String> {
    let Some(meeting) = list_items(conn, "WHERE m.id = ?1", &[&meeting_id])?.pop() else {
        return Ok(None);
    };
    let extra = query_opt(
        conn,
        "read meeting",
        "SELECT COALESCE(r.model, m.model), m.active_run_id, m.error,
                (SELECT COALESCE(SUM(c.n_frames), 0) * 2
                   FROM meeting_audio_chunks c
                   JOIN meeting_tracks t ON t.id = c.track_id
                  WHERE t.meeting_id = m.id AND c.status != 'deleted')
           FROM meetings m
           LEFT JOIN transcript_runs r ON r.id = m.active_run_id
          WHERE m.id = ?1",
        params![meeting_id],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                u64_col(row, 3)?,
            ))
        },
    )?;
    let Some((model, active_run_id, error, audio_bytes)) = extra else {
        return Ok(None);
    };
    Ok(Some(MeetingDetail {
        meeting,
        model,
        active_run_id,
        error,
        audio_bytes,
        tracks: audio::list_tracks(conn, meeting_id)?,
        runs: transcript::list_runs(conn, meeting_id)?,
        speakers: speakers::list_speakers(conn, meeting_id)?,
    }))
}

/// Trims the title; rejects an empty one and an unknown meeting.
pub fn rename_meeting(conn: &Connection, meeting_id: &str, title: &str) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("A meeting title cannot be empty".to_string());
    }
    let changed = execute(
        conn,
        "rename meeting",
        "UPDATE meetings SET title = ?2, updated_at = ?3 WHERE id = ?1",
        params![meeting_id, title, now()],
    )?;
    expect_found(changed, "Meeting", meeting_id)
}

/// Sets the status and replaces `meetings.error`: pass the reason with
/// `failed`, `None` otherwise (which clears an earlier failure).
pub fn set_meeting_status(
    conn: &Connection,
    meeting_id: &str,
    status: MeetingStatus,
    error: Option<&str>,
) -> Result<(), String> {
    let changed = execute(
        conn,
        "set meeting status",
        "UPDATE meetings SET status = ?2, error = ?3, updated_at = ?4 WHERE id = ?1",
        params![meeting_id, status.as_str(), error, now()],
    )?;
    expect_found(changed, "Meeting", meeting_id)
}

/// Records the end of the recording. `duration_ms` is recorded time, pauses
/// excluded. The status is the caller's next move.
pub fn finish_meeting(
    conn: &Connection,
    meeting_id: &str,
    ended_at: &str,
    duration_ms: u64,
) -> Result<(), String> {
    let changed = execute(
        conn,
        "finish meeting",
        "UPDATE meetings SET ended_at = ?2, duration_ms = ?3, updated_at = ?4 WHERE id = ?1",
        params![meeting_id, ended_at, duration_ms as i64, now()],
    )?;
    expect_found(changed, "Meeting", meeting_id)
}

/// Makes `run_id` the run whose segments the meeting shows. The column holds
/// one id, so there is exactly one active run; a run of another meeting is
/// refused.
pub fn set_active_run(conn: &Connection, meeting_id: &str, run_id: &str) -> Result<(), String> {
    let changed = execute(
        conn,
        "set active run",
        "UPDATE meetings SET active_run_id = ?2, updated_at = ?3
          WHERE id = ?1
            AND EXISTS (SELECT 1 FROM transcript_runs r WHERE r.id = ?2 AND r.meeting_id = ?1)",
        params![meeting_id, run_id, now()],
    )?;
    if changed == 0 {
        return Err(format!("Run '{run_id}' does not belong to meeting '{meeting_id}'"));
    }
    Ok(())
}

pub fn set_echo_risk(conn: &Connection, meeting_id: &str, echo_risk: bool) -> Result<(), String> {
    let changed = execute(
        conn,
        "set echo risk",
        "UPDATE meetings SET echo_risk = ?2, updated_at = ?3 WHERE id = ?1",
        params![meeting_id, echo_risk, now()],
    )?;
    expect_found(changed, "Meeting", meeting_id)
}

/// Mach host time of timeline position 0, for a session that only learns it
/// with the first audio callback.
pub fn set_origin_host_ns(
    conn: &Connection,
    meeting_id: &str,
    origin_host_ns: u64,
) -> Result<(), String> {
    let changed = execute(
        conn,
        "set meeting origin",
        "UPDATE meetings SET origin_host_ns = ?2, updated_at = ?3 WHERE id = ?1",
        params![meeting_id, origin_host_ns as i64, now()],
    )?;
    expect_found(changed, "Meeting", meeting_id)
}

/// The audio is gone, the transcript stays: every chunk becomes `deleted`
/// (the rows keep the timeline) and `audio_deleted_at` is set, in one
/// transaction. Deleting the files is the caller's job.
pub fn mark_audio_deleted(conn: &Connection, meeting_id: &str) -> Result<(), String> {
    transaction(conn, || {
        let now = now();
        let changed = execute(
            conn,
            "mark audio deleted",
            "UPDATE meetings SET audio_deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![meeting_id, now],
        )?;
        expect_found(changed, "Meeting", meeting_id)?;
        audio::mark_chunks_deleted(conn, meeting_id)?;
        Ok(())
    })
}

/// Deletes the meeting and, through ON DELETE CASCADE, every row that hangs
/// off it. `false` when there was no such meeting. People are shared between
/// meetings and stay.
pub fn delete_meeting(conn: &Connection, meeting_id: &str) -> Result<bool, String> {
    let changed = execute(
        conn,
        "delete meeting",
        "DELETE FROM meetings WHERE id = ?1",
        params![meeting_id],
    )?;
    Ok(changed > 0)
}

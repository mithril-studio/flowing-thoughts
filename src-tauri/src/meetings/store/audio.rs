//! `meeting_tracks` and `meeting_audio_chunks`: what was recorded, and where
//! it sits on disk and on the timeline.
//!
//! The chunk functions are shaped so a `types::ChunkLedger` is a few lines on
//! top: `chunk_opened` is `insert_chunk`, `chunk_closed` is `close_chunk`.

use rusqlite::{params, Connection, Row};

use super::super::types::{ChunkRecord, ChunkStatus, MeetingTrack, SourceFormat, TrackKind};
use super::{enum_col, execute, expect_found, new_id, now, query_all, query_opt, u32_col, u64_col};

// --- Tracks ------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NewTrack {
    pub meeting_id: String,
    pub kind: TrackKind,
    pub device_name: Option<String>,
    /// The source's native format, before resampling to 16 kHz mono.
    pub format: Option<SourceFormat>,
}

/// One track per kind per meeting. Returns the track id.
pub fn insert_track(conn: &Connection, track: &NewTrack) -> Result<String, String> {
    let id = new_id();
    execute(
        conn,
        "insert track",
        "INSERT INTO meeting_tracks
           (id, meeting_id, kind, device_name, source_sample_rate, source_channels, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            track.meeting_id,
            track.kind.as_str(),
            track.device_name,
            track.format.map(|f| f.sample_rate as i64),
            track.format.map(|f| f.channels as i64),
            now(),
        ],
    )?;
    Ok(id)
}

/// The device behind a track changed mid-meeting (AirPods, output switch).
/// The row keeps the latest device.
pub fn set_track_device(
    conn: &Connection,
    track_id: &str,
    device_name: Option<&str>,
    format: Option<SourceFormat>,
) -> Result<(), String> {
    let changed = execute(
        conn,
        "set track device",
        "UPDATE meeting_tracks
            SET device_name = ?2, source_sample_rate = ?3, source_channels = ?4
          WHERE id = ?1",
        params![
            track_id,
            device_name,
            format.map(|f| f.sample_rate as i64),
            format.map(|f| f.channels as i64),
        ],
    )?;
    expect_found(changed, "Track", track_id)
}

/// Adds frames lost to ring-buffer overflow. Additive, so the recorder can
/// report each overflow as it happens.
pub fn add_track_overflow(conn: &Connection, track_id: &str, frames: u64) -> Result<(), String> {
    let changed = execute(
        conn,
        "add track overflow",
        "UPDATE meeting_tracks SET overflow_frames = overflow_frames + ?2 WHERE id = ?1",
        params![track_id, frames as i64],
    )?;
    expect_found(changed, "Track", track_id)
}

/// Mic first, then system. `duration_ms` is the end of the track's last chunk
/// on the meeting timeline.
pub fn list_tracks(conn: &Connection, meeting_id: &str) -> Result<Vec<MeetingTrack>, String> {
    query_all(
        conn,
        "list tracks",
        "SELECT t.id, t.kind, t.device_name, t.overflow_frames,
                COALESCE((SELECT MAX(c.start_ms + c.n_frames * 1000 / c.sample_rate)
                            FROM meeting_audio_chunks c WHERE c.track_id = t.id), 0),
                EXISTS (SELECT 1 FROM meeting_audio_chunks c
                         WHERE c.track_id = t.id AND c.status != 'deleted')
           FROM meeting_tracks t
          WHERE t.meeting_id = ?1
          ORDER BY t.kind ASC",
        params![meeting_id],
        |row| {
            Ok(MeetingTrack {
                id: row.get(0)?,
                kind: enum_col(row, 1, TrackKind::parse)?,
                device_name: row.get(2)?,
                overflow_frames: u64_col(row, 3)?,
                duration_ms: u64_col(row, 4)?,
                has_audio: row.get(5)?,
            })
        },
    )
}

// --- Audio chunks --------------------------------------------------------------

const CHUNK_SELECT: &str =
    "SELECT c.id, c.track_id, c.seq, c.path, c.status, c.anchor_host_ns, c.start_ms, c.n_frames
       FROM meeting_audio_chunks c";

fn chunk_from_row(row: &Row<'_>) -> rusqlite::Result<ChunkRecord> {
    Ok(ChunkRecord {
        id: row.get(0)?,
        track_id: row.get(1)?,
        seq: u32_col(row, 2)?,
        path: row.get(3)?,
        status: enum_col(row, 4, ChunkStatus::parse)?,
        anchor_host_ns: u64_col(row, 5)?,
        start_ms: u64_col(row, 6)?,
        n_frames: u64_col(row, 7)?,
    })
}

/// Inserts the row as the writer describes it: `open` when the file has just
/// been created. Call it before the first sample is written, so a crash
/// always leaves a row for recovery to find.
pub fn insert_chunk(conn: &Connection, chunk: &ChunkRecord) -> Result<(), String> {
    let now = now();
    let closed_at = (chunk.status != ChunkStatus::Open).then_some(now.as_str());
    execute(
        conn,
        "insert chunk",
        "INSERT INTO meeting_audio_chunks
           (id, track_id, seq, path, status, anchor_host_ns, start_ms, n_frames, opened_at, closed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            chunk.id,
            chunk.track_id,
            chunk.seq as i64,
            chunk.path,
            chunk.status.as_str(),
            chunk.anchor_host_ns as i64,
            chunk.start_ms as i64,
            chunk.n_frames as i64,
            now,
            closed_at,
        ],
    )?;
    Ok(())
}

fn settle_chunk(
    conn: &Connection,
    chunk_id: &str,
    status: ChunkStatus,
    n_frames: u64,
) -> Result<(), String> {
    let changed = execute(
        conn,
        "close chunk",
        "UPDATE meeting_audio_chunks SET status = ?2, n_frames = ?3, closed_at = ?4
          WHERE id = ?1 AND status = 'open'",
        params![chunk_id, status.as_str(), n_frames as i64, now()],
    )?;
    if changed == 0 {
        return Err(format!("Chunk '{chunk_id}' is not open"));
    }
    Ok(())
}

/// The file was fsynced and closed with `n_frames` frames in it. Only an
/// `open` chunk can be closed.
pub fn close_chunk(conn: &Connection, chunk_id: &str, n_frames: u64) -> Result<(), String> {
    settle_chunk(conn, chunk_id, ChunkStatus::Closed, n_frames)
}

/// Launch recovery found the chunk still `open`: `n_frames` is what is on
/// disk (file length / 2).
pub fn mark_chunk_recovered(conn: &Connection, chunk_id: &str, n_frames: u64) -> Result<(), String> {
    settle_chunk(conn, chunk_id, ChunkStatus::Recovered, n_frames)
}

/// Every chunk of the meeting becomes `deleted`; the rows stay for the
/// timeline. Returns how many changed. `store::mark_audio_deleted` is the
/// whole operation.
pub fn mark_chunks_deleted(conn: &Connection, meeting_id: &str) -> Result<usize, String> {
    execute(
        conn,
        "mark chunks deleted",
        "UPDATE meeting_audio_chunks SET status = 'deleted', closed_at = COALESCE(closed_at, ?2)
          WHERE status != 'deleted'
            AND track_id IN (SELECT id FROM meeting_tracks WHERE meeting_id = ?1)",
        params![meeting_id, now()],
    )
}

/// A track's chunks in recording order.
pub fn list_chunks(conn: &Connection, track_id: &str) -> Result<Vec<ChunkRecord>, String> {
    query_all(
        conn,
        "list chunks",
        &format!("{CHUNK_SELECT} WHERE c.track_id = ?1 ORDER BY c.seq ASC"),
        params![track_id],
        chunk_from_row,
    )
}

/// Every `open` chunk of every meeting, for crash recovery at launch.
pub fn list_open_chunks(conn: &Connection) -> Result<Vec<ChunkRecord>, String> {
    query_all(
        conn,
        "list open chunks",
        &format!("{CHUNK_SELECT} WHERE c.status = 'open' ORDER BY c.track_id ASC, c.seq ASC"),
        [],
        chunk_from_row,
    )
}

/// The meeting a track belongs to, for recovery walking from chunk to meeting.
pub fn track_meeting_id(conn: &Connection, track_id: &str) -> Result<Option<String>, String> {
    query_opt(
        conn,
        "read track",
        "SELECT meeting_id FROM meeting_tracks WHERE id = ?1",
        params![track_id],
        |row| row.get(0),
    )
}

//! `speakers`, `speaker_turns` and `segment_speakers`.
//!
//! v1 only seeds the two track speakers. A segment without a
//! `segment_speakers` row belongs to the track speaker of its track, so
//! nothing has to be written per segment until diarization splits "Them"
//! into people. The rest of this file is the CRUD that stage needs.

// v1 only seeds and lists the track speakers. The rest is for diarization and
// per-person labels, which the plan schedules right after v1; the segment
// queries already honour `segment_speakers`. Kept, and covered by
// `store/tests.rs`, rather than rebuilt then.
#![allow(dead_code)]

use rusqlite::{params, Connection, Row};

use super::super::types::{Segment, Speaker, SpeakerSource, TrackKind};
use super::{
    enum_col, execute, expect_found, new_id, now, query_all, query_opt, transaction, transcript,
    u64_col, ASSIGNED_NAME_SQL, ASSIGNMENT_JOINS,
};

pub const SELF_LABEL: &str = "Me";
pub const REMOTE_LABEL: &str = "Them";

/// The label of a track's seeded speaker.
pub fn track_speaker_label(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Mic => SELF_LABEL,
        TrackKind::System => REMOTE_LABEL,
    }
}

fn speaker_select() -> String {
    format!(
        "SELECT spk.id, COALESCE({ASSIGNED_NAME_SQL}, spk.label), spk.source, spk.track_id
           FROM speakers spk
           {ASSIGNMENT_JOINS}"
    )
}

fn speaker_from_row(row: &Row<'_>) -> rusqlite::Result<Speaker> {
    Ok(Speaker {
        id: row.get(0)?,
        label: row.get(1)?,
        source: enum_col(row, 2, SpeakerSource::parse)?,
        track_id: row.get(3)?,
    })
}

/// In creation order, so "Me" and "Them" come first. The label is the
/// assigned participant's name when there is one.
pub fn list_speakers(conn: &Connection, meeting_id: &str) -> Result<Vec<Speaker>, String> {
    query_all(
        conn,
        "list speakers",
        &format!(
            "{} WHERE spk.meeting_id = ?1 ORDER BY spk.created_at ASC, spk.rowid ASC",
            speaker_select()
        ),
        params![meeting_id],
        speaker_from_row,
    )
}

pub fn get_speaker(conn: &Connection, speaker_id: &str) -> Result<Option<Speaker>, String> {
    query_opt(
        conn,
        "read speaker",
        &format!("{} WHERE spk.id = ?1", speaker_select()),
        params![speaker_id],
        speaker_from_row,
    )
}

/// Gives every track of the meeting its `source = 'track'` speaker: "Me" for
/// mic, "Them" for system. Idempotent, and a mic-only meeting only gets "Me".
/// Call it after the tracks are inserted. Returns the meeting's speakers.
pub fn seed_track_speakers(conn: &Connection, meeting_id: &str) -> Result<Vec<Speaker>, String> {
    transaction(conn, || {
        let unseeded = query_all(
            conn,
            "list tracks without a speaker",
            "SELECT t.id, t.kind FROM meeting_tracks t
              WHERE t.meeting_id = ?1
                AND NOT EXISTS (SELECT 1 FROM speakers s
                                 WHERE s.track_id = t.id AND s.source = 'track')
              ORDER BY t.kind ASC",
            params![meeting_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    enum_col(row, 1, TrackKind::parse)?,
                ))
            },
        )?;
        for (track_id, kind) in unseeded {
            insert_speaker(
                conn,
                meeting_id,
                Some(&track_id),
                track_speaker_label(kind),
                SpeakerSource::Track,
            )?;
        }
        list_speakers(conn, meeting_id)
    })
}

fn insert_speaker(
    conn: &Connection,
    meeting_id: &str,
    track_id: Option<&str>,
    label: &str,
    source: SpeakerSource,
) -> Result<String, String> {
    let id = new_id();
    execute(
        conn,
        "insert speaker",
        "INSERT INTO speakers (id, meeting_id, track_id, label, source, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, meeting_id, track_id, label, source.as_str(), now()],
    )?;
    Ok(id)
}

/// A speaker found by diarization ("Speaker 2") or added by hand. `track_id`
/// is the track they were heard on, when known.
pub fn add_speaker(
    conn: &Connection,
    meeting_id: &str,
    track_id: Option<&str>,
    label: &str,
    source: SpeakerSource,
) -> Result<Speaker, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("A speaker needs a label".to_string());
    }
    if let Some(track_id) = track_id {
        let owner = super::audio::track_meeting_id(conn, track_id)?;
        if owner.as_deref() != Some(meeting_id) {
            return Err(format!(
                "Track '{track_id}' does not belong to meeting '{meeting_id}'"
            ));
        }
    }
    let id = insert_speaker(conn, meeting_id, track_id, label, source)?;
    get_speaker(conn, &id)?.ok_or_else(|| format!("Speaker '{id}' not found"))
}

/// Renames the speaker's own label. An assigned participant's name still wins
/// where labels are shown.
pub fn rename_speaker(conn: &Connection, speaker_id: &str, label: &str) -> Result<(), String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("A speaker label cannot be empty".to_string());
    }
    let changed = execute(
        conn,
        "rename speaker",
        "UPDATE speakers SET label = ?2 WHERE id = ?1",
        params![speaker_id, label],
    )?;
    expect_found(changed, "Speaker", speaker_id)
}

/// Folds `from_id` into `into_id` in one transaction: its segments and turns
/// move over, its assignment moves unless `into_id` already has one, and the
/// `from_id` row is deleted. Both must belong to the same meeting. A track
/// speaker cannot be merged away: it is what unlabelled segments fall back to.
pub fn merge_speakers(conn: &Connection, from_id: &str, into_id: &str) -> Result<(), String> {
    if from_id == into_id {
        return Err("Cannot merge a speaker into itself".to_string());
    }
    transaction(conn, || {
        let read = |id: &str| -> Result<(String, SpeakerSource), String> {
            query_opt(
                conn,
                "read speaker",
                "SELECT meeting_id, source FROM speakers WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        enum_col(row, 1, SpeakerSource::parse)?,
                    ))
                },
            )?
            .ok_or_else(|| format!("Speaker '{id}' not found"))
        };
        let (from_meeting, from_source) = read(from_id)?;
        let (into_meeting, _) = read(into_id)?;
        if from_meeting != into_meeting {
            return Err("Cannot merge speakers of different meetings".to_string());
        }
        if from_source == SpeakerSource::Track {
            return Err("A track speaker cannot be merged into another speaker".to_string());
        }

        // OR IGNORE: a segment both speakers already share keeps the target's
        // row; the source's leftover goes with the speaker row below.
        execute(
            conn,
            "move segment speakers",
            "UPDATE OR IGNORE segment_speakers SET speaker_id = ?2 WHERE speaker_id = ?1",
            params![from_id, into_id],
        )?;
        execute(
            conn,
            "move speaker turns",
            "UPDATE speaker_turns SET speaker_id = ?2 WHERE speaker_id = ?1",
            params![from_id, into_id],
        )?;
        execute(
            conn,
            "move speaker assignment",
            "UPDATE OR IGNORE speaker_assignments SET speaker_id = ?2 WHERE speaker_id = ?1",
            params![from_id, into_id],
        )?;
        execute(
            conn,
            "delete merged speaker",
            "DELETE FROM speakers WHERE id = ?1",
            params![from_id],
        )?;
        Ok(())
    })
}

// --- Turns -----------------------------------------------------------------------

/// A stretch of a track where one speaker talks, as diarization found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerTurn {
    pub id: String,
    pub speaker_id: String,
    pub track_id: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Adds `(start_ms, end_ms)` turns for a speaker on a track, in one
/// transaction. Returns how many were added.
pub fn add_speaker_turns(
    conn: &Connection,
    speaker_id: &str,
    track_id: &str,
    turns: &[(u64, u64)],
) -> Result<usize, String> {
    transaction(conn, || {
        let mut stmt = conn
            .prepare(
                "INSERT INTO speaker_turns (id, speaker_id, track_id, start_ms, end_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .map_err(|e| format!("Failed to prepare insert speaker turns: {e}"))?;
        for (start_ms, end_ms) in turns {
            if end_ms < start_ms {
                return Err(format!(
                    "Speaker turn ends before it starts ({end_ms} < {start_ms})"
                ));
            }
            stmt.execute(params![
                new_id(),
                speaker_id,
                track_id,
                *start_ms as i64,
                *end_ms as i64
            ])
            .map_err(|e| format!("Failed to insert speaker turn: {e}"))?;
        }
        Ok(turns.len())
    })
}

/// Every turn of the meeting in timeline order.
pub fn list_speaker_turns(conn: &Connection, meeting_id: &str) -> Result<Vec<SpeakerTurn>, String> {
    query_all(
        conn,
        "list speaker turns",
        "SELECT tu.id, tu.speaker_id, tu.track_id, tu.start_ms, tu.end_ms
           FROM speaker_turns tu
           JOIN speakers s ON s.id = tu.speaker_id
          WHERE s.meeting_id = ?1
          ORDER BY tu.start_ms ASC, tu.end_ms ASC, tu.rowid ASC",
        params![meeting_id],
        |row| {
            Ok(SpeakerTurn {
                id: row.get(0)?,
                speaker_id: row.get(1)?,
                track_id: row.get(2)?,
                start_ms: u64_col(row, 3)?,
                end_ms: u64_col(row, 4)?,
            })
        },
    )
}

// --- Segment speakers --------------------------------------------------------------

/// Makes `speaker_id` the speaker of the segment, replacing whoever it was.
/// `confidence: None` is a manual reassignment; diarization passes its score.
/// Both must belong to the same meeting. Returns the segment as it now
/// displays.
pub fn assign_segment_speaker(
    conn: &Connection,
    segment_id: &str,
    speaker_id: &str,
    confidence: Option<f32>,
) -> Result<Segment, String> {
    transaction(conn, || {
        let same_meeting = query_opt(
            conn,
            "check segment speaker",
            "SELECT 1
               FROM transcript_segments seg
               JOIN transcript_runs run ON run.id = seg.run_id
               JOIN speakers s ON s.meeting_id = run.meeting_id
              WHERE seg.id = ?1 AND s.id = ?2",
            params![segment_id, speaker_id],
            |_| Ok(()),
        )?;
        if same_meeting.is_none() {
            return Err(format!(
                "Segment '{segment_id}' and speaker '{speaker_id}' are not in the same meeting"
            ));
        }
        execute(
            conn,
            "clear segment speaker",
            "DELETE FROM segment_speakers WHERE segment_id = ?1",
            params![segment_id],
        )?;
        execute(
            conn,
            "set segment speaker",
            "INSERT INTO segment_speakers (segment_id, speaker_id, confidence) VALUES (?1, ?2, ?3)",
            params![segment_id, speaker_id, confidence.map(f64::from)],
        )?;
        transcript::get_segment(conn, segment_id)?
            .ok_or_else(|| format!("Segment '{segment_id}' not found"))
    })
}

/// Back to the track speaker of the segment's track.
pub fn clear_segment_speaker(conn: &Connection, segment_id: &str) -> Result<(), String> {
    execute(
        conn,
        "clear segment speaker",
        "DELETE FROM segment_speakers WHERE segment_id = ?1",
        params![segment_id],
    )?;
    Ok(())
}

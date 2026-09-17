//! `people`, `participants` and `speaker_assignments`: who was in the meeting
//! and which voice is theirs.
//!
//! A person is shared between meetings and keyed by email. A participant is
//! that person (or just a name) in one meeting. An assignment links a speaker
//! to a participant; only a confirmed one changes the labels that are shown.

use rusqlite::{params, Connection, Row};

use super::super::types::ParticipantSource;
use super::{enum_col, execute, new_id, now, query_all, query_opt, transaction};

// --- People ------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub id: String,
    /// Trimmed and lowercased.
    pub email: String,
    pub display_name: Option<String>,
}

fn clean(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// One person per email, case-insensitive. A new `display_name` replaces the
/// old one; `None` keeps it.
pub fn upsert_person(
    conn: &Connection,
    email: &str,
    display_name: Option<&str>,
) -> Result<Person, String> {
    let email = email.trim().to_lowercase();
    if email.is_empty() {
        return Err("A person needs an email address".to_string());
    }
    let person = query_opt(
        conn,
        "upsert person",
        "INSERT INTO people (id, email, display_name, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(email) DO UPDATE
           SET display_name = COALESCE(excluded.display_name, people.display_name),
               updated_at = excluded.updated_at
         RETURNING id, email, display_name",
        params![new_id(), email, clean(display_name), now()],
        |row| {
            Ok(Person {
                id: row.get(0)?,
                email: row.get(1)?,
                display_name: row.get(2)?,
            })
        },
    )?;
    person.ok_or_else(|| "Upserting a person returned no row".to_string())
}

// --- Participants ------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NewParticipant {
    pub meeting_id: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub source: ParticipantSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    pub id: String,
    pub meeting_id: String,
    pub person_id: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub source: ParticipantSource,
}

const PARTICIPANT_SELECT: &str =
    "SELECT p.id, p.meeting_id, p.person_id, p.name, p.email, p.source FROM participants p";

fn participant_from_row(row: &Row<'_>) -> rusqlite::Result<Participant> {
    Ok(Participant {
        id: row.get(0)?,
        meeting_id: row.get(1)?,
        person_id: row.get(2)?,
        name: row.get(3)?,
        email: row.get(4)?,
        source: enum_col(row, 5, ParticipantSource::parse)?,
    })
}

/// Adds an attendee. With an email the person is upserted and linked, and an
/// attendee already in the meeting under that email is returned instead of
/// added twice (a missing name is filled in). Needs a name or an email.
pub fn add_participant(
    conn: &Connection,
    participant: &NewParticipant,
) -> Result<Participant, String> {
    let name = clean(participant.name.as_deref());
    let email = clean(participant.email.as_deref()).map(str::to_lowercase);
    if name.is_none() && email.is_none() {
        return Err("A participant needs a name or an email address".to_string());
    }
    transaction(conn, || {
        let mut person_id = None;
        if let Some(email) = email.as_deref() {
            person_id = Some(upsert_person(conn, email, name)?.id);
            let existing = query_opt(
                conn,
                "find participant",
                &format!("{PARTICIPANT_SELECT} WHERE p.meeting_id = ?1 AND p.email = ?2"),
                params![participant.meeting_id, email],
                participant_from_row,
            )?;
            if let Some(mut existing) = existing {
                if existing.name.is_none() && name.is_some() {
                    execute(
                        conn,
                        "name participant",
                        "UPDATE participants SET name = ?2 WHERE id = ?1",
                        params![existing.id, name],
                    )?;
                    existing.name = name.map(str::to_string);
                }
                return Ok(existing);
            }
        }
        let id = new_id();
        execute(
            conn,
            "insert participant",
            "INSERT INTO participants (id, meeting_id, person_id, name, email, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                participant.meeting_id,
                person_id,
                name,
                email,
                participant.source.as_str(),
                now(),
            ],
        )?;
        Ok(Participant {
            id,
            meeting_id: participant.meeting_id.clone(),
            person_id,
            name: name.map(str::to_string),
            email: email.clone(),
            source: participant.source,
        })
    })
}

/// In the order they were added.
pub fn list_participants(conn: &Connection, meeting_id: &str) -> Result<Vec<Participant>, String> {
    query_all(
        conn,
        "list participants",
        &format!("{PARTICIPANT_SELECT} WHERE p.meeting_id = ?1 ORDER BY p.created_at ASC, p.rowid ASC"),
        params![meeting_id],
        participant_from_row,
    )
}

/// Removes the attendee and, by cascade, the assignments pointing at them.
/// The person stays. `false` when there was no such participant.
pub fn remove_participant(conn: &Connection, participant_id: &str) -> Result<bool, String> {
    let changed = execute(
        conn,
        "remove participant",
        "DELETE FROM participants WHERE id = ?1",
        params![participant_id],
    )?;
    Ok(changed > 0)
}

// --- Speaker assignments -------------------------------------------------------------

/// `speaker_assignments.source`, which also carries the assignment's standing:
/// the schema has no separate status column. Only `manual` is confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentSource {
    /// The user picked the attendee. Confirmed: their name labels the speaker.
    Manual,
    /// The app's guess (a voiceprint match). Shown as a suggestion and never
    /// applied to a label until the user confirms it, which makes it `manual`.
    Suggested,
}

impl AssignmentSource {
    pub fn as_str(self) -> &'static str {
        match self {
            AssignmentSource::Manual => "manual",
            AssignmentSource::Suggested => "suggested",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "manual" => Some(AssignmentSource::Manual),
            "suggested" => Some(AssignmentSource::Suggested),
            _ => None,
        }
    }

    pub fn is_confirmed(self) -> bool {
        self == AssignmentSource::Manual
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerAssignment {
    pub speaker_id: String,
    pub participant_id: String,
    pub source: AssignmentSource,
}

/// Says which attendee a speaker is. One assignment per speaker: a new one
/// replaces the old. Both must belong to the same meeting.
pub fn assign_speaker(
    conn: &Connection,
    speaker_id: &str,
    participant_id: &str,
    source: AssignmentSource,
) -> Result<(), String> {
    let changed = execute(
        conn,
        "assign speaker",
        "INSERT INTO speaker_assignments (speaker_id, participant_id, source, created_at)
         SELECT s.id, p.id, ?3, ?4
           FROM speakers s
           JOIN participants p ON p.meeting_id = s.meeting_id
          WHERE s.id = ?1 AND p.id = ?2
         ON CONFLICT(speaker_id) DO UPDATE
           SET participant_id = excluded.participant_id,
               source = excluded.source,
               created_at = excluded.created_at",
        params![speaker_id, participant_id, source.as_str(), now()],
    )?;
    if changed == 0 {
        return Err(format!(
            "Speaker '{speaker_id}' and participant '{participant_id}' are not in the same meeting"
        ));
    }
    Ok(())
}

/// `false` when the speaker had no assignment.
pub fn unassign_speaker(conn: &Connection, speaker_id: &str) -> Result<bool, String> {
    let changed = execute(
        conn,
        "unassign speaker",
        "DELETE FROM speaker_assignments WHERE speaker_id = ?1",
        params![speaker_id],
    )?;
    Ok(changed > 0)
}

pub fn list_speaker_assignments(
    conn: &Connection,
    meeting_id: &str,
) -> Result<Vec<SpeakerAssignment>, String> {
    query_all(
        conn,
        "list speaker assignments",
        "SELECT sa.speaker_id, sa.participant_id, sa.source
           FROM speaker_assignments sa
           JOIN speakers s ON s.id = sa.speaker_id
          WHERE s.meeting_id = ?1
          ORDER BY sa.created_at ASC, sa.rowid ASC",
        params![meeting_id],
        |row| {
            Ok(SpeakerAssignment {
                speaker_id: row.get(0)?,
                participant_id: row.get(1)?,
                source: enum_col(row, 2, AssignmentSource::parse)?,
            })
        },
    )
}

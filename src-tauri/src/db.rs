use crate::storage::HistoryEntry;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Snippet {
    pub id: String,
    pub label: String,
    pub value: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub body: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Dictation {
    pub id: String,
    pub session_id: u64,
    pub started_at: String,
    pub wav_path: Option<String>,
    pub duration_ms: Option<u64>,
    pub sample_rate: Option<u32>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TranscriptionRow {
    pub id: String,
    pub dictation_id: String,
    pub model: String,
    pub text: Option<String>,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LabChoice {
    pub dictation_id: String,
    pub chosen_model: Option<String>,
    pub ground_truth: Option<String>,
    pub chosen_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Correction {
    pub id: String,
    pub dictation_id: String,
    pub model: String,
    pub wrong_text: String,
    pub intended_text: String,
    pub context_snippet: Option<String>,
    pub created_at: String,
}

const BUSY_TIMEOUT_MS: i64 = 5_000;
const SCHEMA_VERSION: i64 = 3;

fn db_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("FlowingThoughts")
        .join("flowing_thoughts.db"))
}

/// Pragmas every read-write connection needs. `busy_timeout` matters once the
/// meeting worker writes through its own connection: a writer waits for the
/// other one instead of failing with SQLITE_BUSY.
fn configure(conn: &Connection) -> Result<(), String> {
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| format!("Failed to enable WAL: {e}"))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| format!("Failed to enable foreign keys: {e}"))?;
    conn.pragma_update(None, "busy_timeout", BUSY_TIMEOUT_MS)
        .map_err(|e| format!("Failed to set busy_timeout: {e}"))?;
    Ok(())
}

fn open_at(path: &Path, run_migrations: bool) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create DB directory: {e}"))?;
    }
    let conn = Connection::open(path).map_err(|e| format!("Failed to open SQLite DB: {e}"))?;
    configure(&conn)?;
    if run_migrations {
        migrate(&conn)?;
    }
    Ok(conn)
}

/// The app's managed connection. Migrates; call once, at startup.
pub fn open() -> Result<Connection, String> {
    open_at(&db_path()?, true)
}

/// A second read-write connection to the already-migrated database, for a
/// thread that must not share the managed connection's mutex (the meeting
/// worker). Never migrates. WAL makes concurrent use safe.
#[allow(dead_code)] // scaffold: first used by meetings/worker.rs (WP6)
pub fn open_connection() -> Result<Connection, String> {
    open_at(&db_path()?, false)
}

/// Read-only handle on the live database, for tooling that must never
/// modify it (the eval harness and correction mining). `None` when the app
/// has not created a database yet.
pub fn open_read_only() -> Result<Option<Connection>, String> {
    let path = db_path()?;
    if !path.exists() {
        return Ok(None);
    }
    Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map(Some)
        .map_err(|e| format!("Failed to open SQLite DB read-only: {e}"))
}

fn migrate(conn: &Connection) -> Result<(), String> {
    migrate_to(conn, SCHEMA_VERSION)
}

/// Runs every migration above the stored `user_version`, up to and including
/// `target`. `target` exists so tests can build a database at an old version.
fn migrate_to(conn: &Connection, target: i64) -> Result<(), String> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| format!("Failed to read user_version: {e}"))?;

    if version < 1 && target >= 1 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS snippets (
               id TEXT PRIMARY KEY,
               label TEXT NOT NULL,
               value TEXT NOT NULL,
               kind TEXT NOT NULL,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS notes (
               id TEXT PRIMARY KEY,
               title TEXT NOT NULL,
               body TEXT NOT NULL,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS history (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               session_id INTEGER NOT NULL,
               text TEXT NOT NULL,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS kv (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             PRAGMA user_version = 1;
             COMMIT;",
        )
        .map_err(|e| format!("Migration v1 failed: {e}"))?;
    }

    if version < 2 && target >= 2 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS dictations (
               id TEXT PRIMARY KEY,
               session_id INTEGER NOT NULL,
               started_at TEXT NOT NULL,
               wav_path TEXT,
               duration_ms INTEGER,
               sample_rate INTEGER
             );
             CREATE TABLE IF NOT EXISTS transcriptions (
               id TEXT PRIMARY KEY,
               dictation_id TEXT NOT NULL REFERENCES dictations(id) ON DELETE CASCADE,
               model TEXT NOT NULL,
               text TEXT,
               latency_ms INTEGER,
               error TEXT,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS choices (
               dictation_id TEXT PRIMARY KEY REFERENCES dictations(id) ON DELETE CASCADE,
               chosen_model TEXT,
               ground_truth TEXT,
               chosen_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS corrections (
               id TEXT PRIMARY KEY,
               dictation_id TEXT NOT NULL REFERENCES dictations(id) ON DELETE CASCADE,
               model TEXT NOT NULL,
               wrong_text TEXT NOT NULL,
               intended_text TEXT NOT NULL,
               context_snippet TEXT,
               created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_transcriptions_dictation
               ON transcriptions(dictation_id);
             CREATE INDEX IF NOT EXISTS idx_corrections_model_wrong
               ON corrections(model, wrong_text);
             PRAGMA user_version = 2;
             COMMIT;",
        )
        .map_err(|e| format!("Migration v2 failed: {e}"))?;
    }

    // v3: meetings. Conventions: text UUID ids, RFC3339 text timestamps,
    // `*_ms` columns are milliseconds on the meeting timeline (0 = meeting
    // start), status/kind/source columns hold the `as_str()` of the matching
    // enum in `meetings/types.rs`. Everything hangs off `meetings` with
    // ON DELETE CASCADE, so deleting a meeting row removes all of it.
    if version < 3 && target >= 3 {
        conn.execute_batch(
            "BEGIN;

             -- Recording ---------------------------------------------------
             CREATE TABLE IF NOT EXISTS meetings (
               id TEXT PRIMARY KEY,
               title TEXT NOT NULL,
               status TEXT NOT NULL,
               started_at TEXT NOT NULL,
               ended_at TEXT,
               duration_ms INTEGER NOT NULL DEFAULT 0,
               language TEXT NOT NULL DEFAULT 'auto',
               model TEXT,
               echo_risk INTEGER NOT NULL DEFAULT 0,
               origin_host_ns INTEGER,
               active_run_id TEXT REFERENCES transcript_runs(id) ON DELETE SET NULL,
               audio_deleted_at TEXT,
               calendar_event_id TEXT,
               error TEXT,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS meeting_tracks (
               id TEXT PRIMARY KEY,
               meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
               kind TEXT NOT NULL,
               device_name TEXT,
               source_sample_rate INTEGER,
               source_channels INTEGER,
               overflow_frames INTEGER NOT NULL DEFAULT 0,
               created_at TEXT NOT NULL,
               UNIQUE (meeting_id, kind)
             );
             CREATE TABLE IF NOT EXISTS meeting_audio_chunks (
               id TEXT PRIMARY KEY,
               track_id TEXT NOT NULL REFERENCES meeting_tracks(id) ON DELETE CASCADE,
               seq INTEGER NOT NULL,
               path TEXT NOT NULL,
               status TEXT NOT NULL,
               anchor_host_ns INTEGER NOT NULL,
               start_ms INTEGER NOT NULL,
               n_frames INTEGER NOT NULL DEFAULT 0,
               sample_rate INTEGER NOT NULL DEFAULT 16000,
               opened_at TEXT NOT NULL,
               closed_at TEXT,
               UNIQUE (track_id, seq)
             );

             -- Transcript --------------------------------------------------
             CREATE TABLE IF NOT EXISTS transcript_runs (
               id TEXT PRIMARY KEY,
               meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
               model TEXT NOT NULL,
               language TEXT NOT NULL,
               params_json TEXT NOT NULL DEFAULT '{}',
               status TEXT NOT NULL,
               error TEXT,
               created_at TEXT NOT NULL,
               started_at TEXT,
               finished_at TEXT
             );
             CREATE TABLE IF NOT EXISTS transcript_windows (
               id TEXT PRIMARY KEY,
               run_id TEXT NOT NULL REFERENCES transcript_runs(id) ON DELETE CASCADE,
               track_id TEXT NOT NULL REFERENCES meeting_tracks(id) ON DELETE CASCADE,
               seq INTEGER NOT NULL,
               start_ms INTEGER NOT NULL,
               end_ms INTEGER NOT NULL,
               status TEXT NOT NULL,
               language TEXT,
               attempts INTEGER NOT NULL DEFAULT 0,
               error TEXT,
               decoded_at TEXT,
               UNIQUE (run_id, track_id, seq)
             );
             CREATE TABLE IF NOT EXISTS transcript_segments (
               id TEXT PRIMARY KEY,
               run_id TEXT NOT NULL REFERENCES transcript_runs(id) ON DELETE CASCADE,
               window_id TEXT NOT NULL REFERENCES transcript_windows(id) ON DELETE CASCADE,
               track_id TEXT NOT NULL REFERENCES meeting_tracks(id) ON DELETE CASCADE,
               seq INTEGER NOT NULL,
               start_ms INTEGER NOT NULL,
               end_ms INTEGER NOT NULL,
               text TEXT NOT NULL,
               lang TEXT,
               no_speech_prob REAL,
               avg_logprob REAL,
               suppressed_reason TEXT
             );
             CREATE TABLE IF NOT EXISTS segment_edits (
               segment_id TEXT PRIMARY KEY REFERENCES transcript_segments(id) ON DELETE CASCADE,
               text TEXT,
               hidden INTEGER,
               updated_at TEXT NOT NULL
             );

             -- Speakers and people -----------------------------------------
             CREATE TABLE IF NOT EXISTS speakers (
               id TEXT PRIMARY KEY,
               meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
               track_id TEXT REFERENCES meeting_tracks(id) ON DELETE CASCADE,
               label TEXT NOT NULL,
               source TEXT NOT NULL,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS speaker_turns (
               id TEXT PRIMARY KEY,
               speaker_id TEXT NOT NULL REFERENCES speakers(id) ON DELETE CASCADE,
               track_id TEXT NOT NULL REFERENCES meeting_tracks(id) ON DELETE CASCADE,
               start_ms INTEGER NOT NULL,
               end_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS segment_speakers (
               segment_id TEXT NOT NULL REFERENCES transcript_segments(id) ON DELETE CASCADE,
               speaker_id TEXT NOT NULL REFERENCES speakers(id) ON DELETE CASCADE,
               confidence REAL,
               PRIMARY KEY (segment_id, speaker_id)
             );
             CREATE TABLE IF NOT EXISTS people (
               id TEXT PRIMARY KEY,
               email TEXT NOT NULL UNIQUE COLLATE NOCASE,
               display_name TEXT,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS participants (
               id TEXT PRIMARY KEY,
               meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
               person_id TEXT REFERENCES people(id) ON DELETE SET NULL,
               name TEXT,
               email TEXT,
               source TEXT NOT NULL,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS speaker_assignments (
               speaker_id TEXT PRIMARY KEY REFERENCES speakers(id) ON DELETE CASCADE,
               participant_id TEXT NOT NULL REFERENCES participants(id) ON DELETE CASCADE,
               source TEXT NOT NULL,
               created_at TEXT NOT NULL
             );

             -- Summaries ---------------------------------------------------
             CREATE TABLE IF NOT EXISTS summaries (
               id TEXT PRIMARY KEY,
               meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
               run_id TEXT NOT NULL REFERENCES transcript_runs(id) ON DELETE CASCADE,
               provider TEXT NOT NULL,
               model TEXT NOT NULL,
               status TEXT NOT NULL,
               overview TEXT,
               error TEXT,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS summary_items (
               id TEXT PRIMARY KEY,
               summary_id TEXT NOT NULL REFERENCES summaries(id) ON DELETE CASCADE,
               kind TEXT NOT NULL,
               position INTEGER NOT NULL,
               text TEXT NOT NULL,
               owner TEXT,
               owner_participant_id TEXT REFERENCES participants(id) ON DELETE SET NULL,
               due_date TEXT
             );
             CREATE TABLE IF NOT EXISTS summary_item_sources (
               item_id TEXT NOT NULL REFERENCES summary_items(id) ON DELETE CASCADE,
               segment_id TEXT NOT NULL REFERENCES transcript_segments(id) ON DELETE CASCADE,
               PRIMARY KEY (item_id, segment_id)
             );

             -- Jobs --------------------------------------------------------
             CREATE TABLE IF NOT EXISTS jobs (
               id TEXT PRIMARY KEY,
               kind TEXT NOT NULL,
               meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
               run_id TEXT REFERENCES transcript_runs(id) ON DELETE CASCADE,
               status TEXT NOT NULL,
               priority INTEGER NOT NULL DEFAULT 0,
               attempts INTEGER NOT NULL DEFAULT 0,
               progress_done INTEGER NOT NULL DEFAULT 0,
               progress_total INTEGER NOT NULL DEFAULT 0,
               payload_json TEXT NOT NULL DEFAULT '{}',
               error TEXT,
               created_at TEXT NOT NULL,
               started_at TEXT,
               finished_at TEXT,
               updated_at TEXT NOT NULL
             );

             CREATE INDEX IF NOT EXISTS idx_meetings_started
               ON meetings(started_at);
             CREATE INDEX IF NOT EXISTS idx_meeting_audio_chunks_status
               ON meeting_audio_chunks(status);
             CREATE INDEX IF NOT EXISTS idx_transcript_runs_meeting
               ON transcript_runs(meeting_id, created_at);
             CREATE INDEX IF NOT EXISTS idx_transcript_windows_run_status
               ON transcript_windows(run_id, status);
             CREATE INDEX IF NOT EXISTS idx_transcript_segments_run_start
               ON transcript_segments(run_id, start_ms);
             CREATE INDEX IF NOT EXISTS idx_transcript_segments_window
               ON transcript_segments(window_id);
             CREATE INDEX IF NOT EXISTS idx_speakers_meeting
               ON speakers(meeting_id);
             CREATE INDEX IF NOT EXISTS idx_speaker_turns_speaker_start
               ON speaker_turns(speaker_id, start_ms);
             CREATE INDEX IF NOT EXISTS idx_segment_speakers_speaker
               ON segment_speakers(speaker_id);
             CREATE INDEX IF NOT EXISTS idx_participants_meeting
               ON participants(meeting_id);
             CREATE INDEX IF NOT EXISTS idx_summaries_meeting
               ON summaries(meeting_id, created_at);
             CREATE INDEX IF NOT EXISTS idx_summary_items_summary
               ON summary_items(summary_id, position);
             CREATE INDEX IF NOT EXISTS idx_summary_item_sources_segment
               ON summary_item_sources(segment_id);
             CREATE INDEX IF NOT EXISTS idx_jobs_status
               ON jobs(status, priority, created_at);
             CREATE INDEX IF NOT EXISTS idx_jobs_meeting
               ON jobs(meeting_id);

             PRAGMA user_version = 3;
             COMMIT;",
        )
        .map_err(|e| format!("Migration v3 failed: {e}"))?;
    }

    Ok(())
}

pub fn list_snippets(conn: &Connection) -> Result<Vec<Snippet>, String> {
    let mut stmt = conn
        .prepare("SELECT id, label, value, kind FROM snippets ORDER BY created_at ASC")
        .map_err(|e| format!("Failed to prepare list_snippets: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Snippet {
                id: row.get(0)?,
                label: row.get(1)?,
                value: row.get(2)?,
                kind: row.get(3)?,
            })
        })
        .map_err(|e| format!("Failed to query snippets: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Failed to read snippet row: {e}"))?);
    }
    Ok(out)
}

pub fn save_snippet(conn: &Connection, snippet: &Snippet) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO snippets (id, label, value, kind, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET
           label = excluded.label,
           value = excluded.value,
           kind = excluded.kind",
        params![snippet.id, snippet.label, snippet.value, snippet.kind, now],
    )
    .map_err(|e| format!("Failed to save snippet: {e}"))?;
    Ok(())
}

pub fn delete_snippet(conn: &Connection, id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM snippets WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete snippet: {e}"))?;
    Ok(())
}

pub fn list_notes(conn: &Connection) -> Result<Vec<Note>, String> {
    let mut stmt = conn
        .prepare("SELECT id, title, body, updated_at FROM notes ORDER BY updated_at DESC")
        .map_err(|e| format!("Failed to prepare list_notes: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Note {
                id: row.get(0)?,
                title: row.get(1)?,
                body: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })
        .map_err(|e| format!("Failed to query notes: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Failed to read note row: {e}"))?);
    }
    Ok(out)
}

pub fn save_note(conn: &Connection, note: &Note) -> Result<(), String> {
    conn.execute(
        "INSERT INTO notes (id, title, body, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(id) DO UPDATE SET
           title = excluded.title,
           body = excluded.body,
           updated_at = excluded.updated_at",
        params![note.id, note.title, note.body, note.updated_at],
    )
    .map_err(|e| format!("Failed to save note: {e}"))?;
    Ok(())
}

pub fn delete_note(conn: &Connection, id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM notes WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete note: {e}"))?;
    Ok(())
}

pub fn insert_history(conn: &Connection, entry: &HistoryEntry) -> Result<(), String> {
    conn.execute(
        "INSERT INTO history (session_id, text, created_at) VALUES (?1, ?2, ?3)",
        params![entry.session_id as i64, entry.text, entry.timestamp],
    )
    .map_err(|e| format!("Failed to insert history: {e}"))?;
    Ok(())
}

pub fn list_history(conn: &Connection, limit: i64) -> Result<Vec<HistoryEntry>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, text, created_at FROM history
             ORDER BY id DESC
             LIMIT ?1",
        )
        .map_err(|e| format!("Failed to prepare list_history: {e}"))?;
    let rows = stmt
        .query_map(params![limit], |row| {
            let session_id: i64 = row.get(0)?;
            Ok(HistoryEntry {
                session_id: session_id as u64,
                text: row.get(1)?,
                timestamp: row.get(2)?,
            })
        })
        .map_err(|e| format!("Failed to query history: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Failed to read history row: {e}"))?);
    }
    Ok(out)
}

/// Highest session id ever recorded. Used to seed the in-memory session
/// counter so ids stay unique across app restarts — reusing ids made the
/// history UI treat distinct dictations as the same entry.
pub fn max_history_session_id(conn: &Connection) -> Result<u64, String> {
    conn.query_row(
        "SELECT COALESCE(MAX(session_id), 0) FROM history",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|v| v.max(0) as u64)
    .map_err(|e| format!("Failed to read max session id: {e}"))
}

pub fn kv_get(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT value FROM kv WHERE key = ?1",
        params![key],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| format!("Failed to read kv[{key}]: {e}"))
}

pub fn kv_set(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO kv (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(|e| format!("Failed to write kv[{key}]: {e}"))?;
    Ok(())
}

pub fn insert_dictation(conn: &Connection, dictation: &Dictation) -> Result<(), String> {
    conn.execute(
        "INSERT INTO dictations (id, session_id, started_at, wav_path, duration_ms, sample_rate)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            dictation.id,
            dictation.session_id as i64,
            dictation.started_at,
            dictation.wav_path,
            dictation.duration_ms.map(|v| v as i64),
            dictation.sample_rate.map(|v| v as i64),
        ],
    )
    .map_err(|e| format!("Failed to insert dictation: {e}"))?;
    Ok(())
}

pub fn clear_wav_path(conn: &Connection, dictation_id: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE dictations SET wav_path = NULL WHERE id = ?1",
        params![dictation_id],
    )
    .map_err(|e| format!("Failed to clear wav_path: {e}"))?;
    Ok(())
}

pub fn insert_transcription(conn: &Connection, row: &TranscriptionRow) -> Result<(), String> {
    conn.execute(
        "INSERT INTO transcriptions (id, dictation_id, model, text, latency_ms, error, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.id,
            row.dictation_id,
            row.model,
            row.text,
            row.latency_ms.map(|v| v as i64),
            row.error,
            row.created_at,
        ],
    )
    .map_err(|e| format!("Failed to insert transcription: {e}"))?;
    Ok(())
}

pub fn list_transcriptions_for(
    conn: &Connection,
    dictation_id: &str,
) -> Result<Vec<TranscriptionRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, dictation_id, model, text, latency_ms, error, created_at
             FROM transcriptions
             WHERE dictation_id = ?1
             ORDER BY created_at ASC",
        )
        .map_err(|e| format!("Failed to prepare list_transcriptions_for: {e}"))?;
    let rows = stmt
        .query_map(params![dictation_id], |row| {
            let latency: Option<i64> = row.get(4)?;
            Ok(TranscriptionRow {
                id: row.get(0)?,
                dictation_id: row.get(1)?,
                model: row.get(2)?,
                text: row.get(3)?,
                latency_ms: latency.map(|v| v as u64),
                error: row.get(5)?,
                created_at: row.get(6)?,
            })
        })
        .map_err(|e| format!("Failed to query transcriptions: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Failed to read transcription row: {e}"))?);
    }
    Ok(out)
}

pub fn get_choice(conn: &Connection, dictation_id: &str) -> Result<Option<LabChoice>, String> {
    conn.query_row(
        "SELECT dictation_id, chosen_model, ground_truth, chosen_at
         FROM choices WHERE dictation_id = ?1",
        params![dictation_id],
        |row| {
            Ok(LabChoice {
                dictation_id: row.get(0)?,
                chosen_model: row.get(1)?,
                ground_truth: row.get(2)?,
                chosen_at: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(|e| format!("Failed to read choice: {e}"))
}

pub fn list_corrections(conn: &Connection) -> Result<Vec<Correction>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, dictation_id, model, wrong_text, intended_text, context_snippet, created_at
             FROM corrections
             ORDER BY created_at DESC",
        )
        .map_err(|e| format!("Failed to prepare list_corrections: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Correction {
                id: row.get(0)?,
                dictation_id: row.get(1)?,
                model: row.get(2)?,
                wrong_text: row.get(3)?,
                intended_text: row.get(4)?,
                context_snippet: row.get(5)?,
                created_at: row.get(6)?,
            })
        })
        .map_err(|e| format!("Failed to query corrections: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Failed to read correction row: {e}"))?);
    }
    Ok(out)
}

pub fn delete_correction(conn: &Connection, id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM corrections WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete correction: {e}"))?;
    Ok(())
}

/// Update the stored text for the history row tied to `session_id`. Used when
/// the user edits a dictation via the Home-page inline editor so the new text
/// persists across app restarts.
pub fn update_history_text(
    conn: &Connection,
    session_id: u64,
    new_text: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE history SET text = ?1 WHERE session_id = ?2",
        params![new_text, session_id as i64],
    )
    .map_err(|e| format!("Failed to update history text: {e}"))?;
    Ok(())
}

/// Return `(wrong, intended)` pairs for every correction in the table — used
/// to run post-transcription replacements on fresh text. Unlike
/// `top_mistranscribed_words`, this doesn't aggregate/dedupe by model.
pub fn list_correction_pairs(conn: &Connection) -> Result<Vec<(String, String)>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT wrong_text, intended_text FROM corrections
             GROUP BY wrong_text, intended_text
             ORDER BY MAX(created_at) DESC",
        )
        .map_err(|e| format!("Failed to prepare list_correction_pairs: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("Failed to query correction pairs: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Failed to read correction pair: {e}"))?);
    }
    Ok(out)
}

pub fn insert_correction(conn: &Connection, correction: &Correction) -> Result<(), String> {
    conn.execute(
        "INSERT INTO corrections (id, dictation_id, model, wrong_text, intended_text, context_snippet, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            correction.id,
            correction.dictation_id,
            correction.model,
            correction.wrong_text,
            correction.intended_text,
            correction.context_snippet,
            correction.created_at,
        ],
    )
    .map_err(|e| format!("Failed to insert correction: {e}"))?;
    Ok(())
}

pub fn list_recent_dictations(
    conn: &Connection,
    limit: i64,
) -> Result<Vec<Dictation>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, started_at, wav_path, duration_ms, sample_rate
             FROM dictations
             ORDER BY started_at DESC
             LIMIT ?1",
        )
        .map_err(|e| format!("Failed to prepare list_recent_dictations: {e}"))?;
    let rows = stmt
        .query_map(params![limit], |row| {
            let session_id: i64 = row.get(1)?;
            let duration: Option<i64> = row.get(4)?;
            let sample_rate: Option<i64> = row.get(5)?;
            Ok(Dictation {
                id: row.get(0)?,
                session_id: session_id as u64,
                started_at: row.get(2)?,
                wav_path: row.get(3)?,
                duration_ms: duration.map(|v| v as u64),
                sample_rate: sample_rate.map(|v| v as u32),
            })
        })
        .map_err(|e| format!("Failed to query dictations: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Failed to read dictation row: {e}"))?);
    }
    Ok(out)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MistranscribedWord {
    pub model: String,
    pub wrong_text: String,
    pub intended_text: String,
    pub occurrences: i64,
}

pub fn top_mistranscribed_words(
    conn: &Connection,
    model_filter: Option<&str>,
    limit: i64,
) -> Result<Vec<MistranscribedWord>, String> {
    let (sql, use_filter) = if model_filter.is_some() {
        (
            "SELECT model, wrong_text, intended_text, COUNT(*) AS occurrences
             FROM corrections
             WHERE model = ?1
             GROUP BY model, wrong_text, intended_text
             ORDER BY occurrences DESC
             LIMIT ?2",
            true,
        )
    } else {
        (
            "SELECT model, wrong_text, intended_text, COUNT(*) AS occurrences
             FROM corrections
             GROUP BY model, wrong_text, intended_text
             ORDER BY occurrences DESC
             LIMIT ?1",
            false,
        )
    };
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to prepare top_mistranscribed_words: {e}"))?;
    let mapper = |row: &rusqlite::Row<'_>| {
        Ok(MistranscribedWord {
            model: row.get(0)?,
            wrong_text: row.get(1)?,
            intended_text: row.get(2)?,
            occurrences: row.get(3)?,
        })
    };
    let rows_iter = if use_filter {
        stmt.query_map(params![model_filter.unwrap(), limit], mapper)
    } else {
        stmt.query_map(params![limit], mapper)
    }
    .map_err(|e| format!("Failed to query corrections: {e}"))?;
    let mut out = Vec::new();
    for row in rows_iter {
        out.push(row.map_err(|e| format!("Failed to read correction row: {e}"))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{migrate, migrate_to, open_at, BUSY_TIMEOUT_MS, SCHEMA_VERSION};
    use rusqlite::{params, Connection};

    const V3_TABLES: [&str; 17] = [
        "meetings",
        "meeting_tracks",
        "meeting_audio_chunks",
        "transcript_runs",
        "transcript_windows",
        "transcript_segments",
        "segment_edits",
        "speakers",
        "speaker_turns",
        "segment_speakers",
        "people",
        "participants",
        "speaker_assignments",
        "summaries",
        "summary_items",
        "summary_item_sources",
        "jobs",
    ];

    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn
    }

    fn user_version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap()
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .unwrap()
    }

    /// One meeting with a row in every v3 table that hangs off it, plus the
    /// shared person its participant points at.
    fn insert_full_meeting(conn: &Connection, id: &str) {
        let now = "2026-09-17T10:00:00+00:00";
        let sql = format!(
            "INSERT OR IGNORE INTO people (id, email, created_at, updated_at)
               VALUES ('person-anna', 'anna@example.com', '{now}', '{now}');
             INSERT INTO meetings (id, title, status, started_at, created_at, updated_at)
               VALUES ('{id}', 'Standup', 'stopped', '{now}', '{now}', '{now}');
             INSERT INTO meeting_tracks (id, meeting_id, kind, created_at)
               VALUES ('{id}-mic', '{id}', 'mic', '{now}'), ('{id}-sys', '{id}', 'system', '{now}');
             INSERT INTO meeting_audio_chunks
               (id, track_id, seq, path, status, anchor_host_ns, start_ms, opened_at)
               VALUES ('{id}-c0', '{id}-mic', 0, '{id}/mic/0.pcm', 'closed', 1, 0, '{now}'),
                      ('{id}-c1', '{id}-sys', 0, '{id}/system/0.pcm', 'open', 1, 0, '{now}');
             INSERT INTO transcript_runs (id, meeting_id, model, language, status, created_at)
               VALUES ('{id}-run', '{id}', 'whisper-small-q5', 'auto', 'done', '{now}');
             UPDATE meetings SET active_run_id = '{id}-run' WHERE id = '{id}';
             INSERT INTO transcript_windows (id, run_id, track_id, seq, start_ms, end_ms, status)
               VALUES ('{id}-w0', '{id}-run', '{id}-mic', 0, 0, 28000, 'done');
             INSERT INTO transcript_segments
               (id, run_id, window_id, track_id, seq, start_ms, end_ms, text)
               VALUES ('{id}-s0', '{id}-run', '{id}-w0', '{id}-mic', 0, 0, 1500, 'Goedemorgen');
             INSERT INTO segment_edits (segment_id, text, updated_at)
               VALUES ('{id}-s0', 'Goedemorgen allemaal', '{now}');
             INSERT INTO speakers (id, meeting_id, track_id, label, source, created_at)
               VALUES ('{id}-me', '{id}', '{id}-mic', 'Me', 'track', '{now}');
             INSERT INTO speaker_turns (id, speaker_id, track_id, start_ms, end_ms)
               VALUES ('{id}-t0', '{id}-me', '{id}-mic', 0, 1500);
             INSERT INTO segment_speakers (segment_id, speaker_id) VALUES ('{id}-s0', '{id}-me');
             INSERT INTO participants (id, meeting_id, person_id, name, email, source, created_at)
               VALUES ('{id}-p0', '{id}', 'person-anna', 'Anna', 'anna@example.com', 'manual', '{now}');
             INSERT INTO speaker_assignments (speaker_id, participant_id, source, created_at)
               VALUES ('{id}-me', '{id}-p0', 'manual', '{now}');
             INSERT INTO summaries (id, meeting_id, run_id, provider, model, status, created_at)
               VALUES ('{id}-sum', '{id}', '{id}-run', 'openrouter', 'm', 'done', '{now}');
             INSERT INTO summary_items (id, summary_id, kind, position, text)
               VALUES ('{id}-i0', '{id}-sum', 'action', 0, 'Ship it');
             INSERT INTO summary_item_sources (item_id, segment_id) VALUES ('{id}-i0', '{id}-s0');
             INSERT INTO jobs (id, kind, meeting_id, run_id, status, created_at, updated_at)
               VALUES ('{id}-job', 'transcribe', '{id}', '{id}-run', 'done', '{now}', '{now}');"
        );
        conn.execute_batch(&sql).expect("insert full meeting");
    }

    #[test]
    fn fresh_db_reaches_latest_schema() {
        let conn = memory_db();
        migrate(&conn).expect("migrate");
        assert_eq!(SCHEMA_VERSION, 3);
        assert_eq!(user_version(&conn), SCHEMA_VERSION);
        for table in ["snippets", "notes", "history", "kv", "dictations", "corrections"] {
            assert!(table_exists(&conn, table), "missing pre-v3 table {table}");
        }
        for table in V3_TABLES {
            assert!(table_exists(&conn, table), "missing v3 table {table}");
        }
        // Idempotent: a second launch changes nothing.
        migrate(&conn).expect("second migrate");
        assert_eq!(user_version(&conn), SCHEMA_VERSION);
    }

    #[test]
    fn v2_db_migrates_to_v3_and_keeps_its_data() {
        let conn = memory_db();
        migrate_to(&conn, 2).expect("migrate to v2");
        assert_eq!(user_version(&conn), 2);
        assert!(!table_exists(&conn, "meetings"));
        conn.execute_batch(
            "INSERT INTO notes (id, title, body, created_at, updated_at)
               VALUES ('n1', 'Keep me', 'body', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00');
             INSERT INTO history (session_id, text, created_at)
               VALUES (7, 'hallo wereld', '2026-01-01T00:00:00+00:00');
             INSERT INTO dictations (id, session_id, started_at) VALUES ('d1', 7, '2026-01-01T00:00:00+00:00');
             INSERT INTO corrections (id, dictation_id, model, wrong_text, intended_text, created_at)
               VALUES ('c1', 'd1', 'whisper-small-q5', 'tauri', 'Tauri', '2026-01-01T00:00:00+00:00');",
        )
        .unwrap();

        migrate(&conn).expect("migrate v2 -> v3");

        assert_eq!(user_version(&conn), 3);
        for table in V3_TABLES {
            assert!(table_exists(&conn, table), "missing v3 table {table}");
        }
        assert_eq!(super::list_notes(&conn).unwrap()[0].title, "Keep me");
        assert_eq!(super::list_history(&conn, 10).unwrap()[0].text, "hallo wereld");
        assert_eq!(super::list_corrections(&conn).unwrap()[0].intended_text, "Tauri");
    }

    #[test]
    fn deleting_a_meeting_cascades_to_everything_it_owns() {
        let conn = memory_db();
        migrate(&conn).unwrap();
        insert_full_meeting(&conn, "m1");
        insert_full_meeting(&conn, "m2");

        conn.execute("DELETE FROM meetings WHERE id = 'm1'", []).unwrap();

        // Every per-meeting table is back to exactly m2's single row (two for
        // tracks and chunks); people are shared and survive.
        for table in V3_TABLES {
            let expected = match table {
                "meeting_tracks" | "meeting_audio_chunks" => 2,
                _ => 1,
            };
            assert_eq!(count(&conn, table), expected, "rows left in {table}");
        }
        let leftover: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transcript_segments WHERE id LIKE 'm1-%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leftover, 0);
    }

    #[test]
    fn foreign_keys_reject_orphans_and_people_emails_are_unique() {
        let conn = memory_db();
        migrate(&conn).unwrap();
        let orphan = conn.execute(
            "INSERT INTO meeting_tracks (id, meeting_id, kind, created_at)
             VALUES ('t', 'no-such-meeting', 'mic', 'now')",
            [],
        );
        assert!(orphan.is_err());

        conn.execute(
            "INSERT INTO people (id, email, created_at, updated_at) VALUES ('p1', 'Anna@Example.com', 'now', 'now')",
            [],
        )
        .unwrap();
        let duplicate = conn.execute(
            "INSERT INTO people (id, email, created_at, updated_at) VALUES ('p2', 'anna@example.com', 'now', 'now')",
            [],
        );
        assert!(duplicate.is_err());
    }

    #[test]
    fn deleting_a_person_keeps_the_participant() {
        let conn = memory_db();
        migrate(&conn).unwrap();
        insert_full_meeting(&conn, "m1");
        conn.execute("DELETE FROM people WHERE id = 'person-anna'", []).unwrap();
        let person_id: Option<String> = conn
            .query_row("SELECT person_id FROM participants WHERE id = 'm1-p0'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(person_id, None);
    }

    #[test]
    fn second_connection_is_configured_and_does_not_migrate() {
        let dir = std::env::temp_dir().join(format!("ft-db-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("test.db");

        // Without the managed connection's migration there is no schema: a
        // worker connection must never be the one that creates it.
        let bare = open_at(&path, false).expect("open without migrating");
        assert_eq!(user_version(&bare), 0);
        drop(bare);

        let managed = open_at(&path, true).expect("open managed");
        let worker = open_at(&path, false).expect("open worker");
        assert_eq!(user_version(&worker), SCHEMA_VERSION);
        for conn in [&managed, &worker] {
            let timeout: i64 = conn.query_row("PRAGMA busy_timeout", [], |r| r.get(0)).unwrap();
            let fks: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
            let journal: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
            assert_eq!(timeout, BUSY_TIMEOUT_MS);
            assert_eq!(fks, 1);
            assert_eq!(journal.to_lowercase(), "wal");
        }

        // Both connections write to the same database.
        insert_full_meeting(&managed, "m1");
        worker
            .execute("UPDATE jobs SET status = 'running' WHERE id = 'm1-job'", [])
            .unwrap();
        let status: String = managed
            .query_row("SELECT status FROM jobs WHERE id = 'm1-job'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "running");

        drop(managed);
        drop(worker);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

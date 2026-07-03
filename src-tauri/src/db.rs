use crate::storage::HistoryEntry;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;

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

fn db_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("FlowingThoughts")
        .join("flowing_thoughts.db"))
}

pub fn open() -> Result<Connection, String> {
    let path = db_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create DB directory: {e}"))?;
    }
    let conn = Connection::open(&path).map_err(|e| format!("Failed to open SQLite DB: {e}"))?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| format!("Failed to enable WAL: {e}"))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| format!("Failed to enable foreign keys: {e}"))?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<(), String> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| format!("Failed to read user_version: {e}"))?;

    if version < 1 {
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

    if version < 2 {
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

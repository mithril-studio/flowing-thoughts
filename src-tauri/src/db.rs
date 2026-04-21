use rusqlite::{params, Connection};
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

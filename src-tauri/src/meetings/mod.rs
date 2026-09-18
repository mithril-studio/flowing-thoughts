//! Meeting recording and transcription.
//!
//! WP1 (scaffold) owns this file, `types.rs`, `events.rs` and `commands.rs`,
//! and they are final: later packages fill in their own files and never need
//! to touch these, `lib.rs`, `db.rs`, `storage.rs` or `Cargo.toml`.
//!
//! | File | Owner | What |
//! |---|---|---|
//! | `types.rs`, `events.rs`, `commands.rs` | WP1 | contracts, events, command delegates |
//! | `store.rs` | WP2 | typed access to every v3 table |
//! | `longform.rs` | WP3 | VAD windows, long-form decoding, flagging |
//! | `recording/` | WP4 | chunk writer, timeline, resampler, recovery |
//! | `capture/` | WP5 | mic, system tap, permission, device watch |
//! | `jobs.rs`, `worker.rs` | WP6 | job queue and the `meeting-worker` thread |
//! | `session.rs` | WP7 | start/pause/stop state machine, tray, launch recovery |
//! | `echo.rs` | WP9 | flags mic segments that echo the system track |
//! | `summary.rs` | WP10 | BYOK summary |
//! | `export.rs` | WP7 | Markdown export |
//!
//! State lives here in the backend; React only renders the three events in
//! `events.rs`. Everything is persisted as it happens, because the app exits
//! through `_exit(0)`.

pub mod capture;
pub mod commands;
pub mod echo;
pub mod events;
pub mod export;
pub mod jobs;
pub mod longform;
pub mod recording;
pub mod session;
pub mod store;
pub mod summary;
pub mod types;
pub mod worker;

use std::sync::{Arc, Mutex};

/// The managed UI connection (`lib.rs` manages it). The worker never uses
/// this one: it opens its own with `db::open_connection()`.
pub type DbState = Arc<Mutex<rusqlite::Connection>>;
/// The managed persisted state: settings and the OpenRouter key.
pub type PersistedHandle = Arc<Mutex<crate::storage::PersistedState>>;

/// Called once from `lib.rs` setup, after the DB, the settings and the tray
/// exist. Recovery runs before the worker starts, so the worker's first look
/// at the queue already includes the jobs recovery put back. A failed
/// recovery is logged and never keeps the worker, or the app, from starting.
pub fn init(app: &tauri::AppHandle) {
    if let Err(e) = session::init(app) {
        let _ =
            crate::storage::append_log("ERROR", &format!("Meetings: launch recovery failed: {e}"));
    }
    if let Err(e) = worker::start(app) {
        let _ =
            crate::storage::append_log("ERROR", &format!("Meetings: worker failed to start: {e}"));
    }
}

/// What the menu bar title falls back to when dictation is idle: the
/// recording dot plus duration during a meeting, otherwise nothing. `lib.rs`
/// calls this so a finished dictation does not wipe the meeting's title.
///
/// "Nothing" is the empty title, never `None`: tray-icon's `set_title(None)`
/// does nothing on macOS, which would leave dictation's own "●" or "…" in the
/// menu bar for good.
pub fn idle_tray_title() -> Option<String> {
    Some(session::tray_title().unwrap_or_default())
}

/// Called right before the process exits through `_exit(0)`. Nothing may
/// depend on it (a crash skips it); it only gets a chance to close the open
/// chunks cleanly. Must return within a second.
pub fn shutdown() {
    session::shutdown();
}

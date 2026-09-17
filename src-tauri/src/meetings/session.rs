//! OWNER: WP7 (session). The recording state machine, the tray title and
//! launch recovery.
//!
//! Phases: `idle -> starting -> recording <-> paused -> stopping -> idle`
//! (`types::RecordingPhase`). One meeting at a time. Every transition is
//! persisted through `store` first and announced with `meeting-state` after;
//! creating, finishing and deleting a meeting emit `meeting-updated`.
//!
//! ```text
//! command (start/pause/resume/stop)          meeting-session thread (one per meeting)
//!   validates, inserts the rows,      --->   microphone, then the system tap (F2)
//!   waits at most COMMAND_WAIT               loop: commands | tick (notices, devices,
//!   for the answer                                 recorder errors, tray title)
//! ```
//!
//! - Everything that can block (opening devices, the HAL, joining the writer
//!   threads) happens on the session thread. A command waits a couple of
//!   seconds for its answer and otherwise returns the status as it is; the
//!   outcome then arrives as `meeting-state`.
//! - The system tap starts on a thread of its own (`meeting-tap-start`),
//!   after the microphone delivers. Creating it blocks for as long as the
//!   System Audio Recording prompt is on screen (over a minute, seen with the
//!   bundled app), and pause and stop must not wait for that. The track
//!   joins the meeting when it gets there; one that arrives after the stop is
//!   stopped again.
//! - No system audio means a microphone-only meeting, never an error: the
//!   system track row is removed and `RecordingStatus::tracks` says so.
//! - A failure never leaves the session stuck: sources and recorders are
//!   stopped, the meeting becomes `failed` with a message in
//!   `meetings.error`, and the phase goes back to idle. Audio that reached
//!   the disk stays and can be transcribed with "Re-transcribe".
//! - The DB lock is never held while a recorder is stopped: the writer thread
//!   closes its chunk through the same connection (`StoreLedger`).
//! - Tray: `● 12:34` while recording, `❙❙ 12:34` while paused. Dictation's
//!   title wins while a dictation is active (`session-phase`); `lib.rs` falls
//!   back to `tray_title()` when it ends. Recording is never covert.
//! - Dictation keeps working during a meeting: it has its own cpal stream and
//!   the session never touches the inference gate.
//!
//! The state machine is `Session`, which only knows a `SessionEnv` (sources,
//! events, tray, clock), so the tests drive it with fake sources, a temp
//! directory and a temp database. The functions at the bottom are the fixed
//! entry points `commands.rs` and `meetings/mod.rs` call; they use the one
//! `Session` of the process. It is a static and not Tauri managed state
//! because `tray_title()` and `shutdown()` are called without an `AppHandle`.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tauri::{AppHandle, Listener, Manager};

use super::capture::{self, SystemAudioMonitor};
use super::recording::{
    self, record_to_disk, recovery, ChunkWriterConfig, RecorderConfig, TrackRecorder, CHUNK_FRAMES,
};
use super::types::{
    AudioSource, ChunkLedger, ChunkRecord, MeetingChange, MeetingLanguage, MeetingStatus,
    RecordingPhase, RecordingStatus, RetranscribeOptions, SourceFormat, StartMeetingOptions,
    TrackKind, TARGET_SAMPLE_RATE,
};
use super::{events, jobs, store, worker, DbState, PersistedHandle};

/// How long a command waits for the session thread before it returns the
/// status as it is. The outcome still arrives as `meeting-state`.
const COMMAND_WAIT: Duration = Duration::from_secs(2);
/// `shutdown()` runs right before `_exit(0)` and must not hold up the quit.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(900);
/// How often the session thread looks at the recorders, the system-audio
/// monitor and the tray title when no command arrives.
const TICK: Duration = Duration::from_millis(250);
/// How long `start` waits for the system tap before it answers. A tap that
/// works is up in well under a second. One that waits for the user to answer
/// the System Audio Recording prompt takes as long as they do, and joins the
/// meeting when it gets there.
const TAP_GRACE: Duration = Duration::from_millis(1_500);

const RECORDING_MARK: &str = "●";
const PAUSED_MARK: &str = "❙❙";

/// Never from an audio callback. Silent in tests, which must not write to the
/// user's real log file. Never transcript text.
fn log(level: &str, message: &str) {
    recording::log(level, message);
}

// ---------------------------------------------------------------------------
// The environment: everything that is not the state machine
// ---------------------------------------------------------------------------

/// The session's view of the system track while it records.
pub(crate) trait SystemMonitor: Send {
    /// "No system audio detected", permission denied, or not delivering.
    fn has_notice(&self) -> bool;
    /// The output is the built-in speakers right now.
    fn echo_risk(&self) -> bool;
}

impl SystemMonitor for SystemAudioMonitor {
    fn has_notice(&self) -> bool {
        self.notice().is_problem()
    }

    fn echo_risk(&self) -> bool {
        SystemAudioMonitor::echo_risk(self)
    }
}

pub(crate) struct OpenedTap {
    pub source: Box<dyn AudioSource>,
    pub monitor: Option<Box<dyn SystemMonitor>>,
}

/// What the state machine needs from the rest of the app. `TauriEnv` is the
/// real one; the tests use fakes.
pub(crate) trait SessionEnv: Send + Sync {
    /// `capture::support()`: the OS gate for the whole feature.
    fn support(&self) -> Result<(), String>;
    /// The mach host clock both tracks are stamped on.
    fn host_now_ns(&self) -> u64;
    /// Whether the output is the built-in speakers. May touch the HAL: only
    /// called on the session thread.
    fn echo_risk(&self) -> bool;
    fn open_mic(&self) -> Box<dyn AudioSource>;
    /// `Err` means a microphone-only meeting.
    fn open_system_tap(&self) -> Result<OpenedTap, String>;
    fn state_changed(&self, status: &RecordingStatus);
    fn meeting_updated(&self, meeting_id: &str, change: MeetingChange);
    fn set_tray_title(&self, title: Option<String>);
}

struct TauriEnv {
    app: AppHandle,
}

impl SessionEnv for TauriEnv {
    fn support(&self) -> Result<(), String> {
        capture::support()
    }

    fn host_now_ns(&self) -> u64 {
        capture::host_now_ns()
    }

    fn echo_risk(&self) -> bool {
        capture::output_is_builtin_speakers()
    }

    fn open_mic(&self) -> Box<dyn AudioSource> {
        Box::new(capture::open_mic())
    }

    fn open_system_tap(&self) -> Result<OpenedTap, String> {
        let tap = capture::open_system_tap()?;
        let monitor = tap.monitor();
        Ok(OpenedTap { source: Box::new(tap), monitor: Some(Box::new(monitor)) })
    }

    fn state_changed(&self, status: &RecordingStatus) {
        events::emit_state(&self.app, status);
    }

    fn meeting_updated(&self, meeting_id: &str, change: MeetingChange) {
        events::emit_updated(&self.app, meeting_id, change);
    }

    fn set_tray_title(&self, title: Option<String>) {
        if let Some(tray) = self.app.tray_by_id(crate::TRAY_ICON_ID) {
            // tray-icon ignores `None` on macOS: only an empty title clears
            // the old one.
            let _ = tray.set_title(Some(title.unwrap_or_default()));
        }
    }
}

/// `chunk_opened` is `insert_chunk`, `chunk_closed` is `close_chunk`, on the
/// managed connection. Runs on the recorder's writer thread, once a minute
/// per track.
struct StoreLedger {
    db: DbState,
}

impl ChunkLedger for StoreLedger {
    fn chunk_opened(&mut self, chunk: &ChunkRecord) -> Result<(), String> {
        let conn = self.db.lock().map_err(|_| "DB lock poisoned".to_string())?;
        store::insert_chunk(&conn, chunk)
    }

    fn chunk_closed(&mut self, chunk_id: &str, n_frames: u64) -> Result<(), String> {
        let conn = self.db.lock().map_err(|_| "DB lock poisoned".to_string())?;
        store::close_chunk(&conn, chunk_id, n_frames)
    }
}

// ---------------------------------------------------------------------------
// Live state
// ---------------------------------------------------------------------------

/// What `status()` and the tray title are made from. Only the session thread
/// and `start` write it; the lock is held for a few field reads at most.
struct Live {
    phase: RecordingPhase,
    meeting_id: Option<String>,
    started_at: Option<String>,
    /// Recorded time up to the last pause.
    recorded: Duration,
    /// Set while the phase is `recording`.
    running_since: Option<Instant>,
    tracks: Vec<TrackKind>,
    system_audio_silent: bool,
    echo_risk: bool,
}

impl Live {
    fn idle() -> Self {
        Self {
            phase: RecordingPhase::Idle,
            meeting_id: None,
            started_at: None,
            recorded: Duration::ZERO,
            running_since: None,
            tracks: Vec::new(),
            system_audio_silent: false,
            echo_risk: false,
        }
    }

    fn elapsed(&self) -> Duration {
        self.recorded + self.running_since.map_or(Duration::ZERO, |since| since.elapsed())
    }

    /// Stops the clock, as a pause and a stop do.
    fn hold_clock(&mut self) {
        self.recorded = self.elapsed();
        self.running_since = None;
    }

    fn status(&self) -> RecordingStatus {
        RecordingStatus {
            phase: self.phase,
            meeting_id: self.meeting_id.clone(),
            started_at: self.started_at.clone(),
            elapsed_ms: self.elapsed().as_millis() as u64,
            tracks: self.tracks.clone(),
            system_audio_silent: self.system_audio_silent,
            echo_risk: self.echo_risk,
        }
    }
}

/// `5_000` is `0:05`, an hour and a bit is `1:02:03`.
pub(crate) fn format_elapsed(elapsed_ms: u64) -> String {
    let seconds = elapsed_ms / 1_000;
    let (h, m, s) = (seconds / 3_600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The menu bar title of a meeting: nothing when idle, the bare dot while the
/// devices open and close, the dot or the pause mark plus the recorded time
/// in between.
pub(crate) fn tray_title_for(phase: RecordingPhase, elapsed_ms: u64) -> Option<String> {
    match phase {
        RecordingPhase::Idle => None,
        RecordingPhase::Starting | RecordingPhase::Stopping => Some(RECORDING_MARK.to_string()),
        RecordingPhase::Recording => Some(format!("{RECORDING_MARK} {}", format_elapsed(elapsed_ms))),
        RecordingPhase::Paused => Some(format!("{PAUSED_MARK} {}", format_elapsed(elapsed_ms))),
    }
}

/// Whether a `session-phase` payload means a dictation is in progress. The
/// same reading as the tray listener in `lib.rs`.
fn dictation_is_active(session_phase_payload: &str) -> bool {
    ["recording", "transcribing", "injecting"].iter().any(|p| session_phase_payload.contains(p))
}

// ---------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct SessionConfig {
    pub command_wait: Duration,
    pub tap_grace: Duration,
    pub tick: Duration,
    pub recorder: RecorderConfig,
    /// Frames per chunk file. `CHUNK_FRAMES` (60 s); tests use less.
    pub chunk_frames: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            command_wait: COMMAND_WAIT,
            tap_grace: TAP_GRACE,
            tick: TICK,
            recorder: RecorderConfig::default(),
            chunk_frames: CHUNK_FRAMES,
        }
    }
}

/// What a new meeting is made from: the options of the command, completed
/// from `settings.meetings`.
#[derive(Debug, Clone)]
pub(crate) struct NewSession {
    pub title: Option<String>,
    pub language: MeetingLanguage,
    pub model: Option<String>,
}

type Reply = Sender<Result<RecordingStatus, String>>;

enum Control {
    Pause(Reply),
    Resume(Reply),
    Stop {
        reply: Reply,
        /// Right before `_exit(0)`: close the chunks first, the devices after.
        quitting: bool,
    },
}

/// The rows of the meeting being recorded.
#[derive(Clone)]
struct ActiveMeeting {
    id: String,
    origin_host_ns: u64,
    mic_track_id: String,
    system_track_id: String,
}

/// One track while it records: the source, its recorder and what was last
/// persisted about it.
struct LiveTrack {
    kind: TrackKind,
    track_id: String,
    source: Box<dyn AudioSource>,
    recorder: TrackRecorder,
    device: (Option<String>, Option<SourceFormat>),
    overflow_reported: u64,
    failed: bool,
}

enum Outcome {
    Stopped,
    Failed(String),
}

/// What starting the system tap came to: the track and its monitor, or why
/// the meeting is microphone-only.
type TapStart = Result<(LiveTrack, Option<Box<dyn SystemMonitor>>), String>;

pub(crate) struct Session {
    env: Arc<dyn SessionEnv>,
    db: DbState,
    /// The meetings audio root.
    root: PathBuf,
    config: SessionConfig,
    live: Mutex<Live>,
    control: Mutex<Option<Sender<Control>>>,
    dictation_active: AtomicBool,
    /// The title last put on the tray, so a tick only touches it when the
    /// second changed.
    tray_shown: Mutex<Option<Option<String>>>,
}

impl Session {
    pub(crate) fn new(
        env: Arc<dyn SessionEnv>,
        db: DbState,
        root: PathBuf,
        config: SessionConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            env,
            db,
            root,
            config,
            live: Mutex::new(Live::idle()),
            control: Mutex::new(None),
            dictation_active: AtomicBool::new(false),
            tray_shown: Mutex::new(None),
        })
    }

    fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.db.lock().map_err(|_| "DB lock poisoned".to_string())
    }

    /// A store write that must not stop the recording when it fails.
    fn persist(&self, what: &str, f: impl FnOnce(&Connection) -> Result<(), String>) {
        if let Err(e) = self.conn().and_then(|conn| f(&conn)) {
            log("ERROR", &format!("Meetings: failed to {what}: {e}"));
        }
    }

    pub(crate) fn status(&self) -> RecordingStatus {
        self.live().status()
    }

    pub(crate) fn tray_title(&self) -> Option<String> {
        let live = self.live();
        tray_title_for(live.phase, live.elapsed().as_millis() as u64)
    }

    /// The meeting being recorded, in any phase but idle.
    fn live_meeting_id(&self) -> Option<String> {
        self.live().meeting_id.clone()
    }

    /// Announces the state as it is now: `meeting-state`, then the tray.
    /// Call it after the transition was persisted.
    fn announce(&self) -> RecordingStatus {
        let status = self.status();
        self.env.state_changed(&status);
        self.refresh_tray(true);
        status
    }

    /// Dictation owns the title while it is active; `lib.rs` restores ours
    /// from `tray_title()` when it ends.
    fn refresh_tray(&self, force: bool) {
        if self.dictation_active.load(Ordering::Relaxed) {
            return;
        }
        let title = self.tray_title();
        let mut shown = self.tray_shown.lock().unwrap_or_else(|e| e.into_inner());
        if force || shown.as_ref() != Some(&title) {
            self.env.set_tray_title(title.clone());
            *shown = Some(title);
        }
    }

    /// From the `session-phase` listener.
    pub(crate) fn set_dictation_active(&self, active: bool) {
        self.dictation_active.store(active, Ordering::Relaxed);
        if !active {
            // `lib.rs` has just put `tray_title()` back, or is about to.
            *self.tray_shown.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    // --- Commands ------------------------------------------------------------

    pub(crate) fn start(self: &Arc<Self>, new: NewSession) -> Result<RecordingStatus, String> {
        self.env.support().map_err(|reason| {
            format!("Meetings cannot be recorded on this Mac: {reason}. Dictation keeps working.")
        })?;
        {
            let mut live = self.live();
            if live.phase != RecordingPhase::Idle {
                return Err("A meeting is already being recorded. Stop it first.".to_string());
            }
            // Claimed under the lock, so a second start is refused from here.
            *live = Live { phase: RecordingPhase::Starting, ..Live::idle() };
        }

        let created = self.create_meeting(&new);
        let (meeting, started_at) = match created {
            Ok(created) => created,
            Err(e) => {
                *self.live() = Live::idle();
                return Err(format!("The meeting could not be created: {e}"));
            }
        };
        {
            let mut live = self.live();
            live.meeting_id = Some(meeting.id.clone());
            live.started_at = Some(started_at);
        }
        self.announce();
        self.env.meeting_updated(&meeting.id, MeetingChange::Created);

        let (control_tx, control_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        *self.control.lock().unwrap_or_else(|e| e.into_inner()) = Some(control_tx);
        let session = self.clone();
        let meeting_id = meeting.id.clone();
        let spawned = std::thread::Builder::new()
            .name("meeting-session".to_string())
            .spawn(move || session.run_guarded(meeting, control_rx, started_tx));
        if let Err(e) = spawned {
            let message = format!("Failed to start the meeting thread: {e}");
            self.close_meeting(&meeting_id, Outcome::Failed(message.clone()));
            return Err(message);
        }

        match started_rx.recv_timeout(self.config.command_wait) {
            Ok(Ok(())) => Ok(self.status()),
            Ok(Err(message)) => Err(message),
            // Still opening devices (a Bluetooth headset takes seconds), or
            // the thread is gone and has already announced why.
            Err(_) => Ok(self.status()),
        }
    }

    pub(crate) fn pause(&self) -> Result<RecordingStatus, String> {
        if self.live().phase != RecordingPhase::Recording {
            return Err("No meeting is being recorded.".to_string());
        }
        self.send(Control::Pause, self.config.command_wait)
    }

    pub(crate) fn resume(&self) -> Result<RecordingStatus, String> {
        if self.live().phase != RecordingPhase::Paused {
            return Err("No meeting is paused.".to_string());
        }
        self.send(Control::Resume, self.config.command_wait)
    }

    pub(crate) fn stop(&self) -> Result<RecordingStatus, String> {
        if self.live().phase == RecordingPhase::Idle {
            return Err("No meeting is being recorded.".to_string());
        }
        self.send(|reply| Control::Stop { reply, quitting: false }, self.config.command_wait)
    }

    /// Best-effort clean close right before `_exit(0)`.
    pub(crate) fn shutdown(&self) {
        if self.live().phase != RecordingPhase::Idle {
            let _ = self.send(|reply| Control::Stop { reply, quitting: true }, SHUTDOWN_WAIT);
        }
    }

    fn send(
        &self,
        control: impl FnOnce(Reply) -> Control,
        wait: Duration,
    ) -> Result<RecordingStatus, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let sent = self
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|tx| tx.send(control(reply_tx)).is_ok());
        if !sent {
            // The session thread has just ended the meeting by itself.
            return Ok(self.status());
        }
        match reply_rx.recv_timeout(wait) {
            Ok(result) => result,
            // Queued behind slow device work; `meeting-state` follows.
            Err(_) => Ok(self.status()),
        }
    }

    // --- Rows ----------------------------------------------------------------

    /// The meeting, both tracks and the "Me" / "Them" speakers, in one
    /// transaction. Returns the rows and `started_at`.
    fn create_meeting(&self, new: &NewSession) -> Result<(ActiveMeeting, String), String> {
        let origin_host_ns = self.env.host_now_ns();
        let title = new
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Meeting {}", chrono::Local::now().format("%Y-%m-%d %H:%M")));
        let conn = self.conn()?;
        store::transaction(&conn, || {
            let id = store::insert_meeting(
                &conn,
                &store::NewMeeting {
                    title: title.clone(),
                    language: new.language,
                    model: new.model.clone().filter(|m| !m.trim().is_empty()),
                    origin_host_ns: Some(origin_host_ns),
                    calendar_event_id: None,
                },
            )?;
            let track = |kind| {
                store::insert_track(
                    &conn,
                    &store::NewTrack { meeting_id: id.clone(), kind, device_name: None, format: None },
                )
            };
            let mic_track_id = track(TrackKind::Mic)?;
            let system_track_id = track(TrackKind::System)?;
            store::seed_track_speakers(&conn, &id)?;
            let started_at = store::get_meeting(&conn, &id)?
                .map(|m| m.meeting.started_at)
                .ok_or_else(|| "the meeting row is missing".to_string())?;
            Ok((ActiveMeeting { id, origin_host_ns, mic_track_id, system_track_id }, started_at))
        })
    }

    // --- The session thread ----------------------------------------------------

    fn run_guarded(
        self: Arc<Self>,
        meeting: ActiveMeeting,
        control: Receiver<Control>,
        started: Sender<Result<(), String>>,
    ) {
        let meeting_id = meeting.id.clone();
        let result = catch_unwind(AssertUnwindSafe(|| self.run(meeting, control, started)));
        if result.is_err() {
            // The tracks were dropped by the unwind, which stops the sources
            // and closes the chunks.
            log("ERROR", "Meetings: the session thread panicked");
            self.close_meeting(
                &meeting_id,
                Outcome::Failed("Recording stopped because of an internal error.".to_string()),
            );
        }
    }

    fn run(
        self: &Arc<Self>,
        meeting: ActiveMeeting,
        control: Receiver<Control>,
        started: Sender<Result<(), String>>,
    ) {
        if self.env.echo_risk() {
            self.note_echo_risk(&meeting.id);
        }

        // The microphone first, and only then the tap: the other way round
        // stalls the HAL for seconds with a Bluetooth headset (spike F2).
        let mut tracks: Vec<LiveTrack> = Vec::new();
        match self.open_track(&meeting, self.env.open_mic()) {
            Ok(track) => tracks.push(track),
            Err(e) => {
                let message = format!("The microphone could not be started: {e}");
                self.finish(&meeting, tracks, Outcome::Failed(message.clone()), false);
                let _ = started.send(Err(message));
                return;
            }
        }
        // The rows say `recording` since they were inserted.
        {
            let mut live = self.live();
            live.phase = RecordingPhase::Recording;
            live.running_since = Some(Instant::now());
            live.tracks = vec![TrackKind::Mic];
        }
        self.announce();

        // The tap starts on a thread of its own: creating it blocks for as
        // long as the System Audio Recording prompt is on screen, and the
        // meeting and its controls must not wait for that.
        let mut monitor: Option<Box<dyn SystemMonitor>> = None;
        let mut pending_tap = Some(self.start_tap(&meeting));
        let tap_deadline = Instant::now() + self.config.tap_grace;
        while pending_tap.is_some() && Instant::now() < tap_deadline {
            self.poll_tap(&meeting, &mut pending_tap, &mut tracks, &mut monitor, self.config.tick);
        }
        let _ = started.send(Ok(()));

        // `pending_tap` is dropped with the meeting: a tap that arrives after
        // that finds nobody listening and is abandoned (`start_tap`).
        loop {
            let command = control.recv_timeout(self.config.tick);
            self.poll_tap(&meeting, &mut pending_tap, &mut tracks, &mut monitor, Duration::ZERO);
            match command {
                Ok(Control::Pause(reply)) => {
                    let _ = reply.send(self.set_paused(&meeting, &tracks, true));
                }
                Ok(Control::Resume(reply)) => {
                    let _ = reply.send(self.set_paused(&meeting, &tracks, false));
                }
                Ok(Control::Stop { reply, quitting }) => {
                    let _ = reply.send(Ok(self.finish(&meeting, tracks, Outcome::Stopped, quitting)));
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(error) = self.observe(&meeting, &mut tracks, monitor.as_deref()) {
                        let message = format!(
                            "Recording stopped early: {error}. What was recorded until then is kept; use Re-transcribe to transcribe it."
                        );
                        self.finish(&meeting, tracks, Outcome::Failed(message), false);
                        return;
                    }
                    self.refresh_tray(false);
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.finish(&meeting, tracks, Outcome::Stopped, false);
                    return;
                }
            }
        }
    }

    /// Opens and starts the system tap on a thread of its own. The result
    /// comes back over the channel; when the meeting is over by then, the tap
    /// is stopped again right there.
    fn start_tap(self: &Arc<Self>, meeting: &ActiveMeeting) -> Receiver<TapStart> {
        let (tx, rx) = mpsc::channel();
        let (session, meeting) = (self.clone(), meeting.clone());
        let spawned = std::thread::Builder::new().name("meeting-tap-start".to_string()).spawn(move || {
            let result = session.env.open_system_tap().and_then(|opened| {
                let track = session.open_track(&meeting, opened.source)?;
                Ok((track, opened.monitor))
            });
            if let Err(mpsc::SendError(result)) = tx.send(result) {
                session.abandon_tap(&meeting, result);
            }
        });
        if let Err(e) = spawned {
            // The sender went with the closure: the receiver reports it as a
            // tap that could not start.
            log("WARN", &format!("Meetings: failed to start the system audio thread: {e}"));
        }
        rx
    }

    /// Takes the tap's result if it is there (waiting up to `wait` for it):
    /// the system track joins the meeting, or the meeting is settled as
    /// microphone-only.
    fn poll_tap(
        &self,
        meeting: &ActiveMeeting,
        pending: &mut Option<Receiver<TapStart>>,
        tracks: &mut Vec<LiveTrack>,
        monitor: &mut Option<Box<dyn SystemMonitor>>,
        wait: Duration,
    ) {
        let Some(rx) = pending.as_ref() else { return };
        let result = match rx.recv_timeout(wait) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => return,
            Err(RecvTimeoutError::Disconnected) => Err("the system audio thread ended".to_string()),
        };
        *pending = None;
        match result {
            Ok((track, tap_monitor)) => {
                if self.live().phase == RecordingPhase::Paused {
                    track.recorder.pause();
                }
                tracks.push(track);
                *monitor = tap_monitor;
                self.live().tracks.push(TrackKind::System);
                self.observe(meeting, tracks, monitor.as_deref());
                self.announce();
            }
            Err(e) => {
                log("WARN", &format!("Meetings: recording the microphone only: {e}"));
                self.discard_system_track(meeting);
            }
        }
    }

    /// The tap came up after its meeting had ended.
    fn abandon_tap(&self, meeting: &ActiveMeeting, result: TapStart) {
        let recorded = match result {
            Ok((mut track, _)) => {
                let _ = track.source.stop();
                track.recorder.stop().written_frames > 0
            }
            Err(_) => false,
        };
        log("INFO", "Meetings: system audio came up after the meeting had ended");
        if !recorded {
            self.discard_system_track(meeting);
        }
    }

    /// The recorder, then the source into it. `Err` leaves nothing running.
    fn open_track(
        &self,
        meeting: &ActiveMeeting,
        mut source: Box<dyn AudioSource>,
    ) -> Result<LiveTrack, String> {
        let kind = source.kind();
        let track_id = match kind {
            TrackKind::Mic => &meeting.mic_track_id,
            TrackKind::System => &meeting.system_track_id,
        };
        let mut writer = ChunkWriterConfig::new(
            self.root.clone(),
            &meeting.id,
            track_id,
            kind,
            meeting.origin_host_ns,
        );
        writer.chunk_frames = self.config.chunk_frames;
        let ledger = Box::new(StoreLedger { db: self.db.clone() });
        let (mut recorder, handler) = record_to_disk(writer, ledger, self.config.recorder.clone())?;
        if let Err(e) = source.start(Box::new(handler)) {
            recorder.stop();
            return Err(e);
        }
        let mut track = LiveTrack {
            kind,
            track_id: track_id.clone(),
            source,
            recorder,
            device: (None, None),
            overflow_reported: 0,
            failed: false,
        };
        self.persist_device(&mut track);
        Ok(track)
    }

    fn persist_device(&self, track: &mut LiveTrack) {
        let device = (track.source.device_name(), track.source.format());
        // A source between two devices reports nothing: keep the last one.
        if device.1.is_none() || device == track.device {
            return;
        }
        self.persist("record the track's device", |conn| {
            store::set_track_device(conn, &track.track_id, device.0.as_deref(), device.1)
        });
        track.device = device;
    }

    /// No system audio: the meeting has one track and one speaker.
    fn discard_system_track(&self, meeting: &ActiveMeeting) {
        self.persist("remove the system track", |conn| {
            store::delete_track(conn, &meeting.system_track_id)
        });
        if let Ok(dir) = recording::track_dir(&self.root, &meeting.id, TrackKind::System) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    fn note_echo_risk(&self, meeting_id: &str) {
        self.persist("record the echo risk", |conn| store::set_echo_risk(conn, meeting_id, true));
        self.live().echo_risk = true;
    }

    /// One look at the tracks: overflow, device changes, the system-audio
    /// notice and the echo risk. Announces what changed. Returns the error
    /// that ends the meeting, if the microphone track has failed.
    fn observe(
        &self,
        meeting: &ActiveMeeting,
        tracks: &mut [LiveTrack],
        monitor: Option<&dyn SystemMonitor>,
    ) -> Option<String> {
        let mut changed = false;
        for track in tracks.iter_mut() {
            let status = track.recorder.status();
            self.persist_overflow(track, status.overflow_frames);
            self.persist_device(track);
            let Some(error) = status.error.filter(|_| !track.failed) else { continue };
            track.failed = true;
            if track.kind == TrackKind::Mic {
                return Some(error);
            }
            // The meeting carries on with the microphone.
            log("WARN", &format!("Meetings: the system track stopped recording: {error}"));
            let _ = track.source.stop();
            self.live().tracks.retain(|kind| *kind != TrackKind::System);
            changed = true;
        }

        if let Some(monitor) = monitor {
            let system_live = self.live().tracks.contains(&TrackKind::System);
            let silent = system_live && monitor.has_notice();
            // Sticky for the meeting: some of it was recorded over speakers.
            if monitor.echo_risk() && !self.live().echo_risk {
                self.note_echo_risk(&meeting.id);
                changed = true;
            }
            let mut live = self.live();
            if live.system_audio_silent != silent {
                live.system_audio_silent = silent;
                changed = true;
            }
        }
        if changed {
            self.announce();
        }
        None
    }

    fn persist_overflow(&self, track: &mut LiveTrack, overflow_frames: u64) {
        let new = overflow_frames.saturating_sub(track.overflow_reported);
        if new > 0 {
            self.persist("record lost frames", |conn| {
                store::add_track_overflow(conn, &track.track_id, new)
            });
            track.overflow_reported = overflow_frames;
        }
    }

    /// A pause is a gap on the timeline, never silence on disk: the recorders
    /// drop what arrives and close their chunk. The devices stay open.
    fn set_paused(
        &self,
        meeting: &ActiveMeeting,
        tracks: &[LiveTrack],
        paused: bool,
    ) -> Result<RecordingStatus, String> {
        let (from, to, status) = if paused {
            (RecordingPhase::Recording, RecordingPhase::Paused, MeetingStatus::Paused)
        } else {
            (RecordingPhase::Paused, RecordingPhase::Recording, MeetingStatus::Recording)
        };
        if self.live().phase != from {
            return Err(if paused { "No meeting is being recorded." } else { "No meeting is paused." }
                .to_string());
        }
        self.persist("record the pause", |conn| {
            store::set_meeting_status(conn, &meeting.id, status, None)
        });
        for track in tracks {
            if paused {
                track.recorder.pause();
            } else {
                track.recorder.resume();
            }
        }
        {
            let mut live = self.live();
            live.hold_clock();
            if !paused {
                live.running_since = Some(Instant::now());
            }
            live.phase = to;
        }
        Ok(self.announce())
    }

    /// Ends the meeting, whatever state it is in: devices and recorders
    /// stopped, chunks closed, rows settled, phase idle. Never fails.
    fn finish(
        &self,
        meeting: &ActiveMeeting,
        mut tracks: Vec<LiveTrack>,
        outcome: Outcome,
        quitting: bool,
    ) -> RecordingStatus {
        {
            let mut live = self.live();
            live.hold_clock();
            live.phase = RecordingPhase::Stopping;
        }
        self.announce();

        let stop_sources = |tracks: &mut Vec<LiveTrack>| {
            for track in tracks.iter_mut() {
                if let Err(e) = track.source.stop() {
                    log("WARN", &format!("Meetings: stopping the {} source: {e}", track.kind.as_str()));
                }
            }
        };
        if quitting {
            // There is a second at most and a HAL call may take longer: the
            // chunks and the rows go first, the devices get what is left.
            self.stop_recorders(&mut tracks);
            let status = self.close_meeting(&meeting.id, outcome);
            stop_sources(&mut tracks);
            return status;
        }
        // The devices first, so the recorders drain a ring nothing writes to
        // any more.
        stop_sources(&mut tracks);
        self.stop_recorders(&mut tracks);
        drop(tracks);
        self.close_meeting(&meeting.id, outcome)
    }

    /// No DB lock may be held here: the writer threads close their chunks
    /// through the ledger while they are joined.
    fn stop_recorders(&self, tracks: &mut [LiveTrack]) {
        for track in tracks.iter_mut() {
            let status = track.recorder.stop();
            self.persist_overflow(track, status.overflow_frames);
            if let Some(error) = status.error {
                log("WARN", &format!("Meetings: the {} track ended with an error: {error}", track.kind.as_str()));
            }
        }
    }

    /// Settles the rows and goes back to idle. A stopped meeting is queued
    /// for transcription; one without audio, or one that failed, is `failed`
    /// with the reason.
    fn close_meeting(&self, meeting_id: &str, outcome: Outcome) -> RecordingStatus {
        let duration_ms = {
            let mut live = self.live();
            live.hold_clock();
            live.recorded.as_millis() as u64
        };
        let mut empty = false;
        let settled = self.conn().and_then(|conn| {
            store::transaction(&conn, || {
                store::finish_meeting(&conn, meeting_id, &chrono::Utc::now().to_rfc3339(), duration_ms)?;
                empty = !store::meeting_has_frames(&conn, meeting_id)?;
                if empty {
                    // Empty chunk files are not audio: nothing to transcribe
                    // again, nothing to delete later.
                    store::mark_audio_deleted(&conn, meeting_id)?;
                }
                let failure = match &outcome {
                    Outcome::Failed(message) => Some(message.clone()),
                    Outcome::Stopped if empty => Some("Nothing was recorded.".to_string()),
                    Outcome::Stopped => {
                        // `enqueue_transcription` refuses a meeting that still
                        // says `recording`. What it refuses for (no model
                        // chosen) is shown on the meeting; the audio is kept.
                        store::set_meeting_status(&conn, meeting_id, MeetingStatus::Queued, None)?;
                        jobs::enqueue_transcription(&conn, meeting_id, &RetranscribeOptions::default())
                            .err()
                    }
                };
                if let Some(message) = &failure {
                    store::set_meeting_status(&conn, meeting_id, MeetingStatus::Failed, Some(message))?;
                }
                Ok(())
            })
        });
        match settled {
            Ok(()) if empty => {
                let _ = recording::delete_meeting_audio_in(&self.root, meeting_id);
            }
            Ok(()) => {}
            // Launch recovery picks the meeting up: it still says `recording`.
            Err(e) => log("ERROR", &format!("Meetings: failed to settle meeting {meeting_id}: {e}")),
        }
        // The job was inserted inside a transaction: the worker may have
        // looked before the commit.
        worker::wake();

        *self.control.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.live() = Live::idle();
        let status = self.announce();
        self.env.meeting_updated(meeting_id, MeetingChange::Status);
        status
    }

    // --- Deleting ----------------------------------------------------------------

    /// Rows (cascade) and the audio directory. Refused for the meeting being
    /// recorded; cancels its unfinished job first, so the worker lets go.
    pub(crate) fn delete_meeting(&self, meeting_id: &str) -> Result<(), String> {
        if self.live_meeting_id().as_deref() == Some(meeting_id) {
            return Err("This meeting is still being recorded. Stop it first.".to_string());
        }
        {
            let conn = self.conn()?;
            if store::get_meeting(&conn, meeting_id)?.is_none() {
                return Err(format!("Meeting '{meeting_id}' not found"));
            }
            store::cancel_unfinished_jobs(&conn, meeting_id)?;
        }
        // Files before rows: if this fails nothing else has changed and the
        // user can try again; the other way round would leave audio behind
        // that nothing points at.
        recording::delete_meeting_audio_in(&self.root, meeting_id)?;
        store::delete_meeting(&*self.conn()?, meeting_id)?;
        self.env.meeting_updated(meeting_id, MeetingChange::Deleted);
        Ok(())
    }

    /// Audio files only: chunks become `deleted`, `audio_deleted_at` is set,
    /// the transcript stays. Refused while recording or while a job is
    /// unfinished (it still needs the audio).
    pub(crate) fn delete_meeting_audio(&self, meeting_id: &str) -> Result<(), String> {
        if self.live_meeting_id().as_deref() == Some(meeting_id) {
            return Err("This meeting is still being recorded. Stop it first.".to_string());
        }
        {
            let conn = self.conn()?;
            if store::get_meeting(&conn, meeting_id)?.is_none() {
                return Err(format!("Meeting '{meeting_id}' not found"));
            }
            if store::job_progress(&conn, meeting_id)?.is_some() {
                return Err(
                    "This meeting is still being transcribed. Delete the audio once that is done."
                        .to_string(),
                );
            }
        }
        recording::delete_meeting_audio_in(&self.root, meeting_id)?;
        store::mark_audio_deleted(&*self.conn()?, meeting_id)?;
        self.env.meeting_updated(meeting_id, MeetingChange::AudioDeleted);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Launch recovery
// ---------------------------------------------------------------------------

/// What launch recovery did to one meeting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveredMeeting {
    pub meeting_id: String,
    /// Chunks that were still `open`, or that only the sidecar knew about.
    pub chunks_repaired: usize,
    /// Whether a transcription job is queued for what survived.
    pub queued: bool,
}

/// Nothing can be recording when the app has just started. Every chunk still
/// `open` is repaired from its file (`recording::recovery`), whichever meeting
/// it belongs to. Every meeting still `recording` or `paused` becomes
/// `interrupted`; its end and duration are filled in, and whatever audio
/// survived is queued for transcription, which moves the meeting on to
/// `queued`. A meeting nothing survived of stays `interrupted`. Jobs left
/// `running` go back to `queued`.
pub(crate) fn recover_at_launch(conn: &Connection, root: &Path) -> Result<Vec<RecoveredMeeting>, String> {
    let requeued = jobs::recover_at_launch(conn)?;
    if requeued > 0 {
        log("INFO", &format!("Meetings: requeued {requeued} interrupted job(s)"));
    }
    let interrupted =
        store::list_meetings_with_status(conn, &[MeetingStatus::Recording, MeetingStatus::Paused])?;
    // A chunk can also be left open under a meeting that did stop: a ledger
    // write that failed, a quit between the stop and the last close.
    let open_chunks = recovery::recover_chunks(&store::list_open_chunks(conn)?, root)?;
    let mut repaired_tracks = Vec::new();
    store::transaction(conn, || {
        for chunk in &open_chunks {
            store::mark_chunk_recovered(conn, &chunk.id, chunk.n_frames)?;
            repaired_tracks.push(chunk.track_id.clone());
        }
        Ok(())
    })?;
    let mut recovered = Vec::new();
    for item in interrupted {
        match store::transaction(conn, || {
            recover_meeting(conn, root, &item.id, &item.started_at, &repaired_tracks)
        }) {
            Ok(meeting) => {
                log(
                    "INFO",
                    &format!(
                        "Meetings: recovered interrupted meeting {} ({} chunk(s) repaired, {})",
                        meeting.meeting_id,
                        meeting.chunks_repaired,
                        if meeting.queued { "queued for transcription" } else { "no audio survived" },
                    ),
                );
                recovered.push(meeting);
            }
            Err(e) => log("ERROR", &format!("Meetings: recovering meeting {} failed: {e}", item.id)),
        }
    }
    Ok(recovered)
}

fn recover_meeting(
    conn: &Connection,
    root: &Path,
    meeting_id: &str,
    started_at: &str,
    repaired_tracks: &[String],
) -> Result<RecoveredMeeting, String> {
    store::set_meeting_status(conn, meeting_id, MeetingStatus::Interrupted, None)?;

    // The sidecars are repaired in place and list every chunk file, also one
    // whose row never made it into the database.
    let sidecars = match recovery::recover_meeting_dir(&recording::meeting_dir(root, meeting_id)?) {
        Ok(tracks) => tracks,
        Err(e) => {
            log("WARN", &format!("Meetings: recovery could not read the audio of {meeting_id}: {e}"));
            Vec::new()
        }
    };

    let mut chunks_repaired = 0;
    let (mut recorded_frames, mut end_ms) = (0u64, 0u64);
    for track in store::list_tracks(conn, meeting_id)? {
        let rows = store::list_chunks(conn, &track.id)?;
        chunks_repaired += repaired_tracks.iter().filter(|id| **id == track.id).count();
        let on_disk = sidecars
            .iter()
            .filter_map(|t| t.sidecar.as_ref())
            .filter(|sidecar| sidecar.track_id == track.id)
            .flat_map(|sidecar| sidecar.chunk_records());
        for chunk in on_disk.filter(|chunk| !rows.iter().any(|row| row.id == chunk.id)) {
            store::insert_chunk(conn, &chunk)?;
            chunks_repaired += 1;
        }

        let chunks = store::list_chunks(conn, &track.id)?;
        let rate = u64::from(TARGET_SAMPLE_RATE);
        recorded_frames = recorded_frames.max(chunks.iter().map(|c| c.n_frames).sum());
        end_ms = end_ms.max(
            chunks.iter().map(|c| c.start_ms + c.n_frames * 1_000 / rate).max().unwrap_or(0),
        );
    }

    // Pauses excluded, like a normal stop: the longest track's audio.
    let duration_ms = recorded_frames * 1_000 / u64::from(TARGET_SAMPLE_RATE);
    let ended_at = chrono::DateTime::parse_from_rfc3339(started_at)
        .map(|start| (start + chrono::Duration::milliseconds(end_ms as i64)).to_rfc3339())
        .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339());
    store::finish_meeting(conn, meeting_id, &ended_at, duration_ms)?;

    let queued = recorded_frames > 0
        && match jobs::enqueue_transcription(conn, meeting_id, &RetranscribeOptions::default()) {
            Ok(_) => true,
            Err(e) => {
                // The audio is intact; "Re-transcribe" still works.
                store::set_meeting_status(conn, meeting_id, MeetingStatus::Interrupted, Some(&e))?;
                false
            }
        };
    Ok(RecoveredMeeting { meeting_id: meeting_id.to_string(), chunks_repaired, queued })
}

// ---------------------------------------------------------------------------
// The fixed entry points (`commands.rs`, `meetings/mod.rs`)
// ---------------------------------------------------------------------------

static SESSION: OnceLock<Arc<Session>> = OnceLock::new();

fn session() -> Result<&'static Arc<Session>, String> {
    SESSION.get().ok_or_else(|| "Meetings are not ready yet.".to_string())
}

/// Launch recovery, before the worker starts, then the session itself.
pub fn init(app: &AppHandle) -> Result<(), String> {
    let db = app
        .try_state::<DbState>()
        .ok_or_else(|| "The database is not available yet".to_string())?
        .inner()
        .clone();
    let root = recording::meetings_root()?;

    let recovery = db
        .lock()
        .map_err(|_| "DB lock poisoned".to_string())
        .and_then(|conn| recover_at_launch(&conn, &root));
    // Aggregate devices a crashed run left behind. The HAL can take its time:
    // not on the thread that is setting up the app.
    let _ = std::thread::Builder::new()
        .name("meeting-cleanup".to_string())
        .spawn(capture::cleanup_leaked_devices);

    let session = Session::new(
        Arc::new(TauriEnv { app: app.clone() }),
        db,
        root,
        SessionConfig::default(),
    );
    if SESSION.set(session.clone()).is_ok() {
        app.listen("session-phase", move |event| {
            session.set_dictation_active(dictation_is_active(event.payload()));
        });
    }
    recovery.map(|_| ())
}

pub fn start(app: &AppHandle, options: StartMeetingOptions) -> Result<RecordingStatus, String> {
    let new = {
        let persisted = app
            .try_state::<PersistedHandle>()
            .ok_or_else(|| "Settings are not available yet".to_string())?;
        let state = persisted.inner().lock().map_err(|_| "Persisted state lock poisoned".to_string())?;
        let settings = &state.settings.meetings;
        if !settings.enabled {
            return Err("Meetings are turned off. Enable them in Settings → Meetings.".to_string());
        }
        NewSession {
            title: options.title,
            language: options
                .language
                .or_else(|| MeetingLanguage::parse(&settings.language))
                .unwrap_or(MeetingLanguage::Auto),
            model: Some(settings.model.clone()),
        }
    };
    session()?.start(new)
}

pub fn pause(_app: &AppHandle) -> Result<RecordingStatus, String> {
    session()?.pause()
}

pub fn resume(_app: &AppHandle) -> Result<RecordingStatus, String> {
    session()?.resume()
}

pub fn stop(_app: &AppHandle) -> Result<RecordingStatus, String> {
    session()?.stop()
}

/// The live status. Idle is a status, not an error.
pub fn status(_app: &AppHandle) -> Result<RecordingStatus, String> {
    Ok(SESSION.get().map_or_else(RecordingStatus::idle, |session| session.status()))
}

pub fn delete_meeting(_app: &AppHandle, meeting_id: &str) -> Result<(), String> {
    session()?.delete_meeting(meeting_id)
}

pub fn delete_meeting_audio(_app: &AppHandle, meeting_id: &str) -> Result<(), String> {
    session()?.delete_meeting_audio(meeting_id)
}

/// Menu bar title while a meeting records (e.g. "● 12:34"), else `None`.
/// Called from the `session-phase` listener in `lib.rs`: cheap, non-blocking.
pub fn tray_title() -> Option<String> {
    SESSION.get().and_then(|session| session.tray_title())
}

/// Best-effort clean close right before `_exit(0)`. Returns within a second.
/// Correctness never depends on it: launch recovery handles a crash.
pub fn shutdown() {
    if let Some(session) = SESSION.get() {
        session.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::meetings::capture::fake::FakeSource;
    use crate::meetings::recording::test_support::TempDir;
    use crate::meetings::recording::chunk_writer::ChunkWriter;
    use crate::meetings::recording::BYTES_PER_FRAME;
    use crate::meetings::types::{
        AudioSourceHandler, ChunkStatus, JobStatus, SampleSink, SpeakerSource,
    };
    use crate::meetings::worker::test_support::{new_meeting, TestDb};

    const ORIGIN_NS: u64 = 1_000_000_000_000;
    const MS: u64 = 1_000_000;
    const MIC_FORMAT: SourceFormat = SourceFormat { sample_rate: 48_000, channels: 1 };
    const TAP_FORMAT: SourceFormat = SourceFormat { sample_rate: 48_000, channels: 2 };
    /// Half-second chunk files, so a short test sees rotation.
    const TEST_CHUNK_FRAMES: u64 = 8_000;

    /// A `FakeSource` the test keeps a hand on while the session owns it.
    #[derive(Clone)]
    struct SharedSource(Arc<Mutex<FakeSource>>);

    impl SharedSource {
        fn new(source: FakeSource) -> Self {
            Self(Arc::new(Mutex::new(source)))
        }

        fn deliver(&self, seconds: f64, host_time_ns: u64) -> u64 {
            self.0.lock().unwrap().deliver_constant(0.1, seconds, host_time_ns)
        }

        fn stops(&self) -> usize {
            self.0.lock().unwrap().stops
        }
    }

    impl AudioSource for SharedSource {
        fn kind(&self) -> TrackKind {
            self.0.lock().unwrap().kind()
        }
        fn device_name(&self) -> Option<String> {
            self.0.lock().unwrap().device_name()
        }
        fn format(&self) -> Option<SourceFormat> {
            self.0.lock().unwrap().format()
        }
        fn start(&mut self, handler: Box<dyn AudioSourceHandler>) -> Result<SourceFormat, String> {
            self.0.lock().unwrap().start(handler)
        }
        fn stop(&mut self) -> Result<(), String> {
            self.0.lock().unwrap().stop()
        }
    }

    /// A tap that is stuck the way the real one is while macOS shows the
    /// System Audio Recording prompt: `start` blocks until the test opens the
    /// gate.
    struct GatedSource {
        inner: SharedSource,
        gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }

    impl AudioSource for GatedSource {
        fn kind(&self) -> TrackKind {
            self.inner.kind()
        }
        fn device_name(&self) -> Option<String> {
            self.inner.device_name()
        }
        fn format(&self) -> Option<SourceFormat> {
            self.inner.format()
        }
        fn start(&mut self, handler: Box<dyn AudioSourceHandler>) -> Result<SourceFormat, String> {
            let (open, signal) = &*self.gate;
            let mut open = open.lock().unwrap();
            while !*open {
                open = signal.wait(open).unwrap();
            }
            drop(open);
            self.inner.start(handler)
        }
        fn stop(&mut self) -> Result<(), String> {
            self.inner.stop()
        }
    }

    #[derive(Default)]
    struct FakeMonitor {
        notice: AtomicBool,
        echo_risk: AtomicBool,
    }

    impl SystemMonitor for Arc<FakeMonitor> {
        fn has_notice(&self) -> bool {
            self.notice.load(Ordering::Relaxed)
        }
        fn echo_risk(&self) -> bool {
            self.echo_risk.load(Ordering::Relaxed)
        }
    }

    struct FakeEnv {
        supported: bool,
        mic: SharedSource,
        /// `None`: this Mac cannot do process taps.
        tap: Option<SharedSource>,
        /// Set: the tap's `start` blocks until the gate is opened.
        tap_gate: Option<Arc<(Mutex<bool>, std::sync::Condvar)>>,
        monitor: Arc<FakeMonitor>,
        states: Mutex<Vec<RecordingStatus>>,
        updates: Mutex<Vec<(String, MeetingChange)>>,
        titles: Mutex<Vec<Option<String>>>,
        tap_opens: AtomicUsize,
    }

    impl FakeEnv {
        fn new(mic: FakeSource, tap: Option<FakeSource>) -> Arc<Self> {
            Arc::new(Self {
                supported: true,
                mic: SharedSource::new(mic),
                tap: tap.map(SharedSource::new),
                tap_gate: None,
                monitor: Arc::default(),
                states: Mutex::default(),
                updates: Mutex::default(),
                titles: Mutex::default(),
                tap_opens: AtomicUsize::new(0),
            })
        }

        fn working() -> Arc<Self> {
            Self::new(
                FakeSource::new(TrackKind::Mic, MIC_FORMAT),
                Some(FakeSource::new(TrackKind::System, TAP_FORMAT)),
            )
        }

        /// The phases announced so far, without repeats.
        fn phases(&self) -> Vec<RecordingPhase> {
            let mut phases: Vec<RecordingPhase> =
                self.states.lock().unwrap().iter().map(|s| s.phase).collect();
            phases.dedup();
            phases
        }

        fn last_state(&self) -> RecordingStatus {
            self.states.lock().unwrap().last().cloned().expect("a state was announced")
        }

        fn open_tap_gate(&self) {
            let (open, signal) = &**self.tap_gate.as_ref().expect("the tap is gated");
            *open.lock().unwrap() = true;
            signal.notify_all();
        }

        fn changes(&self) -> Vec<MeetingChange> {
            self.updates.lock().unwrap().iter().map(|(_, change)| *change).collect()
        }
    }

    impl SessionEnv for FakeEnv {
        fn support(&self) -> Result<(), String> {
            if self.supported { Ok(()) } else { Err("macOS 14.4 or later is needed".to_string()) }
        }
        fn host_now_ns(&self) -> u64 {
            ORIGIN_NS
        }
        fn echo_risk(&self) -> bool {
            false
        }
        fn open_mic(&self) -> Box<dyn AudioSource> {
            Box::new(self.mic.clone())
        }
        fn open_system_tap(&self) -> Result<OpenedTap, String> {
            self.tap_opens.fetch_add(1, Ordering::Relaxed);
            let tap = self.tap.clone().ok_or_else(|| "process taps are not supported".to_string())?;
            let source: Box<dyn AudioSource> = match &self.tap_gate {
                Some(gate) => Box::new(GatedSource { inner: tap, gate: gate.clone() }),
                None => Box::new(tap),
            };
            Ok(OpenedTap { source, monitor: Some(Box::new(self.monitor.clone())) })
        }
        fn state_changed(&self, status: &RecordingStatus) {
            self.states.lock().unwrap().push(status.clone());
        }
        fn meeting_updated(&self, meeting_id: &str, change: MeetingChange) {
            self.updates.lock().unwrap().push((meeting_id.to_string(), change));
        }
        fn set_tray_title(&self, title: Option<String>) {
            self.titles.lock().unwrap().push(title);
        }
    }

    struct Fixture {
        db: TestDb,
        root: TempDir,
        env: Arc<FakeEnv>,
        session: Arc<Session>,
    }

    impl Fixture {
        fn new(env: Arc<FakeEnv>) -> Self {
            let db = TestDb::new();
            let root = TempDir::new("meeting-session");
            let config = SessionConfig {
                command_wait: Duration::from_secs(5),
                tap_grace: Duration::from_millis(if env.tap_gate.is_some() { 50 } else { 5_000 }),
                tick: Duration::from_millis(10),
                recorder: RecorderConfig {
                    poll_interval: Duration::from_millis(2),
                    ..RecorderConfig::default()
                },
                chunk_frames: TEST_CHUNK_FRAMES,
            };
            let session = Session::new(
                env.clone(),
                Arc::new(Mutex::new(db.connect())),
                root.path().to_path_buf(),
                config,
            );
            Self { db, root, env, session }
        }

        fn start(&self) -> Result<RecordingStatus, String> {
            self.session.start(NewSession {
                title: Some("Weekly sync".to_string()),
                language: MeetingLanguage::Nl,
                model: Some("whisper-small-q5".to_string()),
            })
        }

        fn meeting(&self, meeting_id: &str) -> crate::meetings::types::MeetingDetail {
            store::get_meeting(&self.db.conn, meeting_id).unwrap().expect("the meeting exists")
        }

        fn chunks(&self, meeting_id: &str, kind: TrackKind) -> Vec<ChunkRecord> {
            let track = self.meeting(meeting_id).tracks.into_iter().find(|t| t.kind == kind);
            track.map_or_else(Vec::new, |t| store::list_chunks(&self.db.conn, &t.id).unwrap())
        }

        /// The session ends a meeting by itself on the next tick: wait for it.
        fn wait_until_idle(&self) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.session.status().phase != RecordingPhase::Idle {
                assert!(Instant::now() < deadline, "the session never went back to idle");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Every chunk row is settled and says what is in its file.
        fn assert_chunks_match_files(&self, chunks: &[ChunkRecord]) {
            for chunk in chunks {
                assert_eq!(chunk.status, ChunkStatus::Closed, "chunk {}", chunk.path);
                let bytes = std::fs::metadata(self.root.path().join(&chunk.path)).unwrap().len();
                assert_eq!(bytes, chunk.n_frames * BYTES_PER_FRAME, "chunk {}", chunk.path);
            }
        }
    }

    #[test]
    fn a_whole_meeting_leaves_rows_chunk_files_and_a_queued_job() {
        let fx = Fixture::new(FakeEnv::working());

        let status = fx.start().unwrap();
        assert_eq!(status.phase, RecordingPhase::Recording);
        assert_eq!(status.tracks, vec![TrackKind::Mic, TrackKind::System]);
        let meeting_id = status.meeting_id.clone().expect("a live meeting has an id");
        let meeting = fx.meeting(&meeting_id);
        assert_eq!(meeting.meeting.status, MeetingStatus::Recording);
        assert_eq!(meeting.meeting.title, "Weekly sync");
        assert_eq!(meeting.meeting.language, MeetingLanguage::Nl);
        assert_eq!(status.started_at.as_deref(), Some(meeting.meeting.started_at.as_str()));
        assert_eq!(meeting.tracks.len(), 2);
        assert_eq!(meeting.tracks[0].device_name.as_deref(), Some("Fake mic"));
        let labels: Vec<_> = meeting.speakers.iter().map(|s| (s.label.as_str(), s.source)).collect();
        assert_eq!(labels, vec![("Me", SpeakerSource::Track), ("Them", SpeakerSource::Track)]);

        // 1.2 s on both tracks, a pause (what arrives is dropped), 0.6 s more.
        let t0 = ORIGIN_NS + 10 * MS;
        let mic_end = fx.env.mic.deliver(1.2, t0);
        fx.env.tap.as_ref().unwrap().deliver(1.2, t0);

        let paused = fx.session.pause().unwrap();
        assert_eq!(paused.phase, RecordingPhase::Paused);
        assert_eq!(fx.meeting(&meeting_id).meeting.status, MeetingStatus::Paused);
        assert!(fx.session.pause().is_err(), "already paused");
        fx.env.mic.deliver(0.3, mic_end);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(fx.session.status().elapsed_ms, paused.elapsed_ms, "the clock stands still");
        assert!(fx.session.tray_title().unwrap().starts_with(PAUSED_MARK));

        let resumed = fx.session.resume().unwrap();
        assert_eq!(resumed.phase, RecordingPhase::Recording);
        assert_eq!(fx.meeting(&meeting_id).meeting.status, MeetingStatus::Recording);
        assert!(fx.session.tray_title().unwrap().starts_with(RECORDING_MARK));
        let t1 = t0 + 5_000 * MS;
        fx.env.mic.deliver(0.6, t1);
        fx.env.tap.as_ref().unwrap().deliver(0.6, t1);

        let stopped = fx.session.stop().unwrap();
        assert_eq!(stopped.phase, RecordingPhase::Idle);
        assert_eq!(stopped.meeting_id, None);
        assert_eq!((fx.env.mic.stops(), fx.env.tap.as_ref().unwrap().stops()), (1, 1));

        let meeting = fx.meeting(&meeting_id);
        assert_eq!(meeting.meeting.status, MeetingStatus::Queued);
        assert!(meeting.meeting.ended_at.is_some());
        assert!(meeting.meeting.has_audio);
        let job = meeting.meeting.job.expect("a transcription job is queued");
        assert_eq!((job.status, job.meeting_id.as_str()), (JobStatus::Queued, meeting_id.as_str()));
        assert_eq!(meeting.runs.len(), 1);
        assert_eq!(meeting.runs[0].model, "whisper-small-q5");

        for kind in [TrackKind::Mic, TrackKind::System] {
            let chunks = fx.chunks(&meeting_id, kind);
            fx.assert_chunks_match_files(&chunks);
            // Three half-second files before the pause, two after it.
            assert_eq!(chunks.len(), 5, "{kind:?}");
            let frames: u64 = chunks.iter().map(|c| c.n_frames).sum();
            assert!((28_000..=29_000).contains(&frames), "{kind:?}: {frames} frames for 1.8 s");
            // The pause is a gap on the timeline, not silence on disk.
            assert!(chunks[2].start_ms < 1_300 && chunks[3].start_ms >= 5_000, "{kind:?}");
        }

        assert_eq!(
            fx.env.phases(),
            vec![
                RecordingPhase::Starting,
                RecordingPhase::Recording,
                RecordingPhase::Paused,
                RecordingPhase::Recording,
                RecordingPhase::Stopping,
                RecordingPhase::Idle,
            ]
        );
        assert_eq!(fx.env.changes(), vec![MeetingChange::Created, MeetingChange::Status]);
        assert_eq!(fx.env.titles.lock().unwrap().last(), Some(&None), "no title is left behind");
        assert_eq!(fx.session.tray_title(), None);
        assert!(fx.session.stop().is_err(), "nothing to stop");
    }

    #[test]
    fn a_tap_that_cannot_start_means_a_microphone_only_meeting() {
        for tap in [Some(FakeSource::failing(TrackKind::System, "no permission")), None] {
            let fx = Fixture::new(FakeEnv::new(FakeSource::new(TrackKind::Mic, MIC_FORMAT), tap));
            let status = fx.start().expect("the meeting is never blocked");
            assert_eq!(status.phase, RecordingPhase::Recording);
            assert_eq!(status.tracks, vec![TrackKind::Mic]);
            assert_eq!(fx.env.tap_opens.load(Ordering::Relaxed), 1);
            let meeting_id = status.meeting_id.unwrap();

            let meeting = fx.meeting(&meeting_id);
            let kinds: Vec<_> = meeting.tracks.iter().map(|t| t.kind).collect();
            assert_eq!(kinds, vec![TrackKind::Mic]);
            let labels: Vec<_> = meeting.speakers.iter().map(|s| s.label.as_str()).collect();
            assert_eq!(labels, vec!["Me"]);
            let system_dir = recording::track_dir(fx.root.path(), &meeting_id, TrackKind::System).unwrap();
            assert!(!system_dir.exists());

            fx.env.mic.deliver(0.3, ORIGIN_NS + 10 * MS);
            assert_eq!(fx.session.stop().unwrap().phase, RecordingPhase::Idle);
            let meeting = fx.meeting(&meeting_id);
            assert_eq!(meeting.meeting.status, MeetingStatus::Queued);
            assert!(meeting.meeting.job.is_some());
        }
    }

    fn gated_env() -> Arc<FakeEnv> {
        let mut env = FakeEnv::working();
        Arc::get_mut(&mut env).unwrap().tap_gate = Some(Arc::default());
        env
    }

    fn wait_until(what: &str, check: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !check() {
            assert!(Instant::now() < deadline, "never happened: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_tap_stuck_behind_the_permission_prompt_joins_when_it_gets_there() {
        let fx = Fixture::new(gated_env());
        let started = Instant::now();
        let status = fx.start().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2), "start does not wait for the prompt");
        assert_eq!((status.phase, status.tracks.clone()), (RecordingPhase::Recording, vec![TrackKind::Mic]));
        let meeting_id = status.meeting_id.unwrap();

        // The controls work while the tap is stuck, and it joins paused.
        let t0 = ORIGIN_NS + 10 * MS;
        fx.env.mic.deliver(0.3, t0);
        assert_eq!(fx.session.pause().unwrap().phase, RecordingPhase::Paused);
        fx.env.open_tap_gate();
        wait_until("the system track joins", || fx.session.status().tracks.len() == 2);
        fx.env.tap.as_ref().unwrap().deliver(0.3, t0);
        assert_eq!(fx.session.resume().unwrap().tracks, vec![TrackKind::Mic, TrackKind::System]);
        fx.env.tap.as_ref().unwrap().deliver(0.3, t0 + 2_000 * MS);
        fx.session.stop().unwrap();

        let system = fx.chunks(&meeting_id, TrackKind::System);
        fx.assert_chunks_match_files(&system);
        assert_eq!(system.len(), 1, "what arrived during the pause was dropped");
        assert!(system[0].start_ms >= 2_000);
        assert_eq!(fx.meeting(&meeting_id).meeting.status, MeetingStatus::Queued);
    }

    #[test]
    fn a_tap_that_comes_up_after_the_meeting_is_stopped_again() {
        let fx = Fixture::new(gated_env());
        let meeting_id = fx.start().unwrap().meeting_id.unwrap();
        fx.env.mic.deliver(0.3, ORIGIN_NS + 10 * MS);
        let stopping = Instant::now();
        assert_eq!(fx.session.stop().unwrap().phase, RecordingPhase::Idle);
        assert!(stopping.elapsed() < Duration::from_secs(2), "stop does not wait for the prompt");
        assert_eq!(fx.meeting(&meeting_id).meeting.status, MeetingStatus::Queued);

        fx.env.open_tap_gate();
        let tap = fx.env.tap.as_ref().unwrap();
        wait_until("the late tap is stopped", || tap.stops() == 1);
        wait_until("the unused system track is removed", || fx.meeting(&meeting_id).tracks.len() == 1);
        assert_eq!(fx.session.status().phase, RecordingPhase::Idle);
    }

    #[test]
    fn the_system_audio_notice_and_the_echo_risk_reach_the_ui() {
        let fx = Fixture::new(FakeEnv::working());
        let meeting_id = fx.start().unwrap().meeting_id.unwrap();
        assert!(!fx.session.status().system_audio_silent);

        let wait_for = |what: &str, check: &dyn Fn(&RecordingStatus) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !fx.env.states.lock().unwrap().last().is_some_and(check) {
                assert!(Instant::now() < deadline, "never announced: {what}");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        fx.env.monitor.notice.store(true, Ordering::Relaxed);
        wait_for("no system audio", &|s| s.system_audio_silent);
        fx.env.monitor.echo_risk.store(true, Ordering::Relaxed);
        wait_for("echo risk", &|s| s.echo_risk);
        assert!(fx.meeting(&meeting_id).meeting.echo_risk);
        fx.env.monitor.notice.store(false, Ordering::Relaxed);
        wait_for("system audio is back", &|s| !s.system_audio_silent && s.echo_risk);

        fx.session.stop().unwrap();
        assert!(!fx.env.last_state().echo_risk, "idle carries nothing over");
    }

    #[test]
    fn a_track_that_fails_mid_recording_ends_the_meeting_in_a_recoverable_state() {
        let fx = Fixture::new(FakeEnv::working());
        let meeting_id = fx.start().unwrap().meeting_id.unwrap();
        let t0 = ORIGIN_NS + 10 * MS;
        let next = fx.env.mic.deliver(0.7, t0);
        std::thread::sleep(Duration::from_millis(50));

        // The disk goes away under the writer: the next chunk cannot be made.
        let mic_dir = recording::track_dir(fx.root.path(), &meeting_id, TrackKind::Mic).unwrap();
        std::fs::remove_dir_all(&mic_dir).unwrap();
        fx.env.mic.deliver(0.7, next);
        fx.wait_until_idle();

        let meeting = fx.meeting(&meeting_id);
        assert_eq!(meeting.meeting.status, MeetingStatus::Failed);
        let error = meeting.error.expect("the UI has something to show");
        assert!(error.starts_with("Recording stopped early:"), "{error}");
        assert!(meeting.meeting.ended_at.is_some());
        assert!(meeting.meeting.job.is_none());
        assert!(store::list_open_chunks(&fx.db.conn).unwrap().is_empty(), "no chunk is left open");
        assert_eq!((fx.env.mic.stops(), fx.env.tap.as_ref().unwrap().stops()), (1, 1));
        assert_eq!(fx.env.last_state().phase, RecordingPhase::Idle);
        assert_eq!(fx.session.tray_title(), None);

        // Not stuck: the next meeting records.
        let again = fx.start().unwrap();
        assert_eq!(again.phase, RecordingPhase::Recording);
        assert_ne!(again.meeting_id.as_deref(), Some(meeting_id.as_str()));
        fx.session.stop().unwrap();
    }

    #[test]
    fn a_microphone_that_cannot_start_fails_the_meeting_and_says_why() {
        let fx = Fixture::new(FakeEnv::new(
            FakeSource::failing(TrackKind::Mic, "no input device"),
            Some(FakeSource::new(TrackKind::System, TAP_FORMAT)),
        ));
        let error = fx.start().unwrap_err();
        assert_eq!(error, "The microphone could not be started: no input device");
        assert_eq!(fx.session.status().phase, RecordingPhase::Idle);
        assert_eq!(fx.env.tap_opens.load(Ordering::Relaxed), 0, "the tap never comes first");

        let meetings = store::list_meetings(&fx.db.conn).unwrap();
        assert_eq!(meetings.len(), 1);
        assert_eq!(meetings[0].status, MeetingStatus::Failed);
        assert_eq!(fx.meeting(&meetings[0].id).error.as_deref(), Some(error.as_str()));
        assert_eq!(fx.env.titles.lock().unwrap().last(), Some(&None));
    }

    #[test]
    fn a_second_start_and_an_unsupported_mac_are_refused() {
        let fx = Fixture::new(FakeEnv::working());
        fx.start().unwrap();
        let error = fx.start().unwrap_err();
        assert!(error.contains("already being recorded"), "{error}");
        assert_eq!(store::list_meetings(&fx.db.conn).unwrap().len(), 1);
        assert_eq!(fx.session.status().phase, RecordingPhase::Recording, "the first one carries on");
        fx.session.stop().unwrap();

        let mut env = FakeEnv::working();
        Arc::get_mut(&mut env).unwrap().supported = false;
        let fx = Fixture::new(env);
        let error = fx.start().unwrap_err();
        assert!(error.contains("macOS 14.4"), "{error}");
        assert!(store::list_meetings(&fx.db.conn).unwrap().is_empty());
        assert!(fx.env.states.lock().unwrap().is_empty());
    }

    #[test]
    fn a_meeting_stopped_before_any_audio_is_failed_not_queued() {
        let fx = Fixture::new(FakeEnv::working());
        let meeting_id = fx.start().unwrap().meeting_id.unwrap();
        fx.session.stop().unwrap();
        let meeting = fx.meeting(&meeting_id);
        assert_eq!(meeting.meeting.status, MeetingStatus::Failed);
        assert_eq!(meeting.error.as_deref(), Some("Nothing was recorded."));
        assert!(meeting.meeting.job.is_none());
        assert!(!meeting.meeting.has_audio, "nothing to re-transcribe or delete");
        assert!(!recording::meeting_dir(fx.root.path(), &meeting_id).unwrap().exists());
    }

    #[test]
    fn the_tray_title_shows_the_mark_and_the_recorded_time() {
        assert_eq!(tray_title_for(RecordingPhase::Idle, 5_000), None);
        assert_eq!(tray_title_for(RecordingPhase::Starting, 0).as_deref(), Some("●"));
        assert_eq!(tray_title_for(RecordingPhase::Recording, 0).as_deref(), Some("● 0:00"));
        assert_eq!(tray_title_for(RecordingPhase::Recording, 65_999).as_deref(), Some("● 1:05"));
        assert_eq!(tray_title_for(RecordingPhase::Recording, 754_000).as_deref(), Some("● 12:34"));
        assert_eq!(tray_title_for(RecordingPhase::Recording, 3_723_000).as_deref(), Some("● 1:02:03"));
        assert_eq!(tray_title_for(RecordingPhase::Paused, 754_000).as_deref(), Some("❙❙ 12:34"));
        assert_eq!(tray_title_for(RecordingPhase::Stopping, 754_000).as_deref(), Some("●"));
    }

    #[test]
    fn dictation_owns_the_tray_while_it_is_active() {
        assert!(dictation_is_active(r#"{"phase":"recording"}"#));
        assert!(dictation_is_active(r#"{"phase":"transcribing"}"#));
        assert!(dictation_is_active(r#"{"phase":"injecting"}"#));
        assert!(!dictation_is_active(r#"{"phase":"idle"}"#));
        assert!(!dictation_is_active(r#"{"phase":"error"}"#));

        let fx = Fixture::new(FakeEnv::working());
        fx.start().unwrap();
        assert!(fx.env.titles.lock().unwrap().iter().flatten().any(|t| t.starts_with("● 0:0")));

        // A dictation starts: `lib.rs` shows its dot, the meeting keeps quiet.
        fx.session.set_dictation_active(true);
        let shown = fx.env.titles.lock().unwrap().len();
        fx.session.pause().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(fx.env.titles.lock().unwrap().len(), shown, "the meeting left the title alone");
        // It ends: `lib.rs` asks for the title to fall back to, and the next
        // tick shows it again by itself.
        assert!(fx.session.tray_title().unwrap().starts_with(PAUSED_MARK));
        fx.session.set_dictation_active(false);
        std::thread::sleep(Duration::from_millis(50));
        assert!(fx.env.titles.lock().unwrap().last().unwrap().as_ref().unwrap().starts_with(PAUSED_MARK));

        // The meeting stops during a dictation: nothing to fall back to.
        fx.session.set_dictation_active(true);
        fx.session.stop().unwrap();
        assert_eq!(fx.session.tray_title(), None);
    }

    #[test]
    fn shutdown_closes_the_chunks_and_queues_the_meeting() {
        let fx = Fixture::new(FakeEnv::working());
        let meeting_id = fx.start().unwrap().meeting_id.unwrap();
        fx.env.mic.deliver(0.3, ORIGIN_NS + 10 * MS);
        fx.session.shutdown();
        assert_eq!(fx.session.status().phase, RecordingPhase::Idle);
        assert_eq!(fx.meeting(&meeting_id).meeting.status, MeetingStatus::Queued);
        fx.assert_chunks_match_files(&fx.chunks(&meeting_id, TrackKind::Mic));
        fx.session.shutdown();
    }

    /// What a crash leaves behind: a `recording` meeting whose writers never
    /// closed their last chunk.
    fn crashed_meeting(fx: &Fixture, status: MeetingStatus, seconds: f64) -> String {
        let conn = &fx.db.conn;
        let id = store::insert_meeting(
            conn,
            &store::NewMeeting {
                title: "Crashed".to_string(),
                language: MeetingLanguage::Auto,
                model: Some("whisper-small-q5".to_string()),
                origin_host_ns: Some(ORIGIN_NS),
                calendar_event_id: None,
            },
        )
        .unwrap();
        for kind in [TrackKind::Mic, TrackKind::System] {
            let track_id = store::insert_track(
                conn,
                &store::NewTrack { meeting_id: id.clone(), kind, device_name: None, format: None },
            )
            .unwrap();
            if seconds > 0.0 {
                let mut config =
                    ChunkWriterConfig::new(fx.root.path().to_path_buf(), &id, &track_id, kind, ORIGIN_NS);
                config.chunk_frames = TEST_CHUNK_FRAMES;
                let ledger = Box::new(StoreLedger { db: Arc::new(Mutex::new(fx.db.connect())) });
                let mut writer = ChunkWriter::new(config, ledger).unwrap();
                writer.begin(ORIGIN_NS).unwrap();
                writer.write(&vec![0.1; (seconds * 16_000.0) as usize]).unwrap();
                // No `finish`: the process is gone.
                std::mem::forget(writer);
            }
        }
        store::seed_track_speakers(conn, &id).unwrap();
        store::set_meeting_status(conn, &id, status, None).unwrap();
        id
    }

    #[test]
    fn launch_recovery_repairs_a_crashed_meeting_and_queues_what_survived() {
        let fx = Fixture::new(FakeEnv::working());
        let conn = &fx.db.conn;
        // 0.7 s per track: one closed half-second chunk, one left open.
        let crashed = crashed_meeting(&fx, MeetingStatus::Recording, 0.7);
        assert_eq!(store::list_open_chunks(conn).unwrap().len(), 2);
        // The system track's open chunk never reached the database.
        let system_open = fx.chunks(&crashed, TrackKind::System).pop().unwrap();
        conn.execute("DELETE FROM meeting_audio_chunks WHERE id = ?1", [&system_open.id]).unwrap();
        // A meeting that crashed before any audio, and a job that was running.
        let empty = crashed_meeting(&fx, MeetingStatus::Paused, 0.0);
        let other = new_meeting(conn, &[(30_000, &[])]);
        jobs::enqueue_transcription(conn, &other.id, &RetranscribeOptions::default()).unwrap();
        assert_eq!(store::claim_next_job(conn).unwrap().unwrap().status, JobStatus::Running);

        let recovered = recover_at_launch(conn, fx.root.path()).unwrap();
        assert_eq!(
            recovered,
            vec![
                RecoveredMeeting { meeting_id: crashed.clone(), chunks_repaired: 2, queued: true },
                RecoveredMeeting { meeting_id: empty.clone(), chunks_repaired: 0, queued: false },
            ]
        );

        assert!(store::list_open_chunks(conn).unwrap().is_empty());
        for kind in [TrackKind::Mic, TrackKind::System] {
            let chunks = fx.chunks(&crashed, kind);
            let settled: Vec<_> = chunks.iter().map(|c| (c.status, c.n_frames)).collect();
            assert_eq!(
                settled,
                vec![(ChunkStatus::Closed, 8_000), (ChunkStatus::Recovered, 3_200)],
                "{kind:?}"
            );
            assert_eq!(chunks[1].start_ms, 500);
        }
        // `interrupted`, and from there straight on to the queue.
        let meeting = fx.meeting(&crashed);
        assert_eq!(meeting.meeting.status, MeetingStatus::Queued);
        assert_eq!(meeting.meeting.duration_ms, 700);
        assert!(meeting.meeting.ended_at.is_some());
        assert_eq!(meeting.meeting.job.map(|job| job.status), Some(JobStatus::Queued));

        let meeting = fx.meeting(&empty);
        assert_eq!(meeting.meeting.status, MeetingStatus::Interrupted);
        assert!(meeting.meeting.job.is_none());
        assert_eq!(
            store::job_progress(conn, &other.id).unwrap().map(|job| job.status),
            Some(JobStatus::Queued),
            "a job that was running is queued again"
        );

        assert!(recover_at_launch(conn, fx.root.path()).unwrap().is_empty(), "nothing left to do");
    }

    /// A recorded, transcribed meeting with audio on disk.
    fn transcribed_meeting(fx: &Fixture) -> String {
        let meeting_id = fx.start().unwrap().meeting_id.unwrap();
        fx.env.mic.deliver(0.3, ORIGIN_NS + 10 * MS);
        fx.session.stop().unwrap();
        let conn = &fx.db.conn;
        let meeting = fx.meeting(&meeting_id);
        let run_id = meeting.runs[0].id.clone();
        let windows = store::insert_windows(
            conn,
            &run_id,
            &[store::NewWindow { track_id: meeting.tracks[0].id.clone(), seq: 0, start_ms: 0, end_ms: 300 }],
        )
        .unwrap();
        let segment = store::NewSegment {
            start_ms: 0,
            end_ms: 300,
            text: "Goedemorgen.".to_string(),
            lang: Some("nl".to_string()),
            no_speech_prob: None,
            avg_logprob: None,
            suppressed_reason: None,
        };
        store::complete_window(conn, &windows[0], Some("nl"), &[segment]).unwrap();
        store::set_active_run(conn, &meeting_id, &run_id).unwrap();
        meeting_id
    }

    #[test]
    fn deleting_the_audio_keeps_the_transcript() {
        let fx = Fixture::new(FakeEnv::working());
        let meeting_id = transcribed_meeting(&fx);
        let conn = &fx.db.conn;
        let error = fx.session.delete_meeting_audio(&meeting_id).unwrap_err();
        assert!(error.contains("still being transcribed"), "{error}");
        store::cancel_unfinished_jobs(conn, &meeting_id).unwrap();

        let dir = recording::meeting_dir(fx.root.path(), &meeting_id).unwrap();
        assert!(dir.exists());
        fx.session.delete_meeting_audio(&meeting_id).unwrap();
        assert!(!dir.exists());
        let meeting = fx.meeting(&meeting_id);
        assert!(!meeting.meeting.has_audio);
        assert!(fx.chunks(&meeting_id, TrackKind::Mic).iter().all(|c| c.status == ChunkStatus::Deleted));
        let segments = store::list_segments(conn, &meeting_id, None).unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "Goedemorgen.");
        assert_eq!(fx.env.changes().last(), Some(&MeetingChange::AudioDeleted));
        assert!(fx.session.delete_meeting_audio("missing").is_err());
    }

    #[test]
    fn deleting_a_meeting_removes_its_rows_and_its_files() {
        let fx = Fixture::new(FakeEnv::working());
        let meeting_id = transcribed_meeting(&fx);
        let dir = recording::meeting_dir(fx.root.path(), &meeting_id).unwrap();
        assert!(dir.join("mic").join("000000.pcm").exists());

        fx.session.delete_meeting(&meeting_id).unwrap();
        assert!(!dir.exists());
        assert!(store::get_meeting(&fx.db.conn, &meeting_id).unwrap().is_none());
        assert!(store::list_jobs(&fx.db.conn, &meeting_id).unwrap().is_empty());
        assert_eq!(fx.env.changes().last(), Some(&MeetingChange::Deleted));
        assert!(fx.session.delete_meeting(&meeting_id).is_err(), "already gone");
    }

    #[test]
    fn the_meeting_being_recorded_cannot_be_deleted_or_retranscribed() {
        let fx = Fixture::new(FakeEnv::working());
        let meeting_id = fx.start().unwrap().meeting_id.unwrap();
        fx.env.mic.deliver(0.3, ORIGIN_NS + 10 * MS);
        for result in [fx.session.delete_meeting(&meeting_id), fx.session.delete_meeting_audio(&meeting_id)] {
            assert!(result.unwrap_err().contains("still being recorded"));
        }
        let retranscribe =
            jobs::enqueue_transcription(&fx.db.conn, &meeting_id, &RetranscribeOptions::default());
        assert!(retranscribe.unwrap_err().contains("still being recorded"));
        fx.session.stop().unwrap();
        assert!(recording::meeting_dir(fx.root.path(), &meeting_id).unwrap().exists());
    }

    /// The real sources into the real recorders, through the state machine.
    struct HardwareEnv;

    impl SessionEnv for HardwareEnv {
        fn support(&self) -> Result<(), String> {
            capture::support()
        }
        fn host_now_ns(&self) -> u64 {
            capture::host_now_ns()
        }
        fn echo_risk(&self) -> bool {
            capture::output_is_builtin_speakers()
        }
        fn open_mic(&self) -> Box<dyn AudioSource> {
            Box::new(capture::open_mic())
        }
        fn open_system_tap(&self) -> Result<OpenedTap, String> {
            let tap = capture::open_system_tap()?;
            let monitor = tap.monitor();
            Ok(OpenedTap { source: Box::new(tap), monitor: Some(Box::new(monitor)) })
        }
        fn state_changed(&self, status: &RecordingStatus) {
            println!("meeting-state: {status:?}");
        }
        fn meeting_updated(&self, _meeting_id: &str, _change: MeetingChange) {}
        fn set_tray_title(&self, _title: Option<String>) {}
    }

    #[test]
    #[ignore = "needs a microphone, audio output, both permissions, and plays a sound for 10 s"]
    fn a_real_ten_second_meeting_ends_with_two_non_empty_tracks() {
        let _hardware = crate::meetings::capture::test_support::hardware_lock();
        let db = TestDb::new();
        let root = TempDir::new("meeting-session-hardware");
        let session = Session::new(
            Arc::new(HardwareEnv),
            Arc::new(Mutex::new(db.connect())),
            root.path().to_path_buf(),
            SessionConfig { command_wait: Duration::from_secs(30), ..SessionConfig::default() },
        );
        let status = session
            .start(NewSession { title: None, language: MeetingLanguage::Auto, model: Some("whisper-small-q5".into()) })
            .unwrap();
        assert_eq!(status.tracks, vec![TrackKind::Mic, TrackKind::System]);
        let meeting_id = status.meeting_id.unwrap();

        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            let _ = std::process::Command::new("afplay").arg("/System/Library/Sounds/Submarine.aiff").status();
        }
        let stopped = session.stop().unwrap();
        assert_eq!(stopped.phase, RecordingPhase::Idle);

        let meeting = store::get_meeting(&db.conn, &meeting_id).unwrap().unwrap();
        println!("meeting: {:?}, {} ms", meeting.meeting.status, meeting.meeting.duration_ms);
        assert_eq!(meeting.meeting.status, MeetingStatus::Queued);
        assert!(meeting.meeting.duration_ms >= 10_000);
        assert_eq!(meeting.tracks.len(), 2);
        for track in &meeting.tracks {
            let chunks = store::list_chunks(&db.conn, &track.id).unwrap();
            let frames: u64 = chunks.iter().map(|c| c.n_frames).sum();
            println!("{:?}: {} chunk(s), {frames} frames, {:?}", track.kind, chunks.len(), track.device_name);
            assert!(frames > 5 * u64::from(TARGET_SAMPLE_RATE), "{:?} recorded {frames} frames", track.kind);
            let loud = chunks.iter().any(|chunk| {
                let bytes = std::fs::read(root.path().join(&chunk.path)).unwrap();
                bytes.chunks_exact(2).any(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs() > 300)
            });
            assert!(loud, "the {:?} track holds only silence", track.kind);
        }
    }
}

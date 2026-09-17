//! OWNER: WP6 (jobs and worker). The one `meeting-worker` thread.
//!
//! - One thread, its own connection from `db::open_connection()` (WAL and
//!   `busy_timeout` make that safe). It never takes the managed connection's
//!   mutex.
//! - Per job: plan windows (`longform.rs`) if the run has none, all tracks in
//!   one transaction, then decode the `pending` ones in timeline order. A
//!   window is the unit of resume: its segments and its `done` state commit
//!   together, so a restart never decodes a window twice.
//! - Priority: the inference gate is held around the model call of one window
//!   (and around language detection), never around reading audio, VAD or
//!   planning, and never for a job. A decode that dictation aborts leaves the
//!   window `pending` with no attempt counted; the worker then waits for
//!   dictation to finish before it touches the window again.
//! - A window that errors is retried, up to `MAX_WINDOW_ATTEMPTS`, then it is
//!   `failed` and the job goes on: one bad window never loses a meeting.
//! - When the run is settled: the echo pass (`echo.rs`), run `done`, the run
//!   becomes the meeting's active run, the meeting `ready`. A first run is
//!   active from the start so the transcript fills in live; a re-run only
//!   replaces the old one once it is complete.
//! - Emits `meeting-job-progress` (throttled) and `meeting-updated`.
//! - Applies `settings.meetings.auto_delete_audio_days`.
//!
//! Everything outside SQLite sits behind `Host` (model, VAD, audio, events,
//! settings), so the tests drive whole jobs with fakes and no thread.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use tauri::{AppHandle, Manager};

use super::echo;
use super::longform::{
    self, PlannedWindow, SileroDetector, SpeechDetector, WhisperDecoder, WindowDecoder,
    WindowOutcome,
};
use super::recording::{self, log, ChunkTrackAudio};
use super::store::{self, JobRow, WindowRow};
use super::types::{
    JobKind, JobProgress, JobStatus, MeetingChange, MeetingStatus, MeetingTrack, RunStatus,
    TrackAudio, WindowStatus,
};
use super::{events, jobs, PersistedHandle};
use crate::inference_gate;

/// Decode attempts before a window is given up on.
pub const MAX_WINDOW_ATTEMPTS: u32 = 3;
/// Times a job may be claimed. Only a quit or a crash mid-job makes it be
/// claimed again, so this is what stops a window that takes the process down
/// from doing so at every launch.
const MAX_JOB_ATTEMPTS: u32 = 8;
/// `meeting-job-progress` at most this often: a few per second.
const PROGRESS_EVERY: Duration = Duration::from_millis(300);
/// `meeting-updated` for new segments at most this often. The UI refetches
/// the whole transcript on it.
const TRANSCRIPT_EVERY: Duration = Duration::from_secs(2);
/// How long an idle worker sleeps before it looks at the queue unasked: the
/// net under a `wake()` that came before its job was committed.
const IDLE_POLL: Duration = Duration::from_secs(60);
const RETENTION_EVERY: Duration = Duration::from_secs(3600);

/// The worker's hand on a job was taken away: cancelled, or its meeting was
/// deleted. Not a failure.
const JOB_TAKEN_AWAY: &str = "meeting job is no longer running";

// ---------------------------------------------------------------------------
// Start and wake
// ---------------------------------------------------------------------------

struct WakeSignal {
    pending: Mutex<bool>,
    signal: Condvar,
}

impl WakeSignal {
    const fn new() -> Self {
        Self { pending: Mutex::new(false), signal: Condvar::new() }
    }

    fn wake(&self) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        *pending = true;
        drop(pending);
        self.signal.notify_one();
    }

    /// Sleeps until `wake()` or `timeout`. A wake that came while the worker
    /// was busy is still pending, so it is never missed.
    fn wait(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        while !*pending {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            pending = self
                .signal
                .wait_timeout(pending, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *pending = false;
    }
}

static WAKE: WakeSignal = WakeSignal::new();
static STARTED: AtomicBool = AtomicBool::new(false);

/// Spawns the worker thread. Called once at launch, after launch recovery.
/// Requeues the jobs the last run of the app left `running` before the thread
/// takes its first one.
pub fn start(app: &AppHandle) -> Result<(), String> {
    if STARTED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let spawn = || -> Result<(), String> {
        let conn = crate::db::open_connection()?;
        let requeued = jobs::recover_at_launch(&conn)?;
        if requeued > 0 {
            log("INFO", &format!("Meetings: requeued {requeued} interrupted job(s)"));
        }
        let worker = Worker::new(conn, TauriHost { app: app.clone() });
        std::thread::Builder::new()
            .name("meeting-worker".to_string())
            .spawn(move || worker.run_forever())
            .map_err(|e| format!("Failed to spawn the meeting worker: {e}"))?;
        Ok(())
    };
    spawn().inspect_err(|_| STARTED.store(false, Ordering::SeqCst))
}

/// Tells the worker there may be a new job. Cheap, never blocks, and a no-op
/// before `start`.
pub fn wake() {
    WAKE.wake();
}

// ---------------------------------------------------------------------------
// The host: everything that is not SQLite
// ---------------------------------------------------------------------------

/// What the worker needs from the rest of the app. `TauriHost` is the real
/// one; the tests use a fake with a scripted decoder and an event log.
pub(crate) trait Host {
    /// The model a run names, ready to decode. A model meetings cannot use
    /// (missing, Parakeet) is an error that says what to do about it.
    fn decoder(&mut self, model: &str) -> Result<Box<dyn WindowDecoder>, String>;
    fn detector(&mut self) -> Result<Box<dyn SpeechDetector>, String>;
    fn track_audio(
        &mut self,
        conn: &Connection,
        track: &MeetingTrack,
    ) -> Result<Box<dyn TrackAudio>, String>;
    fn job_progress(&mut self, progress: &JobProgress);
    fn meeting_updated(&mut self, meeting_id: &str, change: MeetingChange);
    /// `settings.meetings.auto_delete_audio_days`; 0 keeps audio forever.
    fn auto_delete_audio_days(&mut self) -> u32;
    fn delete_audio_files(&mut self, meeting_id: &str) -> Result<(), String>;
    /// Tests only: stop between two windows and leave the job `running`, the
    /// way a quit or a crash would.
    fn should_stop(&mut self) -> bool {
        false
    }
}

struct TauriHost {
    app: AppHandle,
}

impl Host for TauriHost {
    fn decoder(&mut self, model: &str) -> Result<Box<dyn WindowDecoder>, String> {
        let model_id = jobs::usable_model(model)?;
        Ok(Box::new(WhisperDecoder::load(&model_id)?))
    }

    fn detector(&mut self) -> Result<Box<dyn SpeechDetector>, String> {
        Ok(Box::new(SileroDetector::installed()?))
    }

    fn track_audio(
        &mut self,
        conn: &Connection,
        track: &MeetingTrack,
    ) -> Result<Box<dyn TrackAudio>, String> {
        let chunks = store::list_chunks(conn, &track.id)?;
        Ok(Box::new(ChunkTrackAudio::new(recording::meetings_root()?, chunks)))
    }

    fn job_progress(&mut self, progress: &JobProgress) {
        events::emit_job_progress(&self.app, progress);
    }

    fn meeting_updated(&mut self, meeting_id: &str, change: MeetingChange) {
        events::emit_updated(&self.app, meeting_id, change);
    }

    fn auto_delete_audio_days(&mut self) -> u32 {
        self.app
            .try_state::<PersistedHandle>()
            .and_then(|state| {
                state.inner().lock().ok().map(|s| s.settings.meetings.auto_delete_audio_days)
            })
            .unwrap_or(0)
    }

    fn delete_audio_files(&mut self, meeting_id: &str) -> Result<(), String> {
        recording::delete_meeting_audio(meeting_id)
    }
}

/// Holds the inference gate around each call into the model, and only there.
/// Dictation that arrives mid-decode waits for one abort check; dictation
/// that is already busy makes `acquire_background` wait here, before an
/// encoder pass is started.
struct Gated<'a> {
    inner: &'a mut dyn WindowDecoder,
}

impl WindowDecoder for Gated<'_> {
    fn detect_language(&mut self, samples: &[f32]) -> Result<&'static str, String> {
        let _gate = inference_gate::acquire_background();
        self.inner.detect_language(samples)
    }

    fn decode(
        &mut self,
        samples: &[f32],
        language: &str,
        prompt: Option<&str>,
    ) -> Result<longform::DecodeResult, String> {
        let _gate = inference_gate::acquire_background();
        self.inner.decode(samples, language, prompt)
    }
}

/// Reading audio and VAD need no gate, but they do use the CPU dictation is
/// waiting for. Blocks while an interactive caller waits for or holds the
/// gate.
fn yield_to_dictation() {
    if inference_gate::should_preempt() {
        drop(inference_gate::acquire_background());
    }
}

// ---------------------------------------------------------------------------
// The worker
// ---------------------------------------------------------------------------

/// "At most this often", with the first one always through.
struct Throttle {
    every: Duration,
    last: Option<Instant>,
}

impl Throttle {
    fn new(every: Duration) -> Self {
        Self { every, last: None }
    }

    fn ready(&mut self, now: Instant) -> bool {
        if self.last.is_some_and(|last| now.duration_since(last) < self.every) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// How a job left the worker's hands without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobEnd {
    Finished,
    /// Cancelled or deleted under the worker. Nothing more to write.
    TakenAway,
    /// `Host::should_stop`: left `running`, as a quit would leave it.
    Stopped,
}

pub(crate) struct Worker<H: Host> {
    conn: Connection,
    host: H,
    progress_every: Duration,
    transcript_every: Duration,
}

impl<H: Host> Worker<H> {
    pub(crate) fn new(conn: Connection, host: H) -> Self {
        Self { conn, host, progress_every: PROGRESS_EVERY, transcript_every: TRANSCRIPT_EVERY }
    }

    fn run_forever(mut self) {
        let mut last_retention: Option<Instant> = None;
        loop {
            let handled = self.drain();
            if handled > 0 || last_retention.is_none_or(|at| at.elapsed() >= RETENTION_EVERY) {
                self.apply_audio_retention(Utc::now());
                last_retention = Some(Instant::now());
            }
            WAKE.wait(IDLE_POLL);
        }
    }

    /// Runs queued jobs until the queue is empty. Returns how many it took.
    pub(crate) fn drain(&mut self) -> usize {
        let mut handled = 0;
        while !self.host.should_stop() {
            let job = match store::claim_next_job(&self.conn) {
                Ok(Some(job)) => job,
                Ok(None) => break,
                Err(e) => {
                    log("ERROR", &format!("Meetings: could not claim a job: {e}"));
                    break;
                }
            };
            handled += 1;
            self.run_job(&job);
        }
        handled
    }

    fn run_job(&mut self, job: &JobRow) {
        log(
            "INFO",
            &format!("Meetings: job {} ({}) started, attempt {}", job.id, job.kind.as_str(), job.attempts),
        );
        let result = catch_unwind(AssertUnwindSafe(|| match job.kind {
            // A new kind (WP10's summary) gets its arm here.
            JobKind::Transcribe => self.transcribe(job),
        }))
        .unwrap_or_else(|_| {
            // Whatever the panic interrupted must not stay half-written.
            if !self.conn.is_autocommit() {
                let _ = self.conn.execute_batch("ROLLBACK");
            }
            Err("Meeting transcription stopped on an internal error.".to_string())
        });
        let result = match result {
            Err(error) if error == JOB_TAKEN_AWAY => Ok(JobEnd::TakenAway),
            other => other,
        };
        match result {
            Ok(end) => {
                log("INFO", &format!("Meetings: job {} ended: {end:?}", job.id));
                if end == JobEnd::TakenAway {
                    self.cancel_run(job);
                }
            }
            Err(error) => self.fail_job(job, &error),
        }
    }

    /// The run of a job that was cancelled is over too. Best effort: when the
    /// meeting was deleted the run went with it.
    fn cancel_run(&self, job: &JobRow) {
        let Some(run_id) = job.run_id.as_deref() else {
            return;
        };
        if let Ok(Some(run)) = store::get_run(&self.conn, run_id) {
            if matches!(run.status, RunStatus::Queued | RunStatus::Running) {
                let _ = store::set_run_status(&self.conn, run_id, RunStatus::Cancelled, None);
            }
        }
    }

    /// The job cannot go on. The run and the job are `failed` with the
    /// reason; so is the meeting, unless it still shows an older, finished
    /// run, which this failure takes nothing away from.
    fn fail_job(&mut self, job: &JobRow, error: &str) {
        log("ERROR", &format!("Meetings: job {} failed: {error}", job.id));
        let conn = &self.conn;
        let settled = store::transaction(conn, || {
            if !store::fail_job(conn, &job.id, error)? {
                return Ok(None);
            }
            if let Some(run_id) = job.run_id.as_deref() {
                if store::get_run(conn, run_id)?.is_some() {
                    store::set_run_status(conn, run_id, RunStatus::Failed, Some(error))?;
                }
            }
            let meeting_failed = match store::get_meeting(conn, &job.meeting_id)? {
                Some(meeting) if !jobs::shows_finished_run(&meeting) => {
                    store::set_meeting_status(conn, &job.meeting_id, MeetingStatus::Failed, Some(error))?;
                    true
                }
                _ => false,
            };
            Ok(Some(meeting_failed))
        });
        match settled {
            Ok(Some(meeting_failed)) => {
                let mut progress = job.progress();
                if let Ok(Some(row)) = store::get_job(&self.conn, &job.id) {
                    progress = row.progress();
                }
                self.host.job_progress(&progress);
                if meeting_failed {
                    self.host.meeting_updated(&job.meeting_id, MeetingChange::Status);
                }
            }
            Ok(None) => {}
            Err(e) => log("ERROR", &format!("Meetings: could not record the failure of job {}: {e}", job.id)),
        }
    }

    fn transcribe(&mut self, job: &JobRow) -> Result<JobEnd, String> {
        let conn = &self.conn;
        let host = &mut self.host;
        let run_id = job
            .run_id
            .as_deref()
            .ok_or_else(|| "The transcription job has no run.".to_string())?;
        if job.attempts > MAX_JOB_ATTEMPTS {
            return Err(format!(
                "Transcription was interrupted {MAX_JOB_ATTEMPTS} times and was given up on. Try transcribing the meeting again."
            ));
        }
        let run = store::get_run(conn, run_id)?
            .ok_or_else(|| format!("Run '{run_id}' not found"))?;
        let Some(meeting) = store::get_meeting(conn, &job.meeting_id)? else {
            return Ok(JobEnd::TakenAway);
        };
        let meeting_id = meeting.meeting.id.as_str();

        // A first run shows its segments as they land. A re-run stays in the
        // background until it is complete: the old run is what the user sees.
        let takes_over = !jobs::shows_finished_run(&meeting);
        store::transaction(conn, || {
            store::set_run_status(conn, run_id, RunStatus::Running, None)?;
            if takes_over {
                store::set_active_run(conn, meeting_id, run_id)?;
                store::set_meeting_status(conn, meeting_id, MeetingStatus::Transcribing, None)?;
            }
            Ok(())
        })?;
        if takes_over {
            host.meeting_updated(meeting_id, MeetingChange::Status);
        }
        let mut progress = job.progress();
        host.job_progress(&progress);
        let mut progress_pace = Throttle::new(self.progress_every);
        progress_pace.ready(Instant::now());
        let mut transcript_pace = Throttle::new(self.transcript_every);
        let mut transcript_unannounced = false;

        let tracks: Vec<&MeetingTrack> = meeting.tracks.iter().filter(|t| t.has_audio).collect();
        if tracks.is_empty() {
            return Err("The audio of this meeting was deleted, so it cannot be transcribed.".to_string());
        }
        // Before any planning: a model that cannot work fails the job now.
        let mut decoder = host.decoder(&run.model)?;
        let mut decoder = Gated { inner: decoder.as_mut() };
        let mut detector = host.detector()?;
        let mut audio: HashMap<&str, Box<dyn TrackAudio>> = HashMap::new();
        for track in &tracks {
            audio.insert(track.id.as_str(), host.track_audio(conn, track)?);
        }

        // Phase 1: the plan. Every track's windows go in together, so a run
        // has its whole plan or none and planning is never half-resumed.
        if store::window_counts(conn, run_id)?.total == 0 {
            let mut plan = Vec::new();
            for track in &tracks {
                let track_audio = audio.get_mut(track.id.as_str()).expect("opened above");
                let windows = longform::plan_windows(track_audio.as_mut(), detector.as_mut())?;
                plan.extend(windows.into_iter().map(|w| store::NewWindow {
                    track_id: track.id.clone(),
                    seq: w.seq,
                    start_ms: w.start_ms,
                    end_ms: w.end_ms,
                }));
                if !store::heartbeat_job(conn, &job.id)? {
                    return Ok(JobEnd::TakenAway);
                }
            }
            store::insert_windows(conn, run_id, &plan)?;
        }
        let planned = store::list_windows(conn, run_id)?;
        let mut counts = store::window_counts(conn, run_id)?;
        progress.done = counts.done + counts.failed;
        progress.total = counts.total;
        if !store::set_job_progress(conn, &job.id, progress.done, progress.total)? {
            return Ok(JobEnd::TakenAway);
        }
        host.job_progress(&progress);

        // Phase 2: the pending windows, in timeline order.
        let participants: Vec<String> = store::list_participants(conn, meeting_id)?
            .into_iter()
            .filter_map(|p| p.name)
            .collect();
        let prompt = longform::build_initial_prompt(&meeting.meeting.title, &participants);
        let mut languages: HashMap<String, &'static str> = HashMap::new();
        let mut last_error: Option<String> = None;
        loop {
            if host.should_stop() {
                return Ok(JobEnd::Stopped);
            }
            yield_to_dictation();
            let Some(window) = store::next_pending_window(conn, run_id)? else {
                break;
            };
            let outcome = (|| -> Result<Option<usize>, String> {
                let track_audio = audio
                    .get_mut(window.track_id.as_str())
                    .ok_or_else(|| "The audio of this track is gone.".to_string())?;
                let language = match languages.get(&window.track_id) {
                    Some(language) => *language,
                    None => {
                        let known = store::track_language(conn, run_id, &window.track_id)?;
                        let track_windows: Vec<PlannedWindow> = planned
                            .iter()
                            .filter(|w| w.track_id == window.track_id)
                            .map(planned_window)
                            .collect();
                        let language = longform::track_language(
                            run.language,
                            known.as_deref(),
                            track_audio.as_mut(),
                            &track_windows,
                            &mut decoder,
                        )?;
                        languages.insert(window.track_id.clone(), language);
                        language
                    }
                };
                let decoded = longform::decode_window(
                    track_audio.as_mut(),
                    &planned_window(&window),
                    language,
                    prompt.as_deref(),
                    detector.as_mut(),
                    &mut decoder,
                )?;
                match decoded {
                    // Stays `pending`, no attempt counted: dictation asked
                    // for the model, the window did nothing wrong.
                    WindowOutcome::Preempted => Ok(None),
                    WindowOutcome::Done(segments) => {
                        let segments: Vec<store::NewSegment> =
                            segments.into_iter().map(new_segment).collect();
                        store::complete_window(conn, &window.id, Some(language), &segments)?;
                        Ok(Some(segments.len()))
                    }
                }
            })();

            match outcome {
                Ok(None) => continue,
                Ok(Some(n_segments)) => transcript_unannounced |= n_segments > 0,
                Err(error) => {
                    // A window that vanished with its meeting is not a
                    // failure of the window.
                    if !store::heartbeat_job(conn, &job.id)? {
                        return Ok(JobEnd::TakenAway);
                    }
                    let status = store::fail_window(conn, &window.id, &error, MAX_WINDOW_ATTEMPTS)?;
                    log(
                        "WARN",
                        &format!(
                            "Meetings: {} window {} of job {} failed ({}): {error}",
                            window.track_kind.as_str(),
                            window.seq,
                            job.id,
                            if status == WindowStatus::Failed { "gave up" } else { "will retry" },
                        ),
                    );
                    last_error = Some(error);
                    if status != WindowStatus::Failed {
                        continue;
                    }
                }
            }

            counts = store::window_counts(conn, run_id)?;
            progress.done = counts.done + counts.failed;
            progress.total = counts.total;
            if !store::set_job_progress(conn, &job.id, progress.done, progress.total)? {
                return Ok(JobEnd::TakenAway);
            }
            let now = Instant::now();
            if progress_pace.ready(now) {
                host.job_progress(&progress);
            }
            if takes_over && transcript_unannounced && transcript_pace.ready(now) {
                host.meeting_updated(meeting_id, MeetingChange::Transcript);
                transcript_unannounced = false;
            }
        }

        // Phase 3: settle. "Done with failures" is `done` plus a note; only
        // a run that produced nothing at all is a failure.
        counts = store::window_counts(conn, run_id)?;
        if counts.total > 0 && counts.done == 0 {
            let reason = last_error
                .or_else(|| planned_error(conn, run_id))
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(format!(
                "None of the {} parts of this meeting could be transcribed: {reason}",
                counts.total
            ));
        }
        let note = (counts.failed > 0).then(|| {
            format!("{} of {} parts could not be transcribed.", counts.failed, counts.total)
        });
        let mut echoes_flagged = 0;
        store::transaction(conn, || {
            echoes_flagged = echo::flag_run_echoes(conn, meeting_id, run_id)?.flagged;
            store::set_run_status(conn, run_id, RunStatus::Done, note.as_deref())?;
            store::set_active_run(conn, meeting_id, run_id)?;
            store::set_meeting_status(conn, meeting_id, MeetingStatus::Ready, None)?;
            if !store::finish_job(conn, &job.id)? {
                return Err(JOB_TAKEN_AWAY.to_string());
            }
            Ok(())
        })?;
        progress.status = JobStatus::Done;
        host.job_progress(&progress);
        if !takes_over || transcript_unannounced || echoes_flagged > 0 {
            host.meeting_updated(meeting_id, MeetingChange::Transcript);
        }
        host.meeting_updated(meeting_id, MeetingChange::Status);
        Ok(JobEnd::Finished)
    }

    /// Deletes the audio of meetings whose transcript has been finished for
    /// `auto_delete_audio_days`. Only `ready` meetings without an unfinished
    /// job and without failed windows: audio that may still be needed is
    /// never touched. Returns the
    /// meetings whose audio went.
    pub(crate) fn apply_audio_retention(&mut self, now: DateTime<Utc>) -> Vec<String> {
        let days = self.host.auto_delete_audio_days();
        if days == 0 {
            return Vec::new();
        }
        let ready = match store::list_meetings_with_status(&self.conn, &[MeetingStatus::Ready]) {
            Ok(ready) => ready,
            Err(e) => {
                log("ERROR", &format!("Meetings: audio retention could not list meetings: {e}"));
                return Vec::new();
            }
        };
        let mut deleted = Vec::new();
        for item in ready.iter().filter(|m| m.has_audio && m.job.is_none()) {
            let result = (|| -> Result<bool, String> {
                let Some(meeting) = store::get_meeting(&self.conn, &item.id)? else {
                    return Ok(false);
                };
                let active_run = meeting
                    .runs
                    .iter()
                    .find(|run| Some(&run.id) == meeting.active_run_id.as_ref())
                    .filter(|run| run.status == RunStatus::Done);
                let transcribed_at = active_run
                    .and_then(|run| run.finished_at.as_deref())
                    .and_then(|at| DateTime::parse_from_rfc3339(at).ok());
                let (Some(run), Some(transcribed_at)) = (active_run, transcribed_at) else {
                    return Ok(false);
                };
                // Parts that could not be transcribed can only be tried again
                // from the audio: such a meeting keeps it.
                if store::window_counts(&self.conn, &run.id)?.failed > 0 {
                    return Ok(false);
                }
                if now.signed_duration_since(transcribed_at) < chrono::Duration::days(days.into()) {
                    return Ok(false);
                }
                // Files first: if marking fails, the next pass finds the
                // meeting again and deleting a missing directory is fine.
                self.host.delete_audio_files(&item.id)?;
                store::mark_audio_deleted(&self.conn, &item.id)?;
                Ok(true)
            })();
            match result {
                Ok(true) => {
                    log("INFO", &format!("Meetings: deleted the audio of meeting {} after {days} day(s)", item.id));
                    self.host.meeting_updated(&item.id, MeetingChange::AudioDeleted);
                    deleted.push(item.id.clone());
                }
                Ok(false) => {}
                Err(e) => log("ERROR", &format!("Meetings: audio retention failed for meeting {}: {e}", item.id)),
            }
        }
        deleted
    }
}

fn planned_window(window: &WindowRow) -> PlannedWindow {
    PlannedWindow { seq: window.seq, start_ms: window.start_ms, end_ms: window.end_ms }
}

fn new_segment(segment: longform::WindowSegment) -> store::NewSegment {
    store::NewSegment {
        start_ms: segment.start_ms,
        end_ms: segment.end_ms,
        text: segment.text,
        lang: Some(segment.lang),
        no_speech_prob: Some(segment.no_speech_prob),
        avg_logprob: Some(segment.avg_logprob),
        suppressed_reason: segment.suppressed_reason,
    }
}

/// The error of a window an earlier launch gave up on.
fn planned_error(conn: &Connection, run_id: &str) -> Option<String> {
    store::list_windows(conn, run_id).ok()?.into_iter().find_map(|w| w.error)
}

/// The echo pass: mic segments that repeat what the system track said. Only
/// segments nothing else flagged are compared, and only when both tracks
/// have some. `echo.rs` decides; an empty answer (also its stub's) is a skip.

// ---------------------------------------------------------------------------
// Test support, shared with `jobs.rs`
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    use rusqlite::Connection;

    use crate::meetings::recording::test_support::TempDir;
    use crate::meetings::recording::chunk_rel_path;
    use crate::meetings::store;
    use crate::meetings::types::{
        ChunkRecord, ChunkStatus, MeetingLanguage, MeetingStatus, TrackKind, TARGET_SAMPLE_RATE,
    };

    /// A temp-file database at the current schema. A file, not `:memory:`, so
    /// a test can open a second connection the way the app has two, and
    /// reopen it to play a restart.
    pub(crate) struct TestDb {
        pub conn: Connection,
        path: PathBuf,
        _dir: TempDir,
    }

    impl TestDb {
        pub fn new() -> Self {
            let dir = TempDir::new("meeting-worker");
            let path = dir.path().join("test.sqlite");
            let conn = open(&path);
            migrate(&conn);
            Self { conn, path, _dir: dir }
        }

        pub fn connect(&self) -> Connection {
            open(&self.path)
        }
    }

    fn open(path: &std::path::Path) -> Connection {
        let conn = Connection::open(path).expect("open test db");
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        conn
    }

    /// `db.rs` keeps its migrations private, so the schema is built from the
    /// migration SQL in its source, like the store's tests do.
    fn migrate(conn: &Connection) {
        let source = include_str!("../db.rs");
        let mut rest = &source[..source.find("#[cfg(test)]").expect("db.rs test module")];
        while let Some(start) = rest.find("\"BEGIN;") {
            let batch = &rest[start + 1..];
            let end = batch.find("COMMIT;\"").expect("end of migration batch") + "COMMIT;".len();
            conn.execute_batch(&batch[..end]).expect("run migration batch");
            rest = &batch[end..];
        }
    }

    pub(crate) struct TestTrack {
        pub id: String,
        pub duration_ms: u64,
        pub speech: Vec<(u64, u64)>,
    }

    pub(crate) struct TestMeeting {
        pub id: String,
        pub tracks: Vec<TestTrack>,
    }

    /// A recorded meeting, `queued` as the session leaves it at stop. One
    /// track per entry, `(duration_ms, speech ranges)`: mic first, then
    /// system. Each has one closed chunk row; the audio itself comes from the
    /// fake host.
    pub(crate) fn new_meeting(conn: &Connection, tracks: &[(u64, &[(u64, u64)])]) -> TestMeeting {
        let id = store::insert_meeting(
            conn,
            &store::NewMeeting {
                title: "Weekly sync".to_string(),
                language: MeetingLanguage::Auto,
                model: Some("whisper-small-q5".to_string()),
                origin_host_ns: Some(1_000),
                calendar_event_id: None,
            },
        )
        .unwrap();
        let kinds = [TrackKind::Mic, TrackKind::System];
        let tracks = tracks
            .iter()
            .zip(kinds)
            .map(|(&(duration_ms, speech), kind)| {
                let track_id = store::insert_track(
                    conn,
                    &store::NewTrack { meeting_id: id.clone(), kind, device_name: None, format: None },
                )
                .unwrap();
                store::insert_chunk(
                    conn,
                    &ChunkRecord {
                        id: uuid::Uuid::new_v4().to_string(),
                        track_id: track_id.clone(),
                        seq: 0,
                        path: chunk_rel_path(&id, kind, 0),
                        status: ChunkStatus::Closed,
                        anchor_host_ns: 1_000,
                        start_ms: 0,
                        n_frames: duration_ms * u64::from(TARGET_SAMPLE_RATE) / 1_000,
                    },
                )
                .unwrap();
                TestTrack { id: track_id, duration_ms, speech: speech.to_vec() }
            })
            .collect();
        store::seed_track_speakers(conn, &id).unwrap();
        store::set_meeting_status(conn, &id, MeetingStatus::Queued, None).unwrap();
        TestMeeting { id, tracks }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use super::test_support::{new_meeting, TestDb, TestMeeting};
    use super::*;
    use crate::meetings::types::TrackKind;
    use crate::local_transcribe::TranscriptSegment;
    use crate::meetings::longform::{DecodeResult, LevelDetector, MemoryTrackAudio};
    use crate::meetings::types::{MeetingLanguage, RetranscribeOptions};

    /// Three stretches of speech more than 3 s apart: three windows.
    const MIC_SPEECH: &[(u64, u64)] = &[(1_000, 2_000), (10_000, 11_000), (20_000, 21_000)];
    const SYSTEM_SPEECH: &[(u64, u64)] = &[(5_000, 6_000), (15_000, 16_000)];
    const TRACK_MS: u64 = 30_000;

    #[derive(Debug, Clone, PartialEq)]
    enum Event {
        Progress(JobStatus, u32, u32),
        Updated(String, MeetingChange),
    }

    enum Step {
        Preempt,
        Fail(&'static str),
    }

    /// What the fake decoders of one host do and saw. Decode calls are
    /// numbered from 0 across the host's life.
    #[derive(Default)]
    struct Script {
        steps: HashMap<usize, Step>,
        /// `(samples, language)` of every decode call.
        decodes: Vec<(usize, String)>,
        detections: usize,
        before_decode: Option<Box<dyn FnMut(usize)>>,
    }

    struct FakeDecoder(Rc<RefCell<Script>>);

    impl WindowDecoder for FakeDecoder {
        fn detect_language(&mut self, _samples: &[f32]) -> Result<&'static str, String> {
            self.0.borrow_mut().detections += 1;
            Ok("nl")
        }

        fn decode(
            &mut self,
            samples: &[f32],
            language: &str,
            _prompt: Option<&str>,
        ) -> Result<DecodeResult, String> {
            let call = self.0.borrow().decodes.len();
            let hook = self.0.borrow_mut().before_decode.take();
            if let Some(mut hook) = hook {
                hook(call);
                self.0.borrow_mut().before_decode = Some(hook);
            }
            let mut script = self.0.borrow_mut();
            script.decodes.push((samples.len(), language.to_string()));
            match script.steps.get(&call) {
                Some(Step::Preempt) => Ok(DecodeResult::Preempted),
                Some(Step::Fail(message)) => Err(message.to_string()),
                None => Ok(DecodeResult::Segments(vec![TranscriptSegment {
                    start_ms: 0,
                    end_ms: samples.len() as u64 / 16,
                    text: format!(" Decoded in call {call}."),
                    no_speech_prob: 0.01,
                    avg_logprob: -0.2,
                    lang: language.to_string(),
                }])),
            }
        }
    }

    #[derive(Default)]
    struct FakeHost {
        /// Track id to `(duration_ms, speech)`.
        audio: HashMap<String, (u64, Vec<(u64, u64)>)>,
        script: Rc<RefCell<Script>>,
        events: Vec<Event>,
        decoder_error: Option<String>,
        /// Stop (as a quit would) once this many decode calls were made.
        stop_after_decodes: Option<usize>,
        retention_days: u32,
        deleted_audio: Vec<String>,
    }

    impl FakeHost {
        fn for_meetings(meetings: &[&TestMeeting]) -> Self {
            let mut host = Self::default();
            for track in meetings.iter().flat_map(|m| &m.tracks) {
                host.audio.insert(track.id.clone(), (track.duration_ms, track.speech.clone()));
            }
            host
        }
    }

    impl Host for FakeHost {
        fn decoder(&mut self, _model: &str) -> Result<Box<dyn WindowDecoder>, String> {
            match &self.decoder_error {
                Some(error) => Err(error.clone()),
                None => Ok(Box::new(FakeDecoder(self.script.clone()))),
            }
        }

        fn detector(&mut self) -> Result<Box<dyn SpeechDetector>, String> {
            Ok(Box::new(LevelDetector))
        }

        fn track_audio(
            &mut self,
            _conn: &Connection,
            track: &MeetingTrack,
        ) -> Result<Box<dyn TrackAudio>, String> {
            let (duration_ms, speech) =
                self.audio.get(&track.id).ok_or_else(|| "no such track".to_string())?;
            Ok(Box::new(MemoryTrackAudio::with_speech(*duration_ms, speech)))
        }

        fn job_progress(&mut self, progress: &JobProgress) {
            self.events.push(Event::Progress(progress.status, progress.done, progress.total));
        }

        fn meeting_updated(&mut self, meeting_id: &str, change: MeetingChange) {
            self.events.push(Event::Updated(meeting_id.to_string(), change));
        }

        fn auto_delete_audio_days(&mut self) -> u32 {
            self.retention_days
        }

        fn delete_audio_files(&mut self, meeting_id: &str) -> Result<(), String> {
            self.deleted_audio.push(meeting_id.to_string());
            Ok(())
        }

        fn should_stop(&mut self) -> bool {
            self.stop_after_decodes
                .is_some_and(|n| self.script.borrow().decodes.len() >= n)
        }
    }

    /// A worker that emits every event, so tests see them all.
    fn worker(conn: Connection, host: FakeHost) -> Worker<FakeHost> {
        let mut worker = Worker::new(conn, host);
        worker.progress_every = Duration::ZERO;
        worker.transcript_every = Duration::ZERO;
        worker
    }

    fn two_track_meeting(conn: &Connection) -> TestMeeting {
        new_meeting(conn, &[(TRACK_MS, MIC_SPEECH), (TRACK_MS, SYSTEM_SPEECH)])
    }

    fn enqueue(conn: &Connection, meeting: &TestMeeting, language: MeetingLanguage) -> JobProgress {
        jobs::enqueue_transcription(
            conn,
            &meeting.id,
            &RetranscribeOptions { model: None, language: Some(language) },
        )
        .unwrap()
    }

    fn progress_events(events: &[Event]) -> Vec<(JobStatus, u32, u32)> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Progress(status, done, total) => Some((*status, *done, *total)),
                Event::Updated(..) => None,
            })
            .collect()
    }

    #[test]
    fn both_tracks_are_transcribed_and_the_meeting_ends_ready() {
        let db = TestDb::new();
        let meeting = two_track_meeting(&db.conn);
        let job = enqueue(&db.conn, &meeting, MeetingLanguage::Nl);
        let mut worker = worker(db.connect(), FakeHost::for_meetings(&[&meeting]));

        assert_eq!(worker.drain(), 1);

        let detail = store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap();
        assert_eq!(detail.meeting.status, MeetingStatus::Ready);
        assert!(detail.meeting.job.is_none());
        assert_eq!(detail.active_run_id, job.run_id);
        assert_eq!(detail.runs[0].status, RunStatus::Done);
        assert_eq!(detail.runs[0].error, None);
        let row = store::get_job(&db.conn, &job.job_id).unwrap().unwrap();
        assert_eq!(row.status, JobStatus::Done);
        assert_eq!((row.progress_done, row.progress_total), (5, 5));

        let segments = store::list_segments(&db.conn, &meeting.id, None).unwrap();
        let of = |kind| segments.iter().filter(|s| s.track_kind == kind).count();
        assert_eq!((of(TrackKind::Mic), of(TrackKind::System)), (3, 2));
        assert!(segments.iter().all(|s| s.suppressed_reason.is_none() && s.lang.as_deref() == Some("nl")));
        // Windows were decoded in timeline order, across both tracks.
        let starts: Vec<u64> = segments.iter().map(|s| s.start_ms).collect();
        assert_eq!(starts, vec![800, 4_800, 9_800, 14_800, 19_800]);
        let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts[0], "Decoded in call 0.");
        assert_eq!(texts[4], "Decoded in call 4.");
        // An explicit language never runs detection.
        assert_eq!(worker.host.script.borrow().detections, 0);
    }

    #[test]
    fn resume_after_a_restart_skips_the_windows_that_are_done() {
        let db = TestDb::new();
        let meeting = two_track_meeting(&db.conn);
        let job = enqueue(&db.conn, &meeting, MeetingLanguage::Auto);

        // First launch: the app quits after two windows.
        let mut host = FakeHost::for_meetings(&[&meeting]);
        host.stop_after_decodes = Some(2);
        let mut first = worker(db.connect(), host);
        first.drain();
        assert_eq!(first.host.script.borrow().decodes.len(), 2);
        // One window of each track is done, so both languages were detected.
        assert_eq!(first.host.script.borrow().detections, 2);
        let row = store::get_job(&db.conn, &job.job_id).unwrap().unwrap();
        assert_eq!(row.status, JobStatus::Running, "a quit leaves the job running");
        assert_eq!((row.progress_done, row.progress_total), (2, 5));
        let windows_before = store::list_windows(&db.conn, job.run_id.as_deref().unwrap()).unwrap();
        drop(first);

        // Second launch: recovery requeues, a new worker finishes the rest.
        assert_eq!(jobs::recover_at_launch(&db.conn).unwrap(), 1);
        let mut second = worker(db.connect(), FakeHost::for_meetings(&[&meeting]));
        assert_eq!(second.drain(), 1);

        assert_eq!(second.host.script.borrow().decodes.len(), 3, "done windows are not decoded again");
        let windows_after = store::list_windows(&db.conn, job.run_id.as_deref().unwrap()).unwrap();
        assert_eq!(
            windows_after.iter().map(|w| &w.id).collect::<Vec<_>>(),
            windows_before.iter().map(|w| &w.id).collect::<Vec<_>>(),
            "the plan is made once"
        );
        assert!(windows_after.iter().all(|w| w.status == WindowStatus::Done && w.attempts == 1));
        let segments = store::list_segments(&db.conn, &meeting.id, None).unwrap();
        assert_eq!(segments.len(), 5);
        // The languages are read back from the done windows, not detected again.
        assert_eq!(second.host.script.borrow().detections, 0);
        let row = store::get_job(&db.conn, &job.job_id).unwrap().unwrap();
        assert_eq!((row.status, row.attempts), (JobStatus::Done, 2));
        assert_eq!(
            store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap().meeting.status,
            MeetingStatus::Ready
        );
    }

    #[test]
    fn auto_detects_the_language_once_per_track() {
        let db = TestDb::new();
        let meeting = two_track_meeting(&db.conn);
        enqueue(&db.conn, &meeting, MeetingLanguage::Auto);
        let mut worker = worker(db.connect(), FakeHost::for_meetings(&[&meeting]));
        worker.drain();
        assert_eq!(worker.host.script.borrow().detections, 2);
        assert!(worker.host.script.borrow().decodes.iter().all(|(_, lang)| lang == "nl"));
    }

    #[test]
    fn a_preempted_window_stays_pending_and_is_decoded_later() {
        let db = TestDb::new();
        let meeting = two_track_meeting(&db.conn);
        let job = enqueue(&db.conn, &meeting, MeetingLanguage::En);
        let run_id = job.run_id.clone().unwrap();
        let host = FakeHost::for_meetings(&[&meeting]);
        // Dictation takes the model away during the second window's decode.
        host.script.borrow_mut().steps.insert(1, Step::Preempt);
        // What the database says when the worker comes back for it.
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (observer, seen_in_hook, run) = (db.connect(), seen.clone(), run_id.clone());
        host.script.borrow_mut().before_decode = Some(Box::new(move |call| {
            if call == 2 {
                let window = store::next_pending_window(&observer, &run).unwrap().unwrap();
                seen_in_hook.borrow_mut().push((window.seq, window.track_kind, window.attempts, window.error));
            }
        }));
        let mut worker = worker(db.connect(), host);

        worker.drain();

        // After the preemption the same window (system, seq 0) is still the
        // next pending one, with no attempt and no error against it.
        assert_eq!(*seen.borrow(), vec![(0, TrackKind::System, 0, None)]);
        let script = worker.host.script.borrow();
        assert_eq!(script.decodes.len(), 6, "five windows, one of them twice");
        assert_eq!(script.decodes[1], script.decodes[2], "the preempted window is the one retried");
        let windows = store::list_windows(&db.conn, &run_id).unwrap();
        assert!(windows.iter().all(|w| w.status == WindowStatus::Done && w.attempts == 1));
        assert_eq!(store::list_segments(&db.conn, &meeting.id, None).unwrap().len(), 5);
        assert_eq!(store::get_job(&db.conn, &job.job_id).unwrap().unwrap().status, JobStatus::Done);
    }

    #[test]
    fn three_failures_mark_a_window_failed_and_the_job_still_completes() {
        let db = TestDb::new();
        let meeting = two_track_meeting(&db.conn);
        let job = enqueue(&db.conn, &meeting, MeetingLanguage::En);
        let run_id = job.run_id.clone().unwrap();
        let host = FakeHost::for_meetings(&[&meeting]);
        // The first window fails once and recovers; the second never decodes.
        {
            let mut script = host.script.borrow_mut();
            script.steps.insert(0, Step::Fail("decoder hiccup"));
            for call in 2..5 {
                script.steps.insert(call, Step::Fail("whisper_full failed"));
            }
        }
        let mut worker = worker(db.connect(), host);

        worker.drain();

        let windows = store::list_windows(&db.conn, &run_id).unwrap();
        let summary: Vec<(WindowStatus, u32)> = windows.iter().map(|w| (w.status, w.attempts)).collect();
        assert_eq!(
            summary,
            vec![
                (WindowStatus::Done, 2),
                (WindowStatus::Failed, 3),
                (WindowStatus::Done, 1),
                (WindowStatus::Done, 1),
                (WindowStatus::Done, 1),
            ]
        );
        assert_eq!(windows[1].error.as_deref(), Some("whisper_full failed"));
        assert_eq!(worker.host.script.borrow().decodes.len(), 8);

        let detail = store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap();
        assert_eq!(detail.meeting.status, MeetingStatus::Ready);
        assert_eq!(detail.runs[0].status, RunStatus::Done);
        assert_eq!(detail.runs[0].error.as_deref(), Some("1 of 5 parts could not be transcribed."));
        assert_eq!(store::list_segments(&db.conn, &meeting.id, None).unwrap().len(), 4);
        let row = store::get_job(&db.conn, &job.job_id).unwrap().unwrap();
        assert_eq!(row.status, JobStatus::Done);
        assert_eq!((row.progress_done, row.progress_total), (5, 5), "a failed window counts as handled");
    }

    #[test]
    fn a_run_where_every_window_fails_is_a_failed_job() {
        let db = TestDb::new();
        let meeting = new_meeting(&db.conn, &[(TRACK_MS, &[(1_000, 2_000)])]);
        let job = enqueue(&db.conn, &meeting, MeetingLanguage::En);
        let host = FakeHost::for_meetings(&[&meeting]);
        for call in 0..3 {
            host.script.borrow_mut().steps.insert(call, Step::Fail("out of memory"));
        }
        let mut worker = worker(db.connect(), host);

        worker.drain();

        let detail = store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap();
        assert_eq!(detail.meeting.status, MeetingStatus::Failed);
        assert!(detail.error.as_deref().unwrap().contains("out of memory"));
        assert_eq!(detail.runs[0].status, RunStatus::Failed);
        assert_eq!(store::get_job(&db.conn, &job.job_id).unwrap().unwrap().status, JobStatus::Failed);
    }

    #[test]
    fn progress_and_updates_arrive_in_order() {
        let db = TestDb::new();
        let meeting = two_track_meeting(&db.conn);
        enqueue(&db.conn, &meeting, MeetingLanguage::En);
        let mut worker = worker(db.connect(), FakeHost::for_meetings(&[&meeting]));

        worker.drain();

        use JobStatus::{Done, Running};
        assert_eq!(
            progress_events(&worker.host.events),
            vec![
                (Running, 0, 0), // claimed: planning
                (Running, 0, 5), // planned
                (Running, 1, 5),
                (Running, 2, 5),
                (Running, 3, 5),
                (Running, 4, 5),
                (Running, 5, 5),
                (Done, 5, 5),
            ]
        );
        let updated = |change| Event::Updated(meeting.id.clone(), change);
        let events = &worker.host.events;
        assert_eq!(events.first(), Some(&updated(MeetingChange::Status)), "transcribing comes first");
        assert_eq!(events.last(), Some(&updated(MeetingChange::Status)), "ready comes last");
        let transcripts = events.iter().filter(|e| **e == updated(MeetingChange::Transcript)).count();
        assert_eq!(transcripts, 5, "one per window when nothing is throttled");
        // Each window's segments are announced after its progress.
        let at = |wanted: &Event| events.iter().position(|e| e == wanted).unwrap();
        assert!(at(&Event::Progress(Running, 1, 5)) < at(&updated(MeetingChange::Transcript)));
    }

    #[test]
    fn events_are_throttled_but_the_first_and_last_always_arrive() {
        let db = TestDb::new();
        let meeting = two_track_meeting(&db.conn);
        enqueue(&db.conn, &meeting, MeetingLanguage::En);
        let mut worker = Worker::new(db.connect(), FakeHost::for_meetings(&[&meeting]));
        worker.progress_every = Duration::from_secs(3600);
        worker.transcript_every = Duration::from_secs(3600);

        worker.drain();

        use JobStatus::{Done, Running};
        assert_eq!(
            progress_events(&worker.host.events),
            vec![(Running, 0, 0), (Running, 0, 5), (Done, 5, 5)]
        );
        let transcripts = worker
            .host
            .events
            .iter()
            .filter(|e| matches!(e, Event::Updated(_, MeetingChange::Transcript)))
            .count();
        assert_eq!(transcripts, 2, "the first window, then what was held back at the end");

        let mut throttle = Throttle::new(Duration::from_millis(300));
        let start = Instant::now();
        assert!(throttle.ready(start));
        assert!(!throttle.ready(start + Duration::from_millis(299)));
        assert!(throttle.ready(start + Duration::from_millis(300)));
        assert!(!throttle.ready(start + Duration::from_millis(400)));
    }

    #[test]
    fn retranscribe_adds_a_run_and_flips_the_active_one_only_when_it_is_done() {
        let db = TestDb::new();
        let meeting = two_track_meeting(&db.conn);
        let first = enqueue(&db.conn, &meeting, MeetingLanguage::En);
        let first_run = first.run_id.clone().unwrap();
        worker(db.connect(), FakeHost::for_meetings(&[&meeting])).drain();

        let second = enqueue(&db.conn, &meeting, MeetingLanguage::Nl);
        let second_run = second.run_id.clone().unwrap();
        assert_ne!(first_run, second_run);
        let queued = store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap();
        assert_eq!(queued.meeting.status, MeetingStatus::Ready, "the old transcript stays on show");
        assert_eq!(queued.active_run_id.as_deref(), Some(first_run.as_str()));

        let host = FakeHost::for_meetings(&[&meeting]);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (observer, seen_in_hook, meeting_id) = (db.connect(), seen.clone(), meeting.id.clone());
        host.script.borrow_mut().before_decode = Some(Box::new(move |_call| {
            let detail = store::get_meeting(&observer, &meeting_id).unwrap().unwrap();
            seen_in_hook.borrow_mut().push((detail.meeting.status, detail.active_run_id));
        }));
        let mut worker = worker(db.connect(), host);
        worker.drain();

        assert_eq!(seen.borrow().len(), 5);
        assert!(
            seen.borrow().iter().all(|(status, active)| *status == MeetingStatus::Ready
                && active.as_deref() == Some(first_run.as_str())),
            "the first run stays active while the second one is decoded"
        );
        let detail = store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap();
        assert_eq!(detail.active_run_id.as_deref(), Some(second_run.as_str()));
        assert_eq!(detail.meeting.status, MeetingStatus::Ready);
        assert_eq!(detail.runs.len(), 2);
        assert!(detail.runs.iter().all(|run| run.status == RunStatus::Done));
        assert_eq!(detail.runs[1].language, MeetingLanguage::Nl);
        // The old run's segments are still there, the new run's are on show.
        assert_eq!(store::list_segments(&db.conn, &meeting.id, Some(&first_run)).unwrap().len(), 5);
        let shown = store::list_segments(&db.conn, &meeting.id, None).unwrap();
        assert!(shown.iter().all(|s| s.run_id == second_run && s.lang.as_deref() == Some("nl")));
        // A background re-run announces its transcript once, at the flip.
        let transcripts = worker
            .host
            .events
            .iter()
            .filter(|e| matches!(e, Event::Updated(_, MeetingChange::Transcript)))
            .count();
        assert_eq!(transcripts, 1);
    }

    #[test]
    fn a_model_meetings_cannot_use_fails_the_job_with_the_reason() {
        let db = TestDb::new();
        let meeting = two_track_meeting(&db.conn);
        let job = enqueue(&db.conn, &meeting, MeetingLanguage::En);
        let reason = jobs::usable_model("parakeet-tdt-0.6b-v3").unwrap_err();
        let mut host = FakeHost::for_meetings(&[&meeting]);
        host.decoder_error = Some(reason.clone());
        let mut worker = worker(db.connect(), host);

        worker.drain();

        let detail = store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap();
        assert_eq!(detail.meeting.status, MeetingStatus::Failed);
        assert_eq!(detail.error.as_deref(), Some(reason.as_str()));
        assert_eq!(detail.runs[0].status, RunStatus::Failed);
        let row = store::get_job(&db.conn, &job.job_id).unwrap().unwrap();
        assert_eq!((row.status, row.error.as_deref()), (JobStatus::Failed, Some(reason.as_str())));
        assert!(store::list_windows(&db.conn, job.run_id.as_deref().unwrap()).unwrap().is_empty());
        assert_eq!(
            progress_events(&worker.host.events).last(),
            Some(&(JobStatus::Failed, 0, 0))
        );
    }

    #[test]
    fn a_failed_retranscribe_leaves_the_finished_transcript_alone() {
        let db = TestDb::new();
        let meeting = two_track_meeting(&db.conn);
        let first = enqueue(&db.conn, &meeting, MeetingLanguage::En);
        worker(db.connect(), FakeHost::for_meetings(&[&meeting])).drain();

        let second = enqueue(&db.conn, &meeting, MeetingLanguage::Nl);
        let mut host = FakeHost::for_meetings(&[&meeting]);
        host.decoder_error = Some("The model whisper-small-q5 is not installed.".to_string());
        worker(db.connect(), host).drain();

        let detail = store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap();
        assert_eq!(detail.meeting.status, MeetingStatus::Ready);
        assert_eq!(detail.error, None);
        assert_eq!(detail.active_run_id, first.run_id);
        assert_eq!(detail.runs[1].status, RunStatus::Failed);
        assert_eq!(store::get_job(&db.conn, &second.job_id).unwrap().unwrap().status, JobStatus::Failed);
    }

    #[test]
    fn a_job_cancelled_under_the_worker_is_dropped_quietly() {
        let db = TestDb::new();
        let meeting = two_track_meeting(&db.conn);
        let job = enqueue(&db.conn, &meeting, MeetingLanguage::En);
        let host = FakeHost::for_meetings(&[&meeting]);
        let (ui, meeting_id) = (db.connect(), meeting.id.clone());
        host.script.borrow_mut().before_decode = Some(Box::new(move |call| {
            if call == 1 {
                store::cancel_unfinished_jobs(&ui, &meeting_id).unwrap();
            }
        }));
        let mut worker = worker(db.connect(), host);

        worker.drain();

        assert_eq!(worker.host.script.borrow().decodes.len(), 2, "nothing is decoded after the cancel");
        let row = store::get_job(&db.conn, &job.job_id).unwrap().unwrap();
        assert_eq!((row.status, row.error), (JobStatus::Cancelled, None));
        assert!(!progress_events(&worker.host.events)
            .iter()
            .any(|(status, ..)| matches!(status, JobStatus::Done | JobStatus::Failed)));
    }

    #[test]
    fn a_meeting_without_speech_is_ready_with_an_empty_transcript() {
        let db = TestDb::new();
        let meeting = new_meeting(&db.conn, &[(TRACK_MS, &[]), (TRACK_MS, &[])]);
        let job = enqueue(&db.conn, &meeting, MeetingLanguage::Auto);
        let mut worker = worker(db.connect(), FakeHost::for_meetings(&[&meeting]));

        worker.drain();

        let detail = store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap();
        assert_eq!(detail.meeting.status, MeetingStatus::Ready);
        assert_eq!(detail.runs[0].status, RunStatus::Done);
        assert_eq!(store::get_job(&db.conn, &job.job_id).unwrap().unwrap().status, JobStatus::Done);
        assert!(worker.host.script.borrow().decodes.is_empty());
    }

    #[test]
    fn jobs_run_in_queue_order_and_a_failure_does_not_stop_the_queue() {
        let db = TestDb::new();
        let broken = new_meeting(&db.conn, &[(TRACK_MS, MIC_SPEECH)]);
        let fine = two_track_meeting(&db.conn);
        enqueue(&db.conn, &broken, MeetingLanguage::En);
        enqueue(&db.conn, &fine, MeetingLanguage::En);
        // The broken meeting's audio is unknown to the host.
        let mut worker = worker(db.connect(), FakeHost::for_meetings(&[&fine]));

        assert_eq!(worker.drain(), 2);

        let status = |id: &str| store::get_meeting(&db.conn, id).unwrap().unwrap().meeting.status;
        assert_eq!(status(&broken.id), MeetingStatus::Failed);
        assert_eq!(status(&fine.id), MeetingStatus::Ready);
    }

    #[test]
    fn audio_retention_keeps_the_audio_of_a_meeting_with_failed_windows() {
        let db = TestDb::new();
        let meeting = two_track_meeting(&db.conn);
        enqueue(&db.conn, &meeting, MeetingLanguage::En);
        let mut host = FakeHost::for_meetings(&[&meeting]);
        host.retention_days = 30;
        // The first window never decodes; the rest of the run is fine.
        for call in 0..3 {
            host.script.borrow_mut().steps.insert(call, Step::Fail("whisper_full failed"));
        }
        let mut worker = worker(db.connect(), host);
        worker.drain();
        let detail = store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap();
        assert_eq!(detail.meeting.status, MeetingStatus::Ready);
        assert_eq!(detail.runs[0].error.as_deref(), Some("1 of 5 parts could not be transcribed."));

        // Re-transcribing is the only way to get that part back.
        assert!(worker.apply_audio_retention(Utc::now() + chrono::Duration::days(365)).is_empty());
        assert!(store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap().meeting.has_audio);
    }

    #[test]
    fn audio_retention_deletes_only_old_finished_meetings() {
        let db = TestDb::new();
        let old = two_track_meeting(&db.conn);
        let unfinished = two_track_meeting(&db.conn);
        enqueue(&db.conn, &old, MeetingLanguage::En);
        let mut host = FakeHost::for_meetings(&[&old, &unfinished]);
        host.retention_days = 30;
        let mut worker = worker(db.connect(), host);
        worker.drain();
        // Still queued when retention runs: never touched.
        enqueue(&db.conn, &unfinished, MeetingLanguage::En);

        let now = Utc::now();
        assert!(worker.apply_audio_retention(now + chrono::Duration::days(29)).is_empty());
        assert_eq!(
            worker.apply_audio_retention(now + chrono::Duration::days(31)),
            vec![old.id.clone()]
        );

        assert_eq!(worker.host.deleted_audio, vec![old.id.clone()]);
        let detail = store::get_meeting(&db.conn, &old.id).unwrap().unwrap();
        assert!(!detail.meeting.has_audio);
        assert_eq!(store::list_segments(&db.conn, &old.id, None).unwrap().len(), 5, "the transcript stays");
        assert!(store::get_meeting(&db.conn, &unfinished.id).unwrap().unwrap().meeting.has_audio);
        assert_eq!(
            worker.host.events.last(),
            Some(&Event::Updated(old.id.clone(), MeetingChange::AudioDeleted))
        );
        // Nothing left to do, and 0 days means never.
        assert!(worker.apply_audio_retention(now + chrono::Duration::days(365)).is_empty());
        worker.host.retention_days = 0;
        assert!(worker.apply_audio_retention(now + chrono::Duration::days(365)).is_empty());
    }

    #[test]
    fn a_wake_before_the_wait_is_not_lost() {
        let signal = WakeSignal::new();
        signal.wake();
        let started = Instant::now();
        signal.wait(Duration::from_secs(30));
        assert!(started.elapsed() < Duration::from_secs(5));
        // Consumed: the next wait runs into its timeout.
        let started = Instant::now();
        signal.wait(Duration::from_millis(50));
        assert!(started.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn a_wake_from_another_thread_ends_the_wait() {
        let signal = WakeSignal::new();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                signal.wake();
            });
            let started = Instant::now();
            signal.wait(Duration::from_secs(30));
            assert!(started.elapsed() < Duration::from_secs(5));
        });
    }

    /// The real model behind the fake audio: every window is a real encoder
    /// and decoder pass that dictation has to interrupt.
    struct RealModelHost {
        inner: FakeHost,
        model: String,
    }

    impl Host for RealModelHost {
        fn decoder(&mut self, _model: &str) -> Result<Box<dyn WindowDecoder>, String> {
            let model_id = jobs::usable_model(&self.model)?;
            Ok(Box::new(WhisperDecoder::load(&model_id)?))
        }
        fn detector(&mut self) -> Result<Box<dyn SpeechDetector>, String> {
            self.inner.detector()
        }
        fn track_audio(&mut self, conn: &Connection, track: &MeetingTrack) -> Result<Box<dyn TrackAudio>, String> {
            self.inner.track_audio(conn, track)
        }
        fn job_progress(&mut self, progress: &JobProgress) {
            self.inner.job_progress(progress);
        }
        fn meeting_updated(&mut self, meeting_id: &str, change: MeetingChange) {
            self.inner.meeting_updated(meeting_id, change);
        }
        fn auto_delete_audio_days(&mut self) -> u32 {
            0
        }
        fn delete_audio_files(&mut self, _meeting_id: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    #[ignore = "needs an installed Whisper model (FT_MEETING_MODEL, default whisper-large-v3-turbo-q5)"]
    fn dictation_waits_only_briefly_for_the_gate_while_a_job_runs() {
        let model = std::env::var("FT_MEETING_MODEL").unwrap_or_else(|_| "whisper-large-v3-turbo-q5".to_string());
        jobs::usable_model(&model).expect("the model must be installed");
        let db = TestDb::new();
        // Ten minutes of "speech": about 22 full-length windows.
        let meeting = new_meeting(&db.conn, &[(600_000, &[(0, 600_000)])]);
        let job = enqueue(&db.conn, &meeting, MeetingLanguage::En);
        let audio = meeting
            .tracks
            .iter()
            .map(|t| (t.id.clone(), (t.duration_ms, t.speech.clone())))
            .collect::<HashMap<_, _>>();
        let worker_conn = db.connect();
        let background = std::thread::spawn(move || {
            let inner = FakeHost { audio, ..FakeHost::default() };
            Worker::new(worker_conn, RealModelHost { inner, model }).drain();
        });

        // Let the job get into its windows, then dictate ten times.
        let run_id = job.run_id.unwrap();
        while store::window_counts(&db.conn, &run_id).unwrap().done == 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut waits = Vec::new();
        for _ in 0..10 {
            let asked = Instant::now();
            let guard = inference_gate::acquire_interactive();
            waits.push(asked.elapsed());
            std::thread::sleep(Duration::from_millis(300)); // the dictation itself
            drop(guard);
            std::thread::sleep(Duration::from_millis(700));
        }
        background.join().unwrap();

        // whisper.cpp polls the abort callback between graph computes, not
        // inside one, so the longest wait is one encoder pass: measured at
        // 0.75-0.9 s for large-v3-turbo on Metal. Never a whole window.
        println!("dictation waited for the gate: {waits:?}");
        let worst = waits.iter().max().unwrap();
        assert!(*worst < Duration::from_millis(1_500), "dictation waited {worst:?} for the gate");
        let windows = store::list_windows(&db.conn, &run_id).unwrap();
        assert!(windows.iter().all(|w| w.status == WindowStatus::Done), "preempted windows were decoded later");
    }
}

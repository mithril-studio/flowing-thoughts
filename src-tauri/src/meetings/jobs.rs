//! OWNER: WP6 (jobs and worker). The SQLite-backed job queue.
//!
//! A transcription is a `transcript_runs` row (model, language and decode
//! parameters, so a re-run never destroys results) plus a `jobs` row that
//! points at it, inserted together. The `meeting-worker` thread (`worker.rs`)
//! drains the queue by `priority`, then `created_at`. Nothing here is kept in
//! memory: the app exits through `_exit(0)` and may crash, so the queue is
//! whatever SQLite says it is.
//!
//! - `enqueue_transcription`: for the session (WP7), at stop and after launch
//!   recovery. Safe to call twice.
//! - `retranscribe`: "Re-transcribe as…", from `commands.rs`. A new run; the
//!   old one stays the meeting's active run until the new one finishes.
//! - `enqueue`: any other job. Another kind (WP10's summary) needs a
//!   `JobKind` variant in `types.rs` and an arm in `worker.rs`, nothing else.
//! - `recover_at_launch`: every `running` job goes back to `queued`.
//!
//! Anything that adds a job calls `worker::wake()`.

use rusqlite::Connection;
use tauri::{AppHandle, Manager};

use super::types::{
    JobKind, JobProgress, MeetingChange, MeetingDetail, MeetingLanguage, MeetingStatus,
    RetranscribeOptions, RunStatus,
};
use super::{events, longform, store, worker, DbState, PersistedHandle};
use crate::model_manager::{self, Engine, ModelId};

/// What an unfinished transcription job of the same meeting means to the
/// caller.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IfBusy {
    /// The session asking twice (stop, then launch recovery) gets that job.
    #[allow(dead_code)] // scaffold: first used through `enqueue_transcription` (WP7)
    ReturnExisting,
    /// A second "Re-transcribe as…" is refused.
    Refuse,
}

/// Queues the transcription of a recorded meeting: one run covering every
/// track that has audio, and its job. For the session (WP7), once the chunks
/// are closed.
///
/// `options.model` defaults to the meeting's own model (pass
/// `settings.meetings.model` when the meeting row has none),
/// `options.language` to the meeting's language. A meeting that already has
/// an unfinished job gets that job back instead of a second one. The model is
/// not checked here: a missing model fails the job, where the user sees it,
/// rather than the stop of a recording.
///
/// Wakes the worker. Inside a caller's transaction the worker may look before
/// the commit: call `worker::wake()` again afterwards.
#[allow(dead_code)] // scaffold: first used by session.rs (WP7)
pub fn enqueue_transcription(
    conn: &Connection,
    meeting_id: &str,
    options: &RetranscribeOptions,
) -> Result<JobProgress, String> {
    enqueue_run(conn, meeting_id, options, IfBusy::ReturnExisting)
}

/// "Re-transcribe as…": a new run and job for a recorded meeting. Refused
/// while the meeting is recording, has an unfinished job, or has no audio
/// left. Emits `meeting-job-progress` and wakes the worker.
pub fn retranscribe(
    app: &AppHandle,
    meeting_id: &str,
    options: RetranscribeOptions,
) -> Result<JobProgress, String> {
    let mut options = options;
    if options.model.as_deref().is_none_or(|m| m.trim().is_empty()) {
        let persisted = app
            .try_state::<PersistedHandle>()
            .ok_or_else(|| "Settings are not available yet".to_string())?;
        let state = persisted
            .inner()
            .lock()
            .map_err(|_| "Persisted state lock poisoned".to_string())?;
        options.model = Some(state.settings.meetings.model.clone());
    }
    // The user is waiting for an answer, so a model that cannot work is
    // refused here instead of failing the job a moment later.
    usable_model(options.model.as_deref().unwrap_or_default())?;

    let (progress, status_changed) = {
        let db = app
            .try_state::<DbState>()
            .ok_or_else(|| "The database is not available yet".to_string())?;
        let conn = db.inner().lock().map_err(|_| "DB lock poisoned".to_string())?;
        let before = store::get_meeting(&conn, meeting_id)?.map(|m| m.meeting.status);
        let progress = enqueue_run(&conn, meeting_id, &options, IfBusy::Refuse)?;
        let after = store::get_meeting(&conn, meeting_id)?.map(|m| m.meeting.status);
        (progress, before != after)
    };
    if status_changed {
        events::emit_updated(app, meeting_id, MeetingChange::Status);
    }
    events::emit_job_progress(app, &progress);
    Ok(progress)
}

/// Adds any job to the queue and wakes the worker: the slot for a job kind
/// that has no run of its own (WP10's summary). Transcriptions go through
/// `enqueue_transcription` and `retranscribe`, which insert the run with it.
#[allow(dead_code)] // scaffold: first used by summary.rs (WP10)
pub fn enqueue(conn: &Connection, job: &store::NewJob) -> Result<JobProgress, String> {
    let row = store::insert_job(conn, job)?;
    worker::wake();
    Ok(row.progress())
}

/// Launch repair: nothing can be running when the app has just started, so
/// every `running` job goes back to `queued`, keeping its place and its
/// progress. The windows that are `done` stay done, which is what makes the
/// job resume instead of restart. Returns how many jobs were requeued.
/// `worker::start` calls this before the thread takes its first job.
pub fn recover_at_launch(conn: &Connection) -> Result<usize, String> {
    store::requeue_running_jobs(conn)
}

/// The model a run names, as something meetings can decode with: a known,
/// installed Whisper model. The message is what the user sees on the failed
/// job, so it says what to do.
pub fn usable_model(model: &str) -> Result<ModelId, String> {
    let model = model.trim();
    if model.is_empty() {
        return Err(
            "No model is chosen for meeting transcription. Choose one in Settings → Meetings."
                .to_string(),
        );
    }
    let id = ModelId::from_str(model).ok_or_else(|| {
        format!("Unknown transcription model '{model}'. Choose a model in Settings → Meetings.")
    })?;
    if id.engine() != Engine::Whisper {
        return Err(format!(
            "Meeting transcription needs a Whisper model: {model} has no timestamps. Choose a Whisper model in Settings → Meetings."
        ));
    }
    if !model_manager::is_installed(&id) {
        return Err(format!(
            "The model {model} is not installed. Download it in Settings, or choose another model in Settings → Meetings."
        ));
    }
    Ok(id)
}

/// Whether the meeting already shows a finished transcript. Such a meeting
/// stays `ready` while it is transcribed again: the old run is what the user
/// sees until the new one is complete.
pub(crate) fn shows_finished_run(meeting: &MeetingDetail) -> bool {
    meeting.active_run_id.as_deref().is_some_and(|active| {
        meeting.runs.iter().any(|run| run.id == active && run.status == RunStatus::Done)
    })
}

/// The decode parameters a run records, so its result can be reproduced and
/// told apart from a run made with later defaults.
fn run_params_json() -> String {
    serde_json::json!({
        "max_window_ms": longform::MAX_WINDOW_MS,
        "gap_break_ms": longform::GAP_BREAK_MS,
        "window_pad_ms": longform::WINDOW_PAD_MS,
        "no_context": true,
        "temperature": {
            "start": longform::MEETING_TEMPERATURE.start,
            "increment": longform::MEETING_TEMPERATURE.increment,
            "entropy_threshold": longform::MEETING_TEMPERATURE.entropy_threshold,
            "logprob_threshold": longform::MEETING_TEMPERATURE.logprob_threshold,
        },
        "vad": {
            "min_speech_ms": longform::MEETING_VAD.min_speech_ms,
            "min_silence_ms": longform::MEETING_VAD.min_silence_ms,
            "speech_pad_ms": longform::MEETING_VAD.speech_pad_ms,
            "max_speech_s": longform::MEETING_VAD.max_speech_s,
        },
    })
    .to_string()
}

fn enqueue_run(
    conn: &Connection,
    meeting_id: &str,
    options: &RetranscribeOptions,
    if_busy: IfBusy,
) -> Result<JobProgress, String> {
    let progress = store::transaction(conn, || {
        let meeting = store::get_meeting(conn, meeting_id)?
            .ok_or_else(|| format!("Meeting '{meeting_id}' not found"))?;
        if matches!(
            meeting.meeting.status,
            MeetingStatus::Recording | MeetingStatus::Paused
        ) {
            return Err("This meeting is still being recorded. Stop it first.".to_string());
        }
        if let Some(job) = store::list_jobs(conn, meeting_id)?.into_iter().find(|job| {
            job.kind == JobKind::Transcribe && !is_settled(job.status)
        }) {
            return match if_busy {
                IfBusy::ReturnExisting => Ok(job.progress()),
                IfBusy::Refuse => Err("This meeting is already being transcribed.".to_string()),
            };
        }
        if !meeting.meeting.has_audio || !meeting.tracks.iter().any(|track| track.has_audio) {
            return Err(
                "The audio of this meeting was deleted, so it cannot be transcribed again."
                    .to_string(),
            );
        }

        let model = options
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .or_else(|| stored_model(&meeting))
            .ok_or_else(|| {
                "No model is chosen for meeting transcription. Choose one in Settings → Meetings."
                    .to_string()
            })?;
        let language: MeetingLanguage = options.language.unwrap_or(meeting.meeting.language);

        let run_id = store::insert_run(
            conn,
            &store::NewRun {
                meeting_id: meeting_id.to_string(),
                model,
                language,
                params_json: Some(run_params_json()),
            },
        )?;
        let job = store::insert_job(
            conn,
            &store::NewJob {
                kind: JobKind::Transcribe,
                meeting_id: meeting_id.to_string(),
                run_id: Some(run_id),
                priority: 0,
                payload_json: None,
            },
        )?;
        if !shows_finished_run(&meeting) {
            store::set_meeting_status(conn, meeting_id, MeetingStatus::Queued, None)?;
        }
        Ok(job.progress())
    })?;
    worker::wake();
    Ok(progress)
}

fn is_settled(status: super::types::JobStatus) -> bool {
    use super::types::JobStatus;
    matches!(status, JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled)
}

/// `MeetingDetail.model` is the active run's model when there is one, else
/// the model the meeting was recorded for. Both are a sensible default.
fn stored_model(meeting: &MeetingDetail) -> Option<String> {
    meeting.model.as_deref().map(str::trim).filter(|m| !m.is_empty()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meetings::types::JobStatus;
    use crate::meetings::worker::test_support::{new_meeting, TestDb};

    fn options(model: Option<&str>, language: Option<MeetingLanguage>) -> RetranscribeOptions {
        RetranscribeOptions { model: model.map(str::to_string), language }
    }

    #[test]
    fn enqueue_creates_a_queued_run_and_job() {
        let db = TestDb::new();
        let meeting = new_meeting(&db.conn, &[(10_000, &[(0, 1_000)])]);

        let progress =
            enqueue_transcription(&db.conn, &meeting.id, &RetranscribeOptions::default()).unwrap();

        assert_eq!(progress.status, JobStatus::Queued);
        assert_eq!(progress.kind, JobKind::Transcribe);
        assert_eq!((progress.done, progress.total), (0, 0));
        let detail = store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap();
        assert_eq!(detail.meeting.status, MeetingStatus::Queued);
        assert_eq!(detail.runs.len(), 1);
        let run = &detail.runs[0];
        assert_eq!(Some(run.id.clone()), progress.run_id);
        assert_eq!(run.status, RunStatus::Queued);
        // Defaults come from the meeting row.
        assert_eq!(run.model, "whisper-small-q5");
        assert_eq!(run.language, MeetingLanguage::Auto);
        let params: serde_json::Value =
            serde_json::from_str(&store::get_run_params(&db.conn, &run.id).unwrap().unwrap())
                .unwrap();
        assert_eq!(params["max_window_ms"], 28_000);
        assert_eq!(params["no_context"], true);
    }

    #[test]
    fn enqueue_twice_returns_the_unfinished_job() {
        let db = TestDb::new();
        let meeting = new_meeting(&db.conn, &[(10_000, &[(0, 1_000)])]);
        let first =
            enqueue_transcription(&db.conn, &meeting.id, &RetranscribeOptions::default()).unwrap();
        let second =
            enqueue_transcription(&db.conn, &meeting.id, &options(None, Some(MeetingLanguage::Nl)))
                .unwrap();
        assert_eq!(first.job_id, second.job_id);
        assert_eq!(store::list_runs(&db.conn, &meeting.id).unwrap().len(), 1);
        assert_eq!(store::list_jobs(&db.conn, &meeting.id).unwrap().len(), 1);
    }

    #[test]
    fn a_second_retranscribe_is_refused_while_one_is_unfinished() {
        let db = TestDb::new();
        let meeting = new_meeting(&db.conn, &[(10_000, &[(0, 1_000)])]);
        enqueue_run(&db.conn, &meeting.id, &RetranscribeOptions::default(), IfBusy::Refuse).unwrap();
        let err = enqueue_run(&db.conn, &meeting.id, &RetranscribeOptions::default(), IfBusy::Refuse)
            .unwrap_err();
        assert!(err.contains("already being transcribed"), "{err}");
    }

    #[test]
    fn a_meeting_that_is_recording_is_refused() {
        let db = TestDb::new();
        let meeting = new_meeting(&db.conn, &[(10_000, &[(0, 1_000)])]);
        store::set_meeting_status(&db.conn, &meeting.id, MeetingStatus::Recording, None).unwrap();
        let err = enqueue_transcription(&db.conn, &meeting.id, &RetranscribeOptions::default())
            .unwrap_err();
        assert!(err.contains("still being recorded"), "{err}");
        assert!(store::list_jobs(&db.conn, &meeting.id).unwrap().is_empty());
    }

    #[test]
    fn deleted_audio_fails_cleanly_and_leaves_nothing_behind() {
        let db = TestDb::new();
        let meeting = new_meeting(&db.conn, &[(10_000, &[(0, 1_000)])]);
        store::set_meeting_status(&db.conn, &meeting.id, MeetingStatus::Ready, None).unwrap();
        store::mark_audio_deleted(&db.conn, &meeting.id).unwrap();

        let err = enqueue_run(
            &db.conn,
            &meeting.id,
            &options(None, Some(MeetingLanguage::En)),
            IfBusy::Refuse,
        )
        .unwrap_err();

        assert!(err.contains("audio of this meeting was deleted"), "{err}");
        assert!(store::list_runs(&db.conn, &meeting.id).unwrap().is_empty());
        assert!(store::list_jobs(&db.conn, &meeting.id).unwrap().is_empty());
        let detail = store::get_meeting(&db.conn, &meeting.id).unwrap().unwrap();
        assert_eq!(detail.meeting.status, MeetingStatus::Ready);
    }

    #[test]
    fn an_unknown_meeting_is_an_error() {
        let db = TestDb::new();
        let err = enqueue_transcription(&db.conn, "nope", &RetranscribeOptions::default())
            .unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn running_jobs_are_requeued_at_launch() {
        let db = TestDb::new();
        let first = new_meeting(&db.conn, &[(10_000, &[(0, 1_000)])]);
        let second = new_meeting(&db.conn, &[(10_000, &[(0, 1_000)])]);
        let running =
            enqueue_transcription(&db.conn, &first.id, &RetranscribeOptions::default()).unwrap();
        let queued =
            enqueue_transcription(&db.conn, &second.id, &RetranscribeOptions::default()).unwrap();
        let claimed = store::claim_next_job(&db.conn).unwrap().unwrap();
        assert_eq!(claimed.id, running.job_id);
        store::set_job_progress(&db.conn, &claimed.id, 2, 5).unwrap();

        assert_eq!(recover_at_launch(&db.conn).unwrap(), 1);

        let job = store::get_job(&db.conn, &running.job_id).unwrap().unwrap();
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!((job.progress_done, job.progress_total), (2, 5));
        // It keeps its place: still ahead of the job that was queued later.
        assert_eq!(store::next_queued_job(&db.conn).unwrap().unwrap().id, running.job_id);
        assert_eq!(
            store::get_job(&db.conn, &queued.job_id).unwrap().unwrap().status,
            JobStatus::Queued
        );
        assert_eq!(recover_at_launch(&db.conn).unwrap(), 0);
    }

    #[test]
    fn models_meetings_cannot_use_are_refused_with_a_reason() {
        assert!(usable_model("").unwrap_err().contains("No model is chosen"));
        assert!(usable_model("../../etc/passwd").unwrap_err().contains("Unknown transcription model"));
        let err = usable_model("parakeet-tdt-0.6b-v3").unwrap_err();
        assert!(err.contains("needs a Whisper model"), "{err}");
    }
}

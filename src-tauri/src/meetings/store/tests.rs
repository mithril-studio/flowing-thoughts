//! Store tests: an in-memory SQLite database at schema v3, no hardware and no
//! model files.

use rusqlite::{params, Connection};

use super::super::types::{
    ChunkRecord, ChunkStatus, JobKind, JobStatus, MeetingLanguage, MeetingStatus,
    ParticipantSource, RunStatus, SourceFormat, SpeakerSource, SummaryItemKind, SummaryStatus,
    SuppressedReason, TrackKind, WindowStatus,
};
use super::*;

fn memory_db() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    migrate(&conn);
    conn
}

/// `db.rs` keeps its migrations private and only ever opens the real database
/// file, so the test database is built from the migration SQL in its source:
/// every `"BEGIN; … COMMIT;"` batch above its test module, in order. That is
/// the real schema, not a copy that can drift. The `user_version` assert
/// fails loudly if `db.rs` ever changes shape.
fn migrate(conn: &Connection) {
    let source = include_str!("../../db.rs");
    let mut rest = &source[..source.find("#[cfg(test)]").expect("db.rs test module")];
    while let Some(start) = rest.find("\"BEGIN;") {
        let batch = &rest[start + 1..];
        let end = batch.find("COMMIT;\"").expect("end of migration batch") + "COMMIT;".len();
        conn.execute_batch(&batch[..end]).expect("run migration batch");
        rest = &batch[end..];
    }
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3, "expected the migrations in db.rs to end at v3");
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
        .unwrap()
}

struct Fixture {
    meeting_id: String,
    mic: String,
    system: String,
    run_id: String,
}

fn new_meeting(conn: &Connection, title: &str) -> String {
    insert_meeting(
        conn,
        &NewMeeting {
            title: title.to_string(),
            language: MeetingLanguage::Auto,
            model: Some("whisper-small-q5".to_string()),
            origin_host_ns: Some(1_000),
            calendar_event_id: None,
        },
    )
    .unwrap()
}

fn new_track(conn: &Connection, meeting_id: &str, kind: TrackKind) -> String {
    insert_track(
        conn,
        &NewTrack {
            meeting_id: meeting_id.to_string(),
            kind,
            device_name: Some("MacBook Pro Microphone".to_string()),
            format: Some(SourceFormat { sample_rate: 48_000, channels: 1 }),
        },
    )
    .unwrap()
}

fn new_run(conn: &Connection, meeting_id: &str) -> String {
    insert_run(
        conn,
        &NewRun {
            meeting_id: meeting_id.to_string(),
            model: "whisper-small-q5".to_string(),
            language: MeetingLanguage::Nl,
            params_json: None,
        },
    )
    .unwrap()
}

/// A meeting with both tracks, the seeded speakers and one active run.
fn fixture(conn: &Connection) -> Fixture {
    let meeting_id = new_meeting(conn, "Standup");
    let mic = new_track(conn, &meeting_id, TrackKind::Mic);
    let system = new_track(conn, &meeting_id, TrackKind::System);
    seed_track_speakers(conn, &meeting_id).unwrap();
    let run_id = new_run(conn, &meeting_id);
    set_active_run(conn, &meeting_id, &run_id).unwrap();
    Fixture { meeting_id, mic, system, run_id }
}

fn segment(start_ms: u64, text: &str) -> NewSegment {
    NewSegment {
        start_ms,
        end_ms: start_ms + 1_000,
        text: text.to_string(),
        lang: Some("nl".to_string()),
        no_speech_prob: Some(0.01),
        avg_logprob: Some(-0.2),
        suppressed_reason: None,
    }
}

/// Plans one window on `track_id` and completes it with `segments`.
fn decode(
    conn: &Connection,
    run_id: &str,
    track_id: &str,
    seq: u32,
    segments: &[NewSegment],
) -> Vec<String> {
    let window = NewWindow {
        track_id: track_id.to_string(),
        seq,
        start_ms: seq as u64 * 28_000,
        end_ms: (seq as u64 + 1) * 28_000,
    };
    let ids = insert_windows(conn, run_id, &[window]).unwrap();
    complete_window(conn, &ids[0], Some("nl"), segments).unwrap()
}

fn chunk(track_id: &str, seq: u32, start_ms: u64) -> ChunkRecord {
    ChunkRecord {
        id: format!("{track_id}-chunk-{seq}"),
        track_id: track_id.to_string(),
        seq,
        path: format!("m/{track_id}/{seq}.pcm"),
        status: ChunkStatus::Open,
        anchor_host_ns: 1_000 + start_ms * 1_000_000,
        start_ms,
        n_frames: 0,
    }
}

fn transcribe_job(meeting_id: &str, priority: i64) -> NewJob {
    NewJob {
        kind: JobKind::Transcribe,
        meeting_id: meeting_id.to_string(),
        run_id: None,
        priority,
        payload_json: None,
    }
}

fn summary_item(text: &str, sources: &[&String]) -> NewSummaryItem {
    NewSummaryItem {
        kind: SummaryItemKind::Action,
        text: text.to_string(),
        owner: None,
        owner_participant_id: None,
        due_date: None,
        source_segment_ids: sources.iter().map(|s| s.to_string()).collect(),
    }
}

// --- Meetings ----------------------------------------------------------------

#[test]
fn deleting_a_meeting_cascades_to_every_table() {
    let conn = memory_db();
    let f = fixture(&conn);
    let keep = fixture(&conn);

    insert_chunk(&conn, &chunk(&f.mic, 0, 0)).unwrap();
    let segments = decode(&conn, &f.run_id, &f.mic, 0, &[segment(0, "Goedemorgen")]);
    set_segment_text(&conn, &segments[0], Some("Goedemorgen allemaal")).unwrap();
    let speaker = add_speaker(&conn, &f.meeting_id, Some(&f.system), "Speaker 2", SpeakerSource::Diarization)
        .unwrap();
    add_speaker_turns(&conn, &speaker.id, &f.system, &[(0, 1_500)]).unwrap();
    assign_segment_speaker(&conn, &segments[0], &speaker.id, Some(0.9)).unwrap();
    let anna = add_participant(
        &conn,
        &NewParticipant {
            meeting_id: f.meeting_id.clone(),
            name: Some("Anna".to_string()),
            email: Some("anna@example.com".to_string()),
            source: ParticipantSource::Manual,
        },
    )
    .unwrap();
    assign_speaker(&conn, &speaker.id, &anna.id, AssignmentSource::Manual).unwrap();
    let summary_id = insert_summary(
        &conn,
        &NewSummary {
            meeting_id: f.meeting_id.clone(),
            run_id: f.run_id.clone(),
            provider: "openrouter".to_string(),
            model: "some/model".to_string(),
        },
    )
    .unwrap();
    complete_summary(&conn, &summary_id, "Overview", &[summary_item("Do it", &[&segments[0]])])
        .unwrap();
    insert_job(&conn, &transcribe_job(&f.meeting_id, 0)).unwrap();

    assert!(delete_meeting(&conn, &f.meeting_id).unwrap());
    assert!(!delete_meeting(&conn, &f.meeting_id).unwrap(), "second delete finds nothing");

    // Only the other meeting's rows are left: its two tracks, two speakers and run.
    assert_eq!(count(&conn, "meetings"), 1);
    assert_eq!(count(&conn, "meeting_tracks"), 2);
    assert_eq!(count(&conn, "speakers"), 2);
    assert_eq!(count(&conn, "transcript_runs"), 1);
    for table in [
        "meeting_audio_chunks",
        "transcript_windows",
        "transcript_segments",
        "segment_edits",
        "speaker_turns",
        "segment_speakers",
        "participants",
        "speaker_assignments",
        "summaries",
        "summary_items",
        "summary_item_sources",
        "jobs",
    ] {
        assert_eq!(count(&conn, table), 0, "{table} should be empty");
    }
    assert_eq!(count(&conn, "people"), 1, "people are shared between meetings");
    assert!(get_meeting(&conn, &f.meeting_id).unwrap().is_none());
    assert!(get_meeting(&conn, &keep.meeting_id).unwrap().is_some());
}

#[test]
fn meetings_list_newest_first_with_job_audio_and_summary_state() {
    let conn = memory_db();
    let older = fixture(&conn);
    let newer = fixture(&conn);
    conn.execute(
        "UPDATE meetings SET started_at = '2026-09-16T09:00:00+00:00' WHERE id = ?1",
        params![older.meeting_id],
    )
    .unwrap();

    insert_chunk(&conn, &chunk(&older.mic, 0, 0)).unwrap();
    close_chunk(&conn, &format!("{}-chunk-0", older.mic), 16_000 * 60).unwrap();
    finish_meeting(&conn, &older.meeting_id, "2026-09-16T09:01:00+00:00", 60_000).unwrap();
    set_meeting_status(&conn, &older.meeting_id, MeetingStatus::Queued, None).unwrap();
    set_echo_risk(&conn, &older.meeting_id, true).unwrap();
    let job = insert_job(&conn, &transcribe_job(&older.meeting_id, 0)).unwrap();

    let list = list_meetings(&conn).unwrap();
    assert_eq!(
        list.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec![newer.meeting_id.as_str(), older.meeting_id.as_str()]
    );
    let item = &list[1];
    assert_eq!(item.status, MeetingStatus::Queued);
    assert_eq!(item.duration_ms, 60_000);
    assert_eq!(item.ended_at.as_deref(), Some("2026-09-16T09:01:00+00:00"));
    assert!(item.echo_risk && item.has_audio && !item.has_summary);
    assert_eq!(item.job.as_ref().map(|j| j.job_id.as_str()), Some(job.id.as_str()));
    assert!(!list[0].has_audio && list[0].job.is_none());

    let detail = get_meeting(&conn, &older.meeting_id).unwrap().unwrap();
    assert_eq!(detail.model.as_deref(), Some("whisper-small-q5"));
    assert_eq!(detail.active_run_id.as_deref(), Some(older.run_id.as_str()));
    assert_eq!(detail.audio_bytes, 16_000 * 60 * 2);
    assert_eq!(detail.tracks.len(), 2);
    assert_eq!(detail.tracks[0].kind, TrackKind::Mic);
    assert_eq!(detail.tracks[0].duration_ms, 60_000);
    assert!(detail.tracks[0].has_audio && !detail.tracks[1].has_audio);
    assert_eq!(detail.runs.len(), 1);
    assert_eq!(
        detail.speakers.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(),
        vec!["Me", "Them"]
    );

    let waiting = list_meetings_with_status(&conn, &[MeetingStatus::Queued, MeetingStatus::Paused]).unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].id, older.meeting_id);
    assert!(list_meetings_with_status(&conn, &[]).unwrap().is_empty());
}

#[test]
fn rename_trims_and_rejects_empty_titles_and_unknown_meetings() {
    let conn = memory_db();
    let id = new_meeting(&conn, "  Standup ");
    assert_eq!(get_meeting(&conn, &id).unwrap().unwrap().meeting.title, "Standup");

    rename_meeting(&conn, &id, "  Weekly sync  ").unwrap();
    assert_eq!(get_meeting(&conn, &id).unwrap().unwrap().meeting.title, "Weekly sync");

    assert!(rename_meeting(&conn, &id, "   ").is_err());
    assert!(rename_meeting(&conn, "nope", "Title").unwrap_err().contains("not found"));
    assert_eq!(get_meeting(&conn, &id).unwrap().unwrap().meeting.title, "Weekly sync");
}

#[test]
fn a_failed_status_carries_its_error_and_the_next_status_clears_it() {
    let conn = memory_db();
    let id = new_meeting(&conn, "Standup");
    set_meeting_status(&conn, &id, MeetingStatus::Failed, Some("model missing")).unwrap();
    let detail = get_meeting(&conn, &id).unwrap().unwrap();
    assert_eq!(detail.meeting.status, MeetingStatus::Failed);
    assert_eq!(detail.error.as_deref(), Some("model missing"));

    set_meeting_status(&conn, &id, MeetingStatus::Queued, None).unwrap();
    assert!(get_meeting(&conn, &id).unwrap().unwrap().error.is_none());
    assert!(set_meeting_status(&conn, "nope", MeetingStatus::Ready, None).is_err());
}

// --- Tracks and chunks -------------------------------------------------------

#[test]
fn chunks_open_close_recover_and_delete() {
    let conn = memory_db();
    let f = fixture(&conn);

    // Inserted out of order; listed by seq.
    insert_chunk(&conn, &chunk(&f.mic, 1, 60_000)).unwrap();
    insert_chunk(&conn, &chunk(&f.mic, 0, 0)).unwrap();
    insert_chunk(&conn, &chunk(&f.system, 0, 0)).unwrap();
    assert!(insert_chunk(&conn, &chunk(&f.mic, 0, 0)).is_err(), "one row per chunk");

    let first = format!("{}-chunk-0", f.mic);
    close_chunk(&conn, &first, 960_000).unwrap();
    assert!(close_chunk(&conn, &first, 1).unwrap_err().contains("not open"));
    assert!(close_chunk(&conn, "nope", 1).is_err());

    let listed = list_chunks(&conn, &f.mic).unwrap();
    assert_eq!(listed.iter().map(|c| c.seq).collect::<Vec<_>>(), vec![0, 1]);
    assert_eq!(listed[0].status, ChunkStatus::Closed);
    assert_eq!(listed[0].n_frames, 960_000);
    assert_eq!(listed[1], chunk(&f.mic, 1, 60_000), "an open chunk reads back as written");

    // What a crash leaves behind: the two chunks that were still open.
    let open = list_open_chunks(&conn).unwrap();
    assert_eq!(open.len(), 2);
    for c in &open {
        assert_eq!(track_meeting_id(&conn, &c.track_id).unwrap().as_deref(), Some(f.meeting_id.as_str()));
        mark_chunk_recovered(&conn, &c.id, 8_000).unwrap();
    }
    assert!(list_open_chunks(&conn).unwrap().is_empty());
    assert_eq!(list_chunks(&conn, &f.mic).unwrap()[1].status, ChunkStatus::Recovered);
    // 60 s in, plus 8 000 frames at 16 kHz.
    assert_eq!(list_tracks(&conn, &f.meeting_id).unwrap()[0].duration_ms, 60_500);

    mark_audio_deleted(&conn, &f.meeting_id).unwrap();
    let detail = get_meeting(&conn, &f.meeting_id).unwrap().unwrap();
    assert!(!detail.meeting.has_audio);
    assert_eq!(detail.audio_bytes, 0);
    assert!(detail.tracks.iter().all(|t| !t.has_audio));
    let after = list_chunks(&conn, &f.mic).unwrap();
    assert!(after.iter().all(|c| c.status == ChunkStatus::Deleted));
    assert_eq!(after[0].n_frames, 960_000, "the rows keep the timeline");
    assert!(mark_audio_deleted(&conn, "nope").is_err());
}

#[test]
fn track_device_and_overflow_updates() {
    let conn = memory_db();
    let f = fixture(&conn);
    assert!(
        insert_track(
            &conn,
            &NewTrack { meeting_id: f.meeting_id.clone(), kind: TrackKind::Mic, device_name: None, format: None }
        )
        .is_err(),
        "one track per kind"
    );

    add_track_overflow(&conn, &f.mic, 100).unwrap();
    add_track_overflow(&conn, &f.mic, 28).unwrap();
    set_track_device(&conn, &f.mic, Some("AirPods Pro"), Some(SourceFormat { sample_rate: 24_000, channels: 1 }))
        .unwrap();
    let mic = &list_tracks(&conn, &f.meeting_id).unwrap()[0];
    assert_eq!(mic.overflow_frames, 128);
    assert_eq!(mic.device_name.as_deref(), Some("AirPods Pro"));
    assert!(add_track_overflow(&conn, "nope", 1).is_err());
}

// --- Runs, windows, segments -----------------------------------------------------

#[test]
fn a_meeting_has_exactly_one_active_run() {
    let conn = memory_db();
    let f = fixture(&conn);
    let second = new_run(&conn, &f.meeting_id);
    let other = fixture(&conn);

    decode(&conn, &f.run_id, &f.mic, 0, &[segment(0, "eerste run")]);
    decode(&conn, &second, &f.mic, 0, &[segment(0, "tweede run")]);

    let shown = |conn: &Connection| {
        list_segments(conn, &f.meeting_id, None)
            .unwrap()
            .into_iter()
            .map(|s| s.text)
            .collect::<Vec<_>>()
    };
    assert_eq!(shown(&conn), vec!["eerste run"]);

    set_active_run(&conn, &f.meeting_id, &second).unwrap();
    assert_eq!(shown(&conn), vec!["tweede run"]);
    let active: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM transcript_runs r JOIN meetings m ON m.active_run_id = r.id
              WHERE m.id = ?1",
            params![f.meeting_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active, 1);
    // The old run is kept and still readable by id.
    assert_eq!(list_segments(&conn, &f.meeting_id, Some(&f.run_id)).unwrap()[0].text, "eerste run");
    assert_eq!(list_runs(&conn, &f.meeting_id).unwrap().len(), 2);

    // A run of another meeting is refused and changes nothing.
    assert!(set_active_run(&conn, &f.meeting_id, &other.run_id).is_err());
    assert!(set_active_run(&conn, &f.meeting_id, "nope").is_err());
    assert_eq!(
        get_meeting(&conn, &f.meeting_id).unwrap().unwrap().active_run_id.as_deref(),
        Some(second.as_str())
    );
    assert!(list_segments(&conn, &f.meeting_id, Some(&other.run_id)).unwrap().is_empty());
}

#[test]
fn a_meeting_without_an_active_run_has_no_segments() {
    let conn = memory_db();
    let meeting_id = new_meeting(&conn, "Standup");
    let mic = new_track(&conn, &meeting_id, TrackKind::Mic);
    let run_id = new_run(&conn, &meeting_id);
    decode(&conn, &run_id, &mic, 0, &[segment(0, "hallo")]);

    assert!(list_segments(&conn, &meeting_id, None).unwrap().is_empty());
    assert_eq!(count_segments(&conn, &meeting_id, None).unwrap(), 0);
    assert_eq!(list_segments(&conn, &meeting_id, Some(&run_id)).unwrap().len(), 1);
}

#[test]
fn run_status_stamps_started_and_finished() {
    let conn = memory_db();
    let f = fixture(&conn);
    let run = get_run(&conn, &f.run_id).unwrap().unwrap();
    assert_eq!(run.status, RunStatus::Queued);
    assert_eq!(run.language, MeetingLanguage::Nl);
    assert_eq!(get_run_params(&conn, &f.run_id).unwrap().as_deref(), Some("{}"));

    set_run_status(&conn, &f.run_id, RunStatus::Running, None).unwrap();
    assert!(get_run(&conn, &f.run_id).unwrap().unwrap().finished_at.is_none());
    set_run_status(&conn, &f.run_id, RunStatus::Failed, Some("boom")).unwrap();
    let run = get_run(&conn, &f.run_id).unwrap().unwrap();
    assert_eq!(run.error.as_deref(), Some("boom"));
    assert!(run.finished_at.is_some());
    assert!(set_run_status(&conn, "nope", RunStatus::Done, None).is_err());
}

#[test]
fn a_failing_segment_insert_leaves_the_window_pending() {
    let conn = memory_db();
    let f = fixture(&conn);
    let ids = insert_windows(
        &conn,
        &f.run_id,
        &[NewWindow { track_id: f.mic.clone(), seq: 0, start_ms: 0, end_ms: 28_000 }],
    )
    .unwrap();
    let window_id = &ids[0];

    // A real failure from inside SQLite, on the second of two inserts.
    conn.execute_batch(
        "CREATE TEMP TRIGGER fail_segment BEFORE INSERT ON transcript_segments
           WHEN NEW.text = 'boom'
         BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
    )
    .unwrap();
    let error = complete_window(&conn, window_id, Some("nl"), &[segment(0, "prima"), segment(1_000, "boom")])
        .unwrap_err();
    assert!(error.contains("disk full"), "{error}");

    let window = &list_windows(&conn, &f.run_id).unwrap()[0];
    assert_eq!(window.status, WindowStatus::Pending);
    assert_eq!(window.attempts, 0);
    assert!(window.language.is_none());
    assert_eq!(count(&conn, "transcript_segments"), 0, "the first segment is rolled back too");
    assert!(conn.is_autocommit(), "no transaction is left open");
    assert_eq!(
        next_pending_window(&conn, &f.run_id).unwrap().map(|w| w.id),
        Some(window_id.clone()),
        "the window is decoded again on resume"
    );

    // The retry goes through, and only once.
    conn.execute_batch("DROP TRIGGER fail_segment").unwrap();
    let segments = complete_window(&conn, window_id, Some("nl"), &[segment(0, "prima"), segment(1_000, "ook goed")])
        .unwrap();
    assert_eq!(segments.len(), 2);
    let window = &list_windows(&conn, &f.run_id).unwrap()[0];
    assert_eq!((window.status, window.attempts), (WindowStatus::Done, 1));
    assert_eq!(window.language.as_deref(), Some("nl"));
    assert!(complete_window(&conn, window_id, Some("nl"), &[segment(0, "dubbel")]).is_err());
    assert_eq!(count(&conn, "transcript_segments"), 2);
    assert!(complete_window(&conn, "nope", None, &[]).is_err());
}

#[test]
fn windows_are_planned_atomically_and_decoded_in_timeline_order() {
    let conn = memory_db();
    let f = fixture(&conn);
    let window = |track: &str, seq: u32, start_ms: u64| NewWindow {
        track_id: track.to_string(),
        seq,
        start_ms,
        end_ms: start_ms + 20_000,
    };

    // The duplicate (track, seq) fails the whole plan.
    let bad = [window(&f.mic, 0, 0), window(&f.mic, 0, 30_000)];
    assert!(insert_windows(&conn, &f.run_id, &bad).is_err());
    assert_eq!(count(&conn, "transcript_windows"), 0);
    assert!(window_counts(&conn, &f.run_id).unwrap().is_settled(), "no speech, nothing to do");

    let plan = [window(&f.system, 0, 5_000), window(&f.mic, 0, 0), window(&f.mic, 1, 40_000)];
    let ids = insert_windows(&conn, &f.run_id, &plan).unwrap();
    let pending = list_pending_windows(&conn, &f.run_id).unwrap();
    assert_eq!(pending.iter().map(|w| w.start_ms).collect::<Vec<_>>(), vec![0, 5_000, 40_000]);
    assert_eq!(pending[1].track_kind, TrackKind::System);

    // Detection is resumable: the language is read back from a done window.
    assert!(track_language(&conn, &f.run_id, &f.mic).unwrap().is_none());
    complete_window(&conn, &ids[1], Some("nl"), &[]).unwrap();
    assert_eq!(track_language(&conn, &f.run_id, &f.mic).unwrap().as_deref(), Some("nl"));
    assert!(track_language(&conn, &f.run_id, &f.system).unwrap().is_none());

    // One retry, then the window fails for good.
    assert_eq!(fail_window(&conn, &ids[0], "decode error", 2).unwrap(), WindowStatus::Pending);
    assert_eq!(next_pending_window(&conn, &f.run_id).unwrap().unwrap().id, ids[0]);
    assert_eq!(fail_window(&conn, &ids[0], "decode error", 2).unwrap(), WindowStatus::Failed);
    assert!(fail_window(&conn, &ids[0], "again", 2).is_err());

    let counts = window_counts(&conn, &f.run_id).unwrap();
    assert_eq!(counts, WindowCounts { done: 1, failed: 1, total: 3 });
    assert!(!counts.is_settled());
    assert_eq!(next_pending_window(&conn, &f.run_id).unwrap().unwrap().id, ids[2]);
}

#[test]
fn segments_interleave_tracks_and_respect_the_edit_layer() {
    let conn = memory_db();
    let f = fixture(&conn);
    let mut flagged = segment(4_000, "Ondertiteling door de NPO");
    flagged.suppressed_reason = Some(SuppressedReason::PromptEcho);
    let mic = decode(&conn, &f.run_id, &f.mic, 0, &[segment(0, "Goedemorgen"), flagged]);
    let system = decode(&conn, &f.run_id, &f.system, 0, &[segment(2_000, "Hoi Joost")]);

    let listed = list_segments(&conn, &f.meeting_id, None).unwrap();
    assert_eq!(
        listed.iter().map(|s| (s.start_ms, s.speaker_label.as_str())).collect::<Vec<_>>(),
        vec![(0, "Me"), (2_000, "Them"), (4_000, "Me")]
    );
    assert_eq!(listed[1].track_kind, TrackKind::System);
    assert_eq!(listed[0].meeting_id, f.meeting_id);
    assert!(listed[0].speaker_id.is_some());
    assert!(listed[0].original_text.is_none() && !listed[0].hidden);
    // Flagged by the pipeline: hidden by default, still listed.
    assert_eq!(listed[2].suppressed_reason, Some(SuppressedReason::PromptEcho));
    assert!(listed[2].hidden);
    assert_eq!(count_segments(&conn, &f.meeting_id, None).unwrap(), 2);

    // COALESCE(edit.text, seg.text): the edit shows, the raw row is untouched.
    let edited = set_segment_text(&conn, &mic[0], Some("Goedemorgen allemaal")).unwrap();
    assert_eq!(edited.text, "Goedemorgen allemaal");
    assert_eq!(edited.original_text.as_deref(), Some("Goedemorgen"));
    let raw: String = conn
        .query_row("SELECT text FROM transcript_segments WHERE id = ?1", params![mic[0]], |r| r.get(0))
        .unwrap();
    assert_eq!(raw, "Goedemorgen");
    assert_eq!(list_segments(&conn, &f.meeting_id, None).unwrap()[0].text, "Goedemorgen allemaal");

    // The user's hidden choice wins both ways and survives a text change.
    assert!(set_segment_hidden(&conn, &mic[0], true).unwrap().hidden);
    assert!(!set_segment_hidden(&conn, &mic[1], false).unwrap().hidden);
    let cleared = set_segment_text(&conn, &mic[0], None).unwrap();
    assert_eq!(cleared.text, "Goedemorgen");
    assert!(cleared.original_text.is_none() && cleared.hidden);
    assert_eq!(count(&conn, "segment_edits"), 2);

    // Clearing the layer falls back to the pipeline's flag.
    assert!(clear_segment_edit(&conn, &mic[1]).unwrap().hidden);
    assert!(!clear_segment_edit(&conn, &mic[0]).unwrap().hidden);
    assert_eq!(count(&conn, "segment_edits"), 0);

    // Typing the decoded text back is not an edit.
    set_segment_text(&conn, &system[0], Some("Hoi")).unwrap();
    assert!(set_segment_text(&conn, &system[0], Some("Hoi Joost")).unwrap().original_text.is_none());
    assert_eq!(count(&conn, "segment_edits"), 0);

    assert!(set_segment_text(&conn, "nope", Some("x")).unwrap_err().contains("not found"));
    assert!(set_segment_hidden(&conn, "nope", true).is_err());
}

#[test]
fn suppressed_reason_is_the_only_mutable_segment_column() {
    let conn = memory_db();
    let f = fixture(&conn);
    let mut already = segment(2_000, "stilte");
    already.suppressed_reason = Some(SuppressedReason::NoSpeech);
    let ids = decode(&conn, &f.run_id, &f.mic, 0, &[segment(0, "Hoi Joost"), already, segment(4_000, "ja")]);

    let flagged = flag_segments(&conn, &[ids[0].clone(), ids[1].clone(), "nope".to_string()], SuppressedReason::Echo)
        .unwrap();
    assert_eq!(flagged, 1, "an earlier finding is kept, an unknown id is skipped");
    let listed = list_segments(&conn, &f.meeting_id, None).unwrap();
    assert_eq!(listed[0].suppressed_reason, Some(SuppressedReason::Echo));
    assert_eq!(listed[1].suppressed_reason, Some(SuppressedReason::NoSpeech));
    assert!(listed[0].hidden && !listed[2].hidden);
    assert_eq!(listed[0].text, "Hoi Joost");

    set_suppressed_reason(&conn, &ids[0], None).unwrap();
    assert!(!get_segment(&conn, &ids[0]).unwrap().unwrap().hidden);
    assert!(set_suppressed_reason(&conn, "nope", None).is_err());
}

// --- Speakers ------------------------------------------------------------------

#[test]
fn seeding_is_idempotent_and_follows_the_tracks() {
    let conn = memory_db();
    let meeting_id = new_meeting(&conn, "Mic only");
    let mic = new_track(&conn, &meeting_id, TrackKind::Mic);

    let seeded = seed_track_speakers(&conn, &meeting_id).unwrap();
    assert_eq!(seeded.len(), 1);
    assert_eq!(seeded[0].label, "Me");
    assert_eq!(seeded[0].source, SpeakerSource::Track);
    assert_eq!(seeded[0].track_id.as_deref(), Some(mic.as_str()));

    // The system track shows up later (permission granted mid-meeting).
    new_track(&conn, &meeting_id, TrackKind::System);
    let labels = |speakers: Vec<crate::meetings::types::Speaker>| {
        speakers.into_iter().map(|s| s.label).collect::<Vec<_>>()
    };
    assert_eq!(labels(seed_track_speakers(&conn, &meeting_id).unwrap()), vec!["Me", "Them"]);
    assert_eq!(labels(seed_track_speakers(&conn, &meeting_id).unwrap()), vec!["Me", "Them"]);
    assert_eq!(count(&conn, "speakers"), 2);
    assert_eq!(count(&conn, "segment_speakers"), 0, "track labels need no per-segment rows");
}

#[test]
fn an_unseeded_meeting_still_labels_segments_by_track() {
    let conn = memory_db();
    let meeting_id = new_meeting(&conn, "Standup");
    let system = new_track(&conn, &meeting_id, TrackKind::System);
    let run_id = new_run(&conn, &meeting_id);
    decode(&conn, &run_id, &system, 0, &[segment(0, "hallo")]);

    let listed = list_segments(&conn, &meeting_id, Some(&run_id)).unwrap();
    assert_eq!(listed[0].speaker_label, "Them");
    assert!(listed[0].speaker_id.is_none());
}

#[test]
fn merging_speakers_moves_segments_turns_and_assignments() {
    let conn = memory_db();
    let f = fixture(&conn);
    let ids = decode(
        &conn,
        &f.run_id,
        &f.system,
        0,
        &[segment(0, "een"), segment(2_000, "twee"), segment(4_000, "drie")],
    );
    let two = add_speaker(&conn, &f.meeting_id, Some(&f.system), "Speaker 2", SpeakerSource::Diarization).unwrap();
    let three = add_speaker(&conn, &f.meeting_id, Some(&f.system), " Speaker 3 ", SpeakerSource::Diarization).unwrap();
    assert_eq!(three.label, "Speaker 3");

    assign_segment_speaker(&conn, &ids[0], &two.id, Some(0.8)).unwrap();
    assign_segment_speaker(&conn, &ids[1], &three.id, Some(0.7)).unwrap();
    add_speaker_turns(&conn, &two.id, &f.system, &[(0, 1_000)]).unwrap();
    add_speaker_turns(&conn, &three.id, &f.system, &[(2_000, 3_000), (6_000, 7_000)]).unwrap();
    let anna = add_participant(
        &conn,
        &NewParticipant {
            meeting_id: f.meeting_id.clone(),
            name: Some("Anna".to_string()),
            email: None,
            source: ParticipantSource::Manual,
        },
    )
    .unwrap();
    assign_speaker(&conn, &three.id, &anna.id, AssignmentSource::Manual).unwrap();

    merge_speakers(&conn, &three.id, &two.id).unwrap();

    assert!(get_speaker(&conn, &three.id).unwrap().is_none());
    let listed = list_segments(&conn, &f.meeting_id, None).unwrap();
    assert_eq!(listed[0].speaker_id.as_deref(), Some(two.id.as_str()));
    assert_eq!(listed[1].speaker_id.as_deref(), Some(two.id.as_str()));
    // The assignment came along, so both segments now carry Anna's name.
    assert_eq!(listed[0].speaker_label, "Anna");
    assert_eq!(listed[1].speaker_label, "Anna");
    assert_eq!(listed[2].speaker_label, "Them", "untouched segments stay with the track speaker");
    let turns = list_speaker_turns(&conn, &f.meeting_id).unwrap();
    assert_eq!(turns.len(), 3);
    assert!(turns.iter().all(|t| t.speaker_id == two.id));
    assert_eq!(
        list_speaker_assignments(&conn, &f.meeting_id).unwrap(),
        vec![SpeakerAssignment {
            speaker_id: two.id.clone(),
            participant_id: anna.id.clone(),
            source: AssignmentSource::Manual,
        }]
    );

    // Refused: itself, an unknown speaker, another meeting, a track speaker.
    let other = fixture(&conn);
    let stranger = add_speaker(&conn, &other.meeting_id, None, "Speaker 2", SpeakerSource::Manual).unwrap();
    let them = list_speakers(&conn, &f.meeting_id).unwrap().remove(1);
    assert_eq!(them.source, SpeakerSource::Track);
    assert!(merge_speakers(&conn, &two.id, &two.id).is_err());
    assert!(merge_speakers(&conn, "nope", &two.id).is_err());
    assert!(merge_speakers(&conn, &stranger.id, &two.id).is_err());
    assert!(merge_speakers(&conn, &them.id, &two.id).is_err());
    assert_eq!(list_speakers(&conn, &f.meeting_id).unwrap().len(), 3);
}

#[test]
fn merging_keeps_the_targets_own_assignment_and_shared_segments() {
    let conn = memory_db();
    let f = fixture(&conn);
    let ids = decode(&conn, &f.run_id, &f.system, 0, &[segment(0, "een")]);
    let two = add_speaker(&conn, &f.meeting_id, None, "Speaker 2", SpeakerSource::Diarization).unwrap();
    let three = add_speaker(&conn, &f.meeting_id, None, "Speaker 3", SpeakerSource::Diarization).unwrap();
    let participant = |name: &str| {
        add_participant(
            &conn,
            &NewParticipant {
                meeting_id: f.meeting_id.clone(),
                name: Some(name.to_string()),
                email: None,
                source: ParticipantSource::Manual,
            },
        )
        .unwrap()
    };
    assign_speaker(&conn, &two.id, &participant("Anna").id, AssignmentSource::Manual).unwrap();
    assign_speaker(&conn, &three.id, &participant("Bram").id, AssignmentSource::Manual).unwrap();
    // Diarization was unsure: the segment has a row for each of them.
    conn.execute(
        "INSERT INTO segment_speakers (segment_id, speaker_id, confidence) VALUES (?1, ?2, 0.6), (?1, ?3, 0.4)",
        params![ids[0], two.id, three.id],
    )
    .unwrap();

    merge_speakers(&conn, &three.id, &two.id).unwrap();

    assert_eq!(count(&conn, "segment_speakers"), 1);
    assert_eq!(count(&conn, "speaker_assignments"), 1);
    assert_eq!(list_segments(&conn, &f.meeting_id, None).unwrap()[0].speaker_label, "Anna");
}

#[test]
fn a_segment_can_be_reassigned_by_hand() {
    let conn = memory_db();
    let f = fixture(&conn);
    let ids = decode(&conn, &f.run_id, &f.system, 0, &[segment(0, "een")]);
    let two = add_speaker(&conn, &f.meeting_id, Some(&f.system), "Speaker 2", SpeakerSource::Diarization).unwrap();
    let three = add_speaker(&conn, &f.meeting_id, Some(&f.system), "Speaker 3", SpeakerSource::Manual).unwrap();

    assert_eq!(assign_segment_speaker(&conn, &ids[0], &two.id, Some(0.9)).unwrap().speaker_label, "Speaker 2");
    let moved = assign_segment_speaker(&conn, &ids[0], &three.id, None).unwrap();
    assert_eq!(moved.speaker_label, "Speaker 3");
    assert_eq!(count(&conn, "segment_speakers"), 1, "reassigning replaces");

    rename_speaker(&conn, &three.id, "  Bram ").unwrap();
    assert_eq!(get_segment(&conn, &ids[0]).unwrap().unwrap().speaker_label, "Bram");
    assert!(rename_speaker(&conn, &three.id, " ").is_err());

    clear_segment_speaker(&conn, &ids[0]).unwrap();
    assert_eq!(get_segment(&conn, &ids[0]).unwrap().unwrap().speaker_label, "Them");

    let other = fixture(&conn);
    let stranger = add_speaker(&conn, &other.meeting_id, None, "Speaker 2", SpeakerSource::Manual).unwrap();
    assert!(assign_segment_speaker(&conn, &ids[0], &stranger.id, None).is_err());
    assert!(add_speaker(&conn, &f.meeting_id, Some(&other.mic), "Speaker 4", SpeakerSource::Manual).is_err());
    assert!(add_speaker_turns(&conn, &two.id, &f.system, &[(2_000, 1_000)]).is_err());
}

// --- People and participants -----------------------------------------------------

#[test]
fn people_are_upserted_by_email_case_insensitively() {
    let conn = memory_db();
    let first = upsert_person(&conn, "Anna@Example.com", Some("Anna")).unwrap();
    let again = upsert_person(&conn, "  anna@example.COM ", None).unwrap();
    assert_eq!(first.id, again.id);
    assert_eq!(again.email, "anna@example.com");
    assert_eq!(again.display_name.as_deref(), Some("Anna"), "no name keeps the old one");

    let renamed = upsert_person(&conn, "ANNA@EXAMPLE.COM", Some("Anna de Vries")).unwrap();
    assert_eq!(renamed.id, first.id);
    assert_eq!(renamed.display_name.as_deref(), Some("Anna de Vries"));
    assert_eq!(count(&conn, "people"), 1);

    upsert_person(&conn, "bram@example.com", Some("  ")).unwrap();
    assert_eq!(count(&conn, "people"), 2);
    assert!(upsert_person(&conn, "   ", Some("Nobody")).is_err());
}

#[test]
fn participants_link_to_people_and_name_their_speaker() {
    let conn = memory_db();
    let f = fixture(&conn);
    let ids = decode(&conn, &f.run_id, &f.system, 0, &[segment(0, "hallo")]);
    let attendee = |name: Option<&str>, email: Option<&str>| NewParticipant {
        meeting_id: f.meeting_id.clone(),
        name: name.map(str::to_string),
        email: email.map(str::to_string),
        source: ParticipantSource::Calendar,
    };

    let anna = add_participant(&conn, &attendee(None, Some("Anna@Example.com"))).unwrap();
    assert!(anna.person_id.is_some());
    assert_eq!(anna.email.as_deref(), Some("anna@example.com"));
    // The same attendee again, now with a name: one row, name filled in.
    let again = add_participant(&conn, &attendee(Some("Anna"), Some("anna@example.com"))).unwrap();
    assert_eq!(again.id, anna.id);
    assert_eq!(again.name.as_deref(), Some("Anna"));
    let guest = add_participant(&conn, &attendee(Some("Gast"), None)).unwrap();
    assert!(guest.person_id.is_none());
    assert!(add_participant(&conn, &attendee(Some(" "), None)).is_err());
    assert_eq!(list_participants(&conn, &f.meeting_id).unwrap(), vec![again.clone(), guest.clone()]);

    // A person met in two meetings is one person.
    let other = fixture(&conn);
    let elsewhere = add_participant(
        &conn,
        &NewParticipant { meeting_id: other.meeting_id.clone(), ..attendee(None, Some("ANNA@example.com")) },
    )
    .unwrap();
    assert_eq!(elsewhere.person_id, anna.person_id);
    assert_ne!(elsewhere.id, anna.id);
    assert_eq!(count(&conn, "people"), 1);

    let them = list_speakers(&conn, &f.meeting_id).unwrap().remove(1);
    // A suggestion is stored but never applied to a label.
    assign_speaker(&conn, &them.id, &anna.id, AssignmentSource::Suggested).unwrap();
    assert_eq!(get_segment(&conn, &ids[0]).unwrap().unwrap().speaker_label, "Them");
    assert!(!list_speaker_assignments(&conn, &f.meeting_id).unwrap()[0].source.is_confirmed());
    // Confirmed, the participant's name wins over the speaker's label.
    assign_speaker(&conn, &them.id, &anna.id, AssignmentSource::Manual).unwrap();
    assert_eq!(get_segment(&conn, &ids[0]).unwrap().unwrap().speaker_label, "Anna");
    assert_eq!(list_speakers(&conn, &f.meeting_id).unwrap()[1].label, "Anna");
    // One assignment per speaker: a new one replaces the old.
    assign_speaker(&conn, &them.id, &guest.id, AssignmentSource::Manual).unwrap();
    assert_eq!(get_segment(&conn, &ids[0]).unwrap().unwrap().speaker_label, "Gast");
    assert_eq!(count(&conn, "speaker_assignments"), 1);

    assert!(assign_speaker(&conn, &them.id, &elsewhere.id, AssignmentSource::Manual).is_err());
    assert!(assign_speaker(&conn, "nope", &anna.id, AssignmentSource::Manual).is_err());

    // Removing the participant takes the assignment along; the person stays.
    assert!(remove_participant(&conn, &guest.id).unwrap());
    assert!(!remove_participant(&conn, &guest.id).unwrap());
    assert_eq!(get_segment(&conn, &ids[0]).unwrap().unwrap().speaker_label, "Them");
    assign_speaker(&conn, &them.id, &anna.id, AssignmentSource::Manual).unwrap();
    assert!(unassign_speaker(&conn, &them.id).unwrap());
    assert!(!unassign_speaker(&conn, &them.id).unwrap());
    assert_eq!(count(&conn, "people"), 1);
}

// --- Summaries -------------------------------------------------------------------

#[test]
fn a_summary_is_written_with_its_items_and_sources_or_not_at_all() {
    let conn = memory_db();
    let f = fixture(&conn);
    let ids = decode(&conn, &f.run_id, &f.mic, 0, &[segment(0, "een"), segment(2_000, "twee")]);
    let new_summary = NewSummary {
        meeting_id: f.meeting_id.clone(),
        run_id: f.run_id.clone(),
        provider: "openrouter".to_string(),
        model: "some/model".to_string(),
    };
    assert!(latest_summary(&conn, &f.meeting_id).unwrap().is_none());

    let first = insert_summary(&conn, &new_summary).unwrap();
    let pending = latest_summary(&conn, &f.meeting_id).unwrap().unwrap();
    assert_eq!((pending.status, pending.items.len()), (SummaryStatus::Pending, 0));
    assert!(!list_meetings(&conn).unwrap()[0].has_summary);

    // An item without a source, or citing a segment that does not exist,
    // rolls the whole completion back.
    let uncited = [summary_item("Goed", &[&ids[0]]), summary_item("Verzonnen", &[])];
    assert!(complete_summary(&conn, &first, "Overview", &uncited).is_err());
    let ghost = "nope".to_string();
    assert!(complete_summary(&conn, &first, "Overview", &[summary_item("Spook", &[&ghost])]).is_err());
    let still = get_summary(&conn, &first).unwrap().unwrap();
    assert_eq!(still.status, SummaryStatus::Pending);
    assert!(still.overview.is_none());
    assert_eq!(count(&conn, "summary_items"), 0);
    assert!(conn.is_autocommit());

    let mut action = summary_item("Stuur de offerte", &[&ids[1], &ids[0], &ids[1]]);
    action.owner = Some("Anna".to_string());
    action.due_date = Some("2026-09-24".to_string());
    let mut topic = summary_item("Planning", &[&ids[0]]);
    topic.kind = SummaryItemKind::Topic;
    complete_summary(&conn, &first, "We spraken over de planning.", &[action, topic]).unwrap();

    let done = latest_summary(&conn, &f.meeting_id).unwrap().unwrap();
    assert_eq!(done.status, SummaryStatus::Done);
    assert_eq!(done.overview.as_deref(), Some("We spraken over de planning."));
    assert_eq!(done.items.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(), vec!["Stuur de offerte", "Planning"]);
    assert_eq!(done.items[0].kind, SummaryItemKind::Action);
    assert_eq!(done.items[0].owner.as_deref(), Some("Anna"));
    assert_eq!(done.items[0].due_date.as_deref(), Some("2026-09-24"));
    assert_eq!(done.items[0].source_segment_ids, vec![ids[0].clone(), ids[1].clone()], "deduplicated, in transcript order");
    assert!(done.items[1].owner.is_none() && done.items[1].due_date.is_none());
    assert!(list_meetings(&conn).unwrap()[0].has_summary);

    // More items append after the existing ones.
    add_summary_items(&conn, &first, &[summary_item("Nog iets", &[&ids[0]])]).unwrap();
    assert_eq!(get_summary(&conn, &first).unwrap().unwrap().items[2].text, "Nog iets");

    // A failed retry does not hide the summary the user already had.
    let retry = insert_summary(&conn, &new_summary).unwrap();
    fail_summary(&conn, &retry, "HTTP 500").unwrap();
    assert_eq!(latest_summary(&conn, &f.meeting_id).unwrap().unwrap().id, first);
    assert_eq!(get_summary(&conn, &retry).unwrap().unwrap().error.as_deref(), Some("HTTP 500"));
    assert!(fail_summary(&conn, "nope", "x").is_err());
    assert!(complete_summary(&conn, "nope", "x", &[]).is_err());
}

// --- Jobs --------------------------------------------------------------------------

#[test]
fn jobs_are_claimed_by_priority_then_age() {
    let conn = memory_db();
    let f = fixture(&conn);
    let other = fixture(&conn);
    assert!(claim_next_job(&conn).unwrap().is_none());

    let old = insert_job(&conn, &transcribe_job(&f.meeting_id, 0)).unwrap();
    let new = insert_job(&conn, &transcribe_job(&other.meeting_id, 0)).unwrap();
    let urgent = insert_job(&conn, &NewJob { run_id: Some(f.run_id.clone()), ..transcribe_job(&f.meeting_id, 5) })
        .unwrap();
    assert_eq!((old.status, old.attempts), (JobStatus::Queued, 0));
    assert_eq!(old.payload_json, "{}");

    assert_eq!(next_queued_job(&conn).unwrap().unwrap().id, urgent.id, "peeking claims nothing");
    let first = claim_next_job(&conn).unwrap().unwrap();
    assert_eq!(first.id, urgent.id);
    assert_eq!((first.status, first.attempts), (JobStatus::Running, 1));
    assert!(first.started_at.is_some());
    assert_eq!(first.run_id.as_deref(), Some(f.run_id.as_str()));

    // A claimed job is gone from the queue: the next claim gets the next job.
    assert_eq!(claim_next_job(&conn).unwrap().unwrap().id, old.id);
    assert_eq!(claim_next_job(&conn).unwrap().unwrap().id, new.id);
    assert!(claim_next_job(&conn).unwrap().is_none());
    assert_eq!(count(&conn, "jobs"), 3);
}

#[test]
fn running_jobs_are_requeued_at_launch_and_keep_their_place() {
    let conn = memory_db();
    let f = fixture(&conn);
    let other = fixture(&conn);
    let first = insert_job(&conn, &transcribe_job(&f.meeting_id, 0)).unwrap();
    let second = insert_job(&conn, &transcribe_job(&other.meeting_id, 0)).unwrap();

    let claimed = claim_next_job(&conn).unwrap().unwrap();
    assert!(set_job_progress(&conn, &claimed.id, 3, 10).unwrap());
    let shown = job_progress(&conn, &f.meeting_id).unwrap().unwrap();
    assert_eq!((shown.status, shown.done, shown.total), (JobStatus::Running, 3, 10));

    // The app dies here. At the next launch:
    assert_eq!(requeue_running_jobs(&conn).unwrap(), 1);
    assert_eq!(requeue_running_jobs(&conn).unwrap(), 0);
    let requeued = get_job(&conn, &first.id).unwrap().unwrap();
    assert_eq!(requeued.status, JobStatus::Queued);
    assert!(requeued.started_at.is_none());
    assert_eq!((requeued.progress_done, requeued.progress_total), (3, 10), "progress survives");

    // Older than the job that was never started, so it is claimed first again.
    let again = claim_next_job(&conn).unwrap().unwrap();
    assert_eq!((again.id.as_str(), again.attempts), (first.id.as_str(), 2));
    assert!(finish_job(&conn, &again.id).unwrap());
    assert!(job_progress(&conn, &f.meeting_id).unwrap().is_none(), "a done job is not unfinished");
    assert_eq!(claim_next_job(&conn).unwrap().unwrap().id, second.id);
}

#[test]
fn the_worker_cannot_resurrect_a_cancelled_job() {
    let conn = memory_db();
    let f = fixture(&conn);
    let job = insert_job(&conn, &transcribe_job(&f.meeting_id, 0)).unwrap();
    assert!(!heartbeat_job(&conn, &job.id).unwrap(), "only a running job has a heartbeat");

    let job = claim_next_job(&conn).unwrap().unwrap();
    assert!(heartbeat_job(&conn, &job.id).unwrap());
    assert!(get_job(&conn, &job.id).unwrap().unwrap().updated_at >= job.updated_at);

    // Cancelled from the UI connection while a window is being decoded.
    assert_eq!(cancel_unfinished_jobs(&conn, &f.meeting_id).unwrap(), 1);
    assert!(!heartbeat_job(&conn, &job.id).unwrap());
    assert!(!set_job_progress(&conn, &job.id, 1, 2).unwrap());
    assert!(!finish_job(&conn, &job.id).unwrap());
    assert!(!fail_job(&conn, &job.id, "late").unwrap());
    let cancelled = get_job(&conn, &job.id).unwrap().unwrap();
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert!(cancelled.finished_at.is_some() && cancelled.error.is_none());

    // A failure is recorded, and a failed job can be put back by hand.
    let retry = insert_job(&conn, &transcribe_job(&f.meeting_id, 0)).unwrap();
    claim_next_job(&conn).unwrap().unwrap();
    assert!(fail_job(&conn, &retry.id, "model missing").unwrap());
    let failed = get_job(&conn, &retry.id).unwrap().unwrap();
    assert_eq!((failed.status, failed.error.as_deref()), (JobStatus::Failed, Some("model missing")));
    set_job_status(&conn, &retry.id, JobStatus::Queued, None).unwrap();
    let queued = get_job(&conn, &retry.id).unwrap().unwrap();
    assert!(queued.started_at.is_none() && queued.finished_at.is_none() && queued.error.is_none());
    assert!(set_job_status(&conn, "nope", JobStatus::Done, None).is_err());

    let listed = list_jobs(&conn, &f.meeting_id).unwrap();
    assert_eq!(listed.iter().map(|j| j.id.as_str()).collect::<Vec<_>>(), vec![retry.id.as_str(), job.id.as_str()]);
}

#[test]
fn the_ui_and_worker_connections_share_one_queue() {
    // A file database, like the real one: WAL, and one connection each for
    // the UI and the worker (`db::open_connection`).
    let dir = std::env::temp_dir().join(format!("ft-meetings-store-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let open = || {
        let conn = Connection::open(dir.join("test.db")).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.pragma_update(None, "busy_timeout", 5_000).unwrap();
        conn
    };
    let ui = open();
    migrate(&ui);
    let worker = open();

    let f = fixture(&ui);
    let first = insert_job(&ui, &transcribe_job(&f.meeting_id, 0)).unwrap();
    let second = insert_job(&ui, &transcribe_job(&f.meeting_id, 0)).unwrap();

    // Two claimers never get the same job.
    assert_eq!(claim_next_job(&worker).unwrap().unwrap().id, first.id);
    assert_eq!(claim_next_job(&ui).unwrap().unwrap().id, second.id);
    assert!(claim_next_job(&worker).unwrap().is_none());

    // The worker's window lands as a whole on the UI connection.
    let windows = insert_windows(
        &worker,
        &f.run_id,
        &[NewWindow { track_id: f.mic.clone(), seq: 0, start_ms: 0, end_ms: 28_000 }],
    )
    .unwrap();
    complete_window(&worker, &windows[0], Some("nl"), &[segment(0, "Goedemorgen")]).unwrap();
    assert_eq!(list_segments(&ui, &f.meeting_id, None).unwrap()[0].text, "Goedemorgen");
    assert_eq!(cancel_unfinished_jobs(&ui, &f.meeting_id).unwrap(), 2);
    assert!(!finish_job(&worker, &first.id).unwrap());

    drop((ui, worker));
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Transactions ----------------------------------------------------------------

#[test]
fn transactions_nest_and_roll_back_as_a_whole() {
    let conn = memory_db();

    // An inner store transaction that succeeded is undone with the outer one.
    let result: Result<(), String> = transaction(&conn, || {
        let meeting_id = new_meeting(&conn, "Standup");
        new_track(&conn, &meeting_id, TrackKind::Mic);
        seed_track_speakers(&conn, &meeting_id)?;
        assert_eq!(count(&conn, "speakers"), 1);
        Err("capture failed to start".to_string())
    });
    assert_eq!(result.unwrap_err(), "capture failed to start");
    assert!(conn.is_autocommit());
    assert_eq!(count(&conn, "meetings") + count(&conn, "meeting_tracks") + count(&conn, "speakers"), 0);

    // An inner failure the caller handles leaves the outer work intact.
    let meeting_id = transaction(&conn, || {
        let meeting_id = new_meeting(&conn, "Standup");
        assert!(mark_audio_deleted(&conn, "nope").is_err());
        Ok(meeting_id)
    })
    .unwrap();
    assert!(conn.is_autocommit());
    assert!(get_meeting(&conn, &meeting_id).unwrap().is_some());
}

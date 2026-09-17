// Test data builders for the Meetings UI. Imported by tests only.

import type {
  JobProgress,
  MeetingDetail,
  MeetingListItem,
  MeetingSummary,
  RecordingStatus,
  Segment,
} from "../../types/meetings";

export function makeMeeting(overrides: Partial<MeetingListItem> = {}): MeetingListItem {
  return {
    id: "m1",
    title: "Weekly sync",
    status: "ready",
    started_at: new Date().toISOString(),
    ended_at: null,
    duration_ms: 1_865_000,
    language: "auto",
    echo_risk: false,
    has_audio: true,
    has_summary: false,
    job: null,
    ...overrides,
  };
}

export function makeDetail(overrides: Partial<MeetingDetail> = {}): MeetingDetail {
  return {
    ...makeMeeting(),
    model: "whisper-small-q5",
    active_run_id: "r1",
    error: null,
    audio_bytes: 120_000_000,
    tracks: [],
    runs: [],
    speakers: [],
    ...overrides,
  };
}

export function makeSegment(overrides: Partial<Segment> = {}): Segment {
  return {
    id: "s1",
    meeting_id: "m1",
    run_id: "r1",
    track_id: "t-mic",
    track_kind: "mic",
    start_ms: 0,
    end_ms: 2000,
    text: "Hello there",
    original_text: null,
    lang: "en",
    speaker_id: "sp-me",
    speaker_label: "Me",
    suppressed_reason: null,
    hidden: false,
    ...overrides,
  };
}

export function makeJob(overrides: Partial<JobProgress> = {}): JobProgress {
  return {
    job_id: "j1",
    meeting_id: "m1",
    run_id: "r1",
    kind: "transcribe",
    status: "running",
    done: 0,
    total: 0,
    error: null,
    ...overrides,
  };
}

export function makeRecordingStatus(overrides: Partial<RecordingStatus> = {}): RecordingStatus {
  return {
    phase: "recording",
    meeting_id: "m-live",
    started_at: new Date().toISOString(),
    elapsed_ms: 0,
    tracks: ["mic", "system"],
    system_audio_silent: false,
    echo_risk: false,
    ...overrides,
  };
}

export function makeSummary(overrides: Partial<MeetingSummary> = {}): MeetingSummary {
  return {
    id: "sum1",
    meeting_id: "m1",
    run_id: "r1",
    provider: "openrouter",
    model: "openai/gpt-4o-mini",
    status: "done",
    overview: null,
    error: null,
    created_at: new Date().toISOString(),
    items: [],
    ...overrides,
  };
}

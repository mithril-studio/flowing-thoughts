// Mirrors src-tauri/src/meetings/types.rs and events.rs. Field names are
// snake_case on both sides, like the settings types. Keep the two in step.

export type TrackKind = "mic" | "system";

export type MeetingStatus =
  | "recording"
  | "paused"
  | "interrupted"
  | "queued"
  | "transcribing"
  | "ready"
  | "failed";

export type RunStatus = "queued" | "running" | "done" | "failed" | "cancelled";
export type JobKind = "transcribe";
export type JobStatus = "queued" | "running" | "done" | "failed" | "cancelled";

/** Why the pipeline hid a segment. Flagged segments are never deleted. */
export type SuppressedReason =
  | "no_speech"
  | "outside_vad"
  | "repeat"
  | "prompt_echo"
  | "echo";

export type SpeakerSource = "track" | "diarization" | "manual";
export type SummaryStatus = "pending" | "done" | "failed";
export type SummaryItemKind = "decision" | "action" | "topic";
export type MeetingLanguage = "auto" | "nl" | "en";
export type RecordingPhase = "idle" | "starting" | "recording" | "paused" | "stopping";

/** macOS reports a denial as silence, so "unknown" is common and never blocks. */
export type PermissionState = "granted" | "denied" | "unknown" | "unsupported";

export type MeetingChange =
  | "created"
  | "renamed"
  | "status"
  | "transcript"
  | "segment"
  | "audio_deleted"
  | "summary"
  | "deleted";

export interface JobProgress {
  job_id: string;
  meeting_id: string;
  run_id: string | null;
  kind: JobKind;
  status: JobStatus;
  /** Windows decoded, out of `total`. `total` is 0 until planning is done. */
  done: number;
  total: number;
  error: string | null;
}

/** A row in the meetings list. */
export interface MeetingListItem {
  id: string;
  title: string;
  status: MeetingStatus;
  started_at: string;
  ended_at: string | null;
  /** Recorded time, pauses excluded. */
  duration_ms: number;
  language: MeetingLanguage;
  /** Recorded over the built-in speakers: expect echo in the mic track. */
  echo_risk: boolean;
  has_audio: boolean;
  has_summary: boolean;
  /** The meeting's unfinished job, if any. */
  job: JobProgress | null;
}

export interface MeetingTrack {
  id: string;
  kind: TrackKind;
  device_name: string | null;
  duration_ms: number;
  has_audio: boolean;
  overflow_frames: number;
}

export interface TranscriptRun {
  id: string;
  model: string;
  language: MeetingLanguage;
  status: RunStatus;
  error: string | null;
  created_at: string;
  finished_at: string | null;
}

export interface Speaker {
  id: string;
  /** "Me", "Them", later "Speaker 2" or the assigned participant's name. */
  label: string;
  source: SpeakerSource;
  track_id: string | null;
}

export interface MeetingDetail extends MeetingListItem {
  model: string | null;
  /** The run whose segments are shown. Null until one has started. */
  active_run_id: string | null;
  error: string | null;
  /** Bytes of audio still on disk. */
  audio_bytes: number;
  tracks: MeetingTrack[];
  runs: TranscriptRun[];
  speakers: Speaker[];
}

export interface Segment {
  id: string;
  meeting_id: string;
  run_id: string;
  track_id: string;
  track_kind: TrackKind;
  start_ms: number;
  end_ms: number;
  /** What to show: the user's edit if there is one, else the decoded text. */
  text: string;
  /** The decoded text, present only when `text` is an edit. */
  original_text: string | null;
  lang: string | null;
  speaker_id: string | null;
  speaker_label: string;
  suppressed_reason: SuppressedReason | null;
  /** Hide by default: the user's choice if they made one, else flagged. */
  hidden: boolean;
}

export interface PermissionStatus {
  state: PermissionState;
  detail: string | null;
}

/** Payload of `meeting-state` and result of `get_meeting_recording_status`. */
export interface RecordingStatus {
  phase: RecordingPhase;
  meeting_id: string | null;
  started_at: string | null;
  /** Recorded time so far, pauses excluded. The UI ticks on from here. */
  elapsed_ms: number;
  /** Tracks that are actually capturing. Mic-only when the tap is unavailable. */
  tracks: TrackKind[];
  /** Only zeros on the system track for a while: probably denied. */
  system_audio_silent: boolean;
  echo_risk: boolean;
}

export interface StartMeetingOptions {
  title?: string | null;
  language?: MeetingLanguage | null;
}

export interface RetranscribeOptions {
  model?: string | null;
  language?: MeetingLanguage | null;
}

export interface SummaryItem {
  id: string;
  kind: SummaryItemKind;
  text: string;
  owner: string | null;
  due_date: string | null;
  source_segment_ids: string[];
}

export interface MeetingSummary {
  id: string;
  meeting_id: string;
  run_id: string;
  provider: string;
  model: string;
  status: SummaryStatus;
  overview: string | null;
  error: string | null;
  created_at: string;
  items: SummaryItem[];
}

export interface MeetingExport {
  file_name: string;
  markdown: string;
}

/** Payload of `meeting-updated`: refetch this meeting, or drop it on "deleted". */
export interface MeetingUpdated {
  meeting_id: string;
  change: MeetingChange;
}

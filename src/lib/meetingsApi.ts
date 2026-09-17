// The only place the frontend talks to the meetings backend: one typed
// wrapper per command in src-tauri/src/meetings/commands.rs, and one typed
// listener per event in events.rs. State lives in the backend; pages render
// what these return and what the events say.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  JobProgress,
  MeetingDetail,
  MeetingExport,
  MeetingListItem,
  MeetingSummary,
  MeetingUpdated,
  PermissionStatus,
  RecordingStatus,
  RetranscribeOptions,
  Segment,
  StartMeetingOptions,
} from "../types/meetings";

export const MEETING_EVENTS = {
  state: "meeting-state",
  jobProgress: "meeting-job-progress",
  updated: "meeting-updated",
} as const;

// --- Availability and permission ---------------------------------------------

/** The OS gate: system audio needs macOS 14.4+. */
export const meetingsSupported = () => invoke<boolean>("meetings_supported");

export const checkSystemAudioPermission = () =>
  invoke<PermissionStatus>("check_system_audio_permission");

export const openSystemAudioSettings = () => invoke<void>("open_system_audio_settings");

// --- Recording ---------------------------------------------------------------

export const startMeeting = (options: StartMeetingOptions = {}) =>
  invoke<RecordingStatus>("start_meeting", { options });

export const pauseMeeting = () => invoke<RecordingStatus>("pause_meeting");

export const resumeMeeting = () => invoke<RecordingStatus>("resume_meeting");

export const stopMeeting = () => invoke<RecordingStatus>("stop_meeting");

export const getMeetingRecordingStatus = () =>
  invoke<RecordingStatus>("get_meeting_recording_status");

// --- Meetings ----------------------------------------------------------------

export const listMeetings = () => invoke<MeetingListItem[]>("list_meetings");

export const getMeeting = (meetingId: string) =>
  invoke<MeetingDetail>("get_meeting", { meetingId });

export const renameMeeting = (meetingId: string, title: string) =>
  invoke<void>("rename_meeting", { meetingId, title });

/** Removes the transcript and the audio. */
export const deleteMeeting = (meetingId: string) =>
  invoke<void>("delete_meeting", { meetingId });

/** Removes the audio, keeps the transcript. */
export const deleteMeetingAudio = (meetingId: string) =>
  invoke<void>("delete_meeting_audio", { meetingId });

// --- Transcript --------------------------------------------------------------

/** Hidden segments are included; fold them away in the UI. No `runId` means the active run. */
export const listMeetingSegments = (meetingId: string, runId?: string) =>
  invoke<Segment[]>("list_meeting_segments", { meetingId, runId: runId ?? null });

/** `null` drops the edit and shows the decoded text again. */
export const editMeetingSegmentText = (segmentId: string, text: string | null) =>
  invoke<Segment>("edit_meeting_segment_text", { segmentId, text });

export const setMeetingSegmentHidden = (segmentId: string, hidden: boolean) =>
  invoke<Segment>("set_meeting_segment_hidden", { segmentId, hidden });

/** Queues a new run. Earlier runs are kept. */
export const retranscribeMeeting = (meetingId: string, options: RetranscribeOptions = {}) =>
  invoke<JobProgress>("retranscribe_meeting", { meetingId, options });

// --- Summary and export --------------------------------------------------------

/**
 * Sends the transcript to OpenRouter with the user's own key. `confirmed` must
 * come from an explicit "this sends the transcript to …" confirmation for this
 * meeting; the backend refuses without it.
 */
export const generateMeetingSummary = (meetingId: string, confirmed: boolean) =>
  invoke<MeetingSummary>("generate_meeting_summary", { meetingId, confirmed });

export const getMeetingSummary = (meetingId: string) =>
  invoke<MeetingSummary | null>("get_meeting_summary", { meetingId });

export const exportMeetingMarkdown = (meetingId: string) =>
  invoke<MeetingExport>("export_meeting_markdown", { meetingId });

// --- Events --------------------------------------------------------------------

export const onMeetingState = (handler: (status: RecordingStatus) => void): Promise<UnlistenFn> =>
  listen<RecordingStatus>(MEETING_EVENTS.state, (event) => handler(event.payload));

export const onMeetingJobProgress = (
  handler: (progress: JobProgress) => void,
): Promise<UnlistenFn> =>
  listen<JobProgress>(MEETING_EVENTS.jobProgress, (event) => handler(event.payload));

export const onMeetingUpdated = (handler: (update: MeetingUpdated) => void): Promise<UnlistenFn> =>
  listen<MeetingUpdated>(MEETING_EVENTS.updated, (event) => handler(event.payload));

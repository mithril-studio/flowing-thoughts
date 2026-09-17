import { beforeEach, describe, expect, it, vi } from "vitest";
import libRs from "../../src-tauri/src/lib.rs?raw";
import commandsRs from "../../src-tauri/src/meetings/commands.rs?raw";
import eventsRs from "../../src-tauri/src/meetings/events.rs?raw";

const invoke = vi.fn();
const listen = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => listen(...args),
}));

import * as api from "./meetingsApi";

/** Every wrapper, called once, with the command and arguments it must send. */
const CALLS: [string, () => Promise<unknown>, Record<string, unknown> | undefined][] = [
  ["meetings_supported", () => api.meetingsSupported(), undefined],
  ["check_system_audio_permission", () => api.checkSystemAudioPermission(), undefined],
  ["open_system_audio_settings", () => api.openSystemAudioSettings(), undefined],
  ["start_meeting", () => api.startMeeting({ title: "Standup" }), { options: { title: "Standup" } }],
  ["pause_meeting", () => api.pauseMeeting(), undefined],
  ["resume_meeting", () => api.resumeMeeting(), undefined],
  ["stop_meeting", () => api.stopMeeting(), undefined],
  ["get_meeting_recording_status", () => api.getMeetingRecordingStatus(), undefined],
  ["list_meetings", () => api.listMeetings(), undefined],
  ["get_meeting", () => api.getMeeting("m1"), { meetingId: "m1" }],
  ["rename_meeting", () => api.renameMeeting("m1", "Retro"), { meetingId: "m1", title: "Retro" }],
  ["delete_meeting", () => api.deleteMeeting("m1"), { meetingId: "m1" }],
  ["delete_meeting_audio", () => api.deleteMeetingAudio("m1"), { meetingId: "m1" }],
  ["list_meeting_segments", () => api.listMeetingSegments("m1"), { meetingId: "m1", runId: null }],
  [
    "edit_meeting_segment_text",
    () => api.editMeetingSegmentText("s1", "Hallo"),
    { segmentId: "s1", text: "Hallo" },
  ],
  [
    "set_meeting_segment_hidden",
    () => api.setMeetingSegmentHidden("s1", true),
    { segmentId: "s1", hidden: true },
  ],
  [
    "retranscribe_meeting",
    () => api.retranscribeMeeting("m1", { language: "nl" }),
    { meetingId: "m1", options: { language: "nl" } },
  ],
  [
    "generate_meeting_summary",
    () => api.generateMeetingSummary("m1", true),
    { meetingId: "m1", confirmed: true },
  ],
  ["get_meeting_summary", () => api.getMeetingSummary("m1"), { meetingId: "m1" }],
  ["export_meeting_markdown", () => api.exportMeetingMarkdown("m1"), { meetingId: "m1" }],
];

/** camelCase invoke key -> the snake_case Rust parameter Tauri maps it to. */
const toSnake = (key: string) => key.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);

/** The source of one `pub fn` / `pub async fn` in commands.rs, signature only. */
function rustSignature(command: string): string {
  const match = commandsRs.match(new RegExp(`pub (?:async )?fn ${command}\\(([^)]*)\\)`));
  expect(match, `${command} is not defined in commands.rs`).not.toBeNull();
  return match![1];
}

describe("meetingsApi", () => {
  beforeEach(() => {
    invoke.mockReset().mockResolvedValue(undefined);
    listen.mockReset().mockResolvedValue(() => {});
  });

  it.each(CALLS)("%s sends the right command and arguments", async (command, call, args) => {
    await call();
    expect(invoke).toHaveBeenCalledTimes(1);
    const [sentCommand, sentArgs] = invoke.mock.calls[0];
    expect(sentCommand).toBe(command);
    expect(sentArgs).toEqual(args);
  });

  it.each(CALLS)("%s is registered in lib.rs with matching parameters", (command, _call, args) => {
    expect(libRs).toContain(`meetings::commands::${command}`);
    const signature = rustSignature(command);
    for (const key of Object.keys(args ?? {})) {
      expect(signature, `${command} has no parameter for '${key}'`).toMatch(
        new RegExp(`\\b${toSnake(key)}:`),
      );
    }
  });

  it("wraps every command lib.rs registers", () => {
    const registered = [...libRs.matchAll(/meetings::commands::(\w+)/g)].map((m) => m[1]);
    expect(registered.sort()).toEqual(CALLS.map(([command]) => command).sort());
  });

  it("uses the event names the backend emits", () => {
    for (const name of Object.values(api.MEETING_EVENTS)) {
      expect(eventsRs).toContain(`"${name}"`);
    }
  });

  it("hands listeners the payload, not the event envelope", async () => {
    const helpers = [
      [api.onMeetingState, api.MEETING_EVENTS.state],
      [api.onMeetingJobProgress, api.MEETING_EVENTS.jobProgress],
      [api.onMeetingUpdated, api.MEETING_EVENTS.updated],
    ] as const;
    for (const [subscribe, eventName] of helpers) {
      listen.mockClear();
      const handler = vi.fn();
      await subscribe(handler);
      expect(listen.mock.calls[0][0]).toBe(eventName);
      const payload = { meeting_id: "m1" };
      listen.mock.calls[0][1]({ event: eventName, id: 1, payload });
      expect(handler).toHaveBeenCalledWith(payload);
    }
  });
});

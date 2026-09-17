import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import Meetings from "./Meetings";
import { defaultAppSettings, type AppSettings } from "../types/settings";
import {
  makeDetail,
  makeJob,
  makeMeeting,
  makeRecordingStatus,
  makeSegment,
  makeSummary,
} from "../components/meetings/fixtures";

const invokeMock = vi.fn();
const listeners = new Map<string, Set<(event: { payload: unknown }) => void>>();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) => {
    if (!listeners.has(name)) listeners.set(name, new Set());
    listeners.get(name)!.add(handler);
    return Promise.resolve(() => listeners.get(name)?.delete(handler));
  },
}));

/** Fires a backend event at every listener the page registered. */
function emit(name: string, payload: unknown) {
  act(() => {
    listeners.get(name)?.forEach((handler) => handler({ payload }));
  });
}

type Reply = unknown | ((args: Record<string, unknown>) => unknown);

const idle = makeRecordingStatus({ phase: "idle", meeting_id: null, started_at: null, tracks: [] });

/** Answers every meetings command; a test overrides the ones it cares about. */
function mockBackend(overrides: Record<string, Reply> = {}) {
  const replies: Record<string, Reply> = {
    meetings_supported: true,
    check_system_audio_permission: { state: "granted", detail: null },
    open_system_audio_settings: null,
    get_meeting_recording_status: idle,
    list_meetings: [],
    ...overrides,
  };
  invokeMock.mockImplementation((command: string, args: Record<string, unknown> = {}) => {
    if (!(command in replies)) return Promise.reject(`unexpected command: ${command}`);
    const reply = replies[command];
    return Promise.resolve(typeof reply === "function" ? reply(args) : reply);
  });
}

const withSummaries: AppSettings = {
  ...defaultAppSettings,
  meetings: { ...defaultAppSettings.meetings, enabled: true, summary_enabled: true },
};

const renderPage = (settings: AppSettings = defaultAppSettings) =>
  render(<Meetings settings={settings} />);

beforeEach(() => {
  invokeMock.mockReset();
  listeners.clear();
});

describe("Meetings list", () => {
  it("shows the empty state when there are no meetings", async () => {
    mockBackend();
    renderPage();
    expect(await screen.findByText("No meetings yet")).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Start meeting" })).toBeEnabled(),
    );
  });

  it("groups meetings by day with time, duration and state", async () => {
    const now = new Date();
    const yesterday = new Date(now.getTime() - 24 * 60 * 60 * 1000);
    mockBackend({
      list_meetings: [
        makeMeeting({ id: "a", title: "Design review", started_at: now.toISOString() }),
        makeMeeting({
          id: "b",
          title: "Client call",
          started_at: yesterday.toISOString(),
          status: "interrupted",
          duration_ms: 3_725_000,
        }),
        makeMeeting({ id: "c", title: "", started_at: yesterday.toISOString(), status: "failed" }),
      ],
    });
    renderPage();

    const today = await screen.findByRole("region", { name: "Today" });
    expect(within(today).getByText("Design review")).toBeInTheDocument();
    expect(within(today).getByText("Ready")).toBeInTheDocument();
    expect(within(today).getByText(/31:05/)).toBeInTheDocument();

    const earlier = screen.getByRole("region", { name: "Yesterday" });
    expect(within(earlier).getByText("Client call")).toBeInTheDocument();
    expect(within(earlier).getByText("Interrupted")).toBeInTheDocument();
    expect(within(earlier).getByText(/1:02:05/)).toBeInTheDocument();
    expect(within(earlier).getByText("Untitled meeting")).toBeInTheDocument();
    expect(within(earlier).getByText("Failed")).toBeInTheDocument();
  });

  it("updates a row's badge from a meeting-job-progress event", async () => {
    mockBackend({
      list_meetings: [makeMeeting({ status: "transcribing", job: makeJob() })],
    });
    renderPage();
    expect(await screen.findByText("Processing")).toBeInTheDocument();

    emit("meeting-job-progress", makeJob({ done: 3, total: 6 }));
    expect(screen.getByText("Processing 50%")).toBeInTheDocument();
  });

  it("deletes a meeting only after the in-page confirm step", async () => {
    mockBackend({ list_meetings: [makeMeeting()], delete_meeting: null });
    renderPage();

    fireEvent.click(await screen.findByRole("button", { name: "Delete Weekly sync" }));
    expect(invokeMock).not.toHaveBeenCalledWith("delete_meeting", expect.anything());

    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("delete_meeting", { meetingId: "m1" }),
    );
    expect(await screen.findByText("No meetings yet")).toBeInTheDocument();
  });

  it("drops a meeting when the backend reports it deleted", async () => {
    mockBackend({ list_meetings: [makeMeeting()] });
    renderPage();
    expect(await screen.findByText("Weekly sync")).toBeInTheDocument();

    emit("meeting-updated", { meeting_id: "m1", change: "deleted" });
    expect(screen.queryByText("Weekly sync")).not.toBeInTheDocument();
  });

  it("explains the macOS requirement and disables Start when unsupported", async () => {
    mockBackend({ meetings_supported: false });
    renderPage();
    expect(await screen.findByText(/Meetings need macOS 14.4 or later/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start meeting" })).toBeDisabled();
  });
});

describe("Meeting recording", () => {
  it("starts, pauses, resumes and stops through the backend commands", async () => {
    mockBackend({
      start_meeting: makeRecordingStatus(),
      pause_meeting: makeRecordingStatus({ phase: "paused", elapsed_ms: 5000 }),
      resume_meeting: makeRecordingStatus({ elapsed_ms: 5000 }),
      stop_meeting: idle,
    });
    renderPage();

    const start = await screen.findByRole("button", { name: "Start meeting" });
    await waitFor(() => expect(start).toBeEnabled());
    fireEvent.click(start);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("start_meeting", { options: {} }));

    fireEvent.click(await screen.findByRole("button", { name: "Pause" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("pause_meeting"));

    fireEvent.click(await screen.findByRole("button", { name: "Resume" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("resume_meeting"));

    const stop = await screen.findByRole("button", { name: "Stop" });
    await waitFor(() => expect(stop).toBeEnabled());
    fireEvent.click(stop);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("stop_meeting"));
    expect(await screen.findByRole("button", { name: "Start meeting" })).toBeInTheDocument();
  });

  it("renders the recording bar from meeting-state events", async () => {
    mockBackend();
    renderPage();
    await screen.findByText("No meetings yet");
    expect(screen.queryByRole("region", { name: "Meeting recording" })).not.toBeInTheDocument();

    emit("meeting-state", makeRecordingStatus({ phase: "paused", elapsed_ms: 65_000 }));
    const bar = screen.getByRole("region", { name: "Meeting recording" });
    expect(within(bar).getByLabelText("Elapsed time")).toHaveTextContent("01:05");
    expect(within(bar).getByText("Paused")).toBeInTheDocument();
    expect(within(bar).getByRole("button", { name: "Resume" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Start meeting" })).not.toBeInTheDocument();

    emit("meeting-state", idle);
    expect(screen.queryByRole("region", { name: "Meeting recording" })).not.toBeInTheDocument();
  });

  it("picks up a recording that was already running", async () => {
    mockBackend({
      get_meeting_recording_status: makeRecordingStatus({ phase: "paused", elapsed_ms: 600_000 }),
    });
    renderPage();
    const bar = await screen.findByRole("region", { name: "Meeting recording" });
    expect(within(bar).getByLabelText("Elapsed time")).toHaveTextContent("10:00");
  });

  it("says so when system audio permission is denied", async () => {
    mockBackend({
      check_system_audio_permission: { state: "denied", detail: null },
      get_meeting_recording_status: makeRecordingStatus({ tracks: ["mic"] }),
    });
    renderPage();

    expect(
      await screen.findByText("System audio permission denied, recording microphone only."),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open System Settings" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("open_system_audio_settings"));
  });

  it("shows the headphones and silent-system-audio notices", async () => {
    mockBackend({ check_system_audio_permission: { state: "unknown", detail: null } });
    renderPage();
    await screen.findByText("No meetings yet");

    emit("meeting-state", makeRecordingStatus({ echo_risk: true, system_audio_silent: true }));
    expect(screen.getByText(/Use headphones for best results/)).toBeInTheDocument();
    expect(screen.getByText(/No system audio detected/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open System Settings" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("open_system_audio_settings"));
  });
});

describe("Meeting detail", () => {
  const segments = [
    makeSegment({
      id: "s2",
      start_ms: 4000,
      end_ms: 6000,
      text: "Doing well, thanks",
      track_kind: "system",
      speaker_label: "Them",
    }),
    makeSegment({ id: "s1", start_ms: 1000, end_ms: 3000, text: "How are you?" }),
    makeSegment({
      id: "s3",
      start_ms: 4200,
      end_ms: 6000,
      text: "Doing well thanks",
      suppressed_reason: "echo",
      hidden: true,
    }),
  ];

  async function openDetail(overrides: Record<string, Reply> = {}, settings?: AppSettings) {
    mockBackend({
      list_meetings: [makeMeeting()],
      get_meeting: makeDetail(),
      list_meeting_segments: segments,
      get_meeting_summary: null,
      ...overrides,
    });
    renderPage(settings);
    fireEvent.click(await screen.findByRole("button", { name: /^Weekly sync/ }));
    await screen.findByLabelText("Meeting title");
  }

  it("renders Me and Them segments in time order", async () => {
    await openDetail();
    const rows = screen.getAllByTestId("meeting-segment");
    expect(rows).toHaveLength(2);
    expect(within(rows[0]).getByText("Me")).toBeInTheDocument();
    expect(within(rows[0]).getByText("00:01")).toBeInTheDocument();
    expect(within(rows[0]).getByText("How are you?")).toBeInTheDocument();
    expect(within(rows[1]).getByText("Them")).toBeInTheDocument();
    expect(within(rows[1]).getByText("Doing well, thanks")).toBeInTheDocument();
  });

  it("reveals flagged segments, dimmed and with their reason", async () => {
    await openDetail();
    expect(screen.queryByText("Doing well thanks")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Show 1 hidden" }));
    const flagged = screen.getByText("Doing well thanks").closest("li")!;
    expect(flagged.className).toContain("opacity-50");
    expect(within(flagged).getByText("Echo of the other side")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Hide 1 hidden" }));
    expect(screen.queryByText("Doing well thanks")).not.toBeInTheDocument();
  });

  it("saves an edited segment on blur", async () => {
    await openDetail({
      edit_meeting_segment_text: ({ text }: Record<string, unknown>) =>
        makeSegment({
          id: "s1",
          start_ms: 1000,
          text: String(text),
          original_text: "How are you?",
        }),
    });

    fireEvent.click(screen.getByRole("button", { name: "Edit segment at 00:01" }));
    const editor = screen.getByLabelText("Segment text at 00:01");
    fireEvent.change(editor, { target: { value: "How are you doing?" } });
    fireEvent.blur(editor);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("edit_meeting_segment_text", {
        segmentId: "s1",
        text: "How are you doing?",
      }),
    );
    expect(await screen.findByText("How are you doing?")).toBeInTheDocument();
    expect(screen.getByText("edited")).toBeInTheDocument();
  });

  it("does not save an unchanged or cancelled edit", async () => {
    await openDetail();
    fireEvent.click(screen.getByText("How are you?"));
    const editor = screen.getByLabelText("Segment text at 00:01");
    fireEvent.change(editor, { target: { value: "Something else" } });
    fireEvent.keyDown(editor, { key: "Escape" });
    fireEvent.blur(editor);

    expect(screen.getByText("How are you?")).toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalledWith("edit_meeting_segment_text", expect.anything());
  });

  it("renames the meeting on blur", async () => {
    await openDetail({ rename_meeting: null });
    const title = screen.getByLabelText("Meeting title");
    expect(title).toHaveValue("Weekly sync");
    fireEvent.focus(title);
    fireEvent.change(title, { target: { value: "Weekly sync with Anna " } });
    fireEvent.blur(title);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("rename_meeting", {
        meetingId: "m1",
        title: "Weekly sync with Anna",
      }),
    );
  });

  it("shows transcription progress and follows progress events", async () => {
    await openDetail({
      get_meeting: makeDetail({ status: "transcribing", job: makeJob({ done: 1, total: 4 }) }),
      list_meeting_segments: [],
    });
    const bar = screen.getByRole("progressbar", { name: "Transcription progress" });
    expect(bar).toHaveAttribute("aria-valuenow", "25");

    emit("meeting-job-progress", makeJob({ done: 3, total: 4 }));
    expect(bar).toHaveAttribute("aria-valuenow", "75");
  });

  it("refetches when the backend reports a new transcript", async () => {
    let ready = false;
    await openDetail({
      get_meeting: () => (ready ? makeDetail() : makeDetail({ status: "transcribing" })),
      list_meeting_segments: () => (ready ? segments : []),
    });
    expect(screen.queryByTestId("meeting-segment")).not.toBeInTheDocument();

    ready = true;
    emit("meeting-updated", { meeting_id: "m1", change: "transcript" });
    expect(await screen.findByText("How are you?")).toBeInTheDocument();
  });

  it("copies the visible transcript and re-transcribes in a chosen language", async () => {
    await openDetail({
      copy_to_clipboard: null,
      retranscribe_meeting: makeJob({ status: "queued" }),
    });

    fireEvent.click(screen.getByRole("button", { name: "Copy transcript" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("copy_to_clipboard", {
        text: "[00:01] Me: How are you?\n[00:04] Them: Doing well, thanks",
      }),
    );
    expect(await screen.findByText("Transcript copied")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Re-transcribe as Dutch" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("retranscribe_meeting", {
        meetingId: "m1",
        options: { language: "nl" },
      }),
    );
    expect(await screen.findByRole("progressbar")).toBeInTheDocument();
  });

  it("exports the backend's Markdown", async () => {
    await openDetail({
      copy_to_clipboard: null,
      export_meeting_markdown: { file_name: "2026-09-17 Weekly sync.md", markdown: "# Weekly sync" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Export Markdown" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("copy_to_clipboard", { text: "# Weekly sync" }),
    );
    expect(await screen.findByText("Markdown copied")).toBeInTheDocument();
  });

  it("deletes the audio after a confirm step and keeps the transcript", async () => {
    let deleted = false;
    await openDetail({
      get_meeting: () => makeDetail(deleted ? { has_audio: false, audio_bytes: 0 } : {}),
      delete_meeting_audio: () => {
        deleted = true;
        return null;
      },
    });

    fireEvent.click(screen.getByRole("button", { name: "Delete audio (120 MB)" }));
    expect(invokeMock).not.toHaveBeenCalledWith("delete_meeting_audio", expect.anything());
    fireEvent.click(screen.getByRole("button", { name: "Delete audio" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("delete_meeting_audio", { meetingId: "m1" }),
    );
    await waitFor(() => expect(screen.getByRole("button", { name: "Delete audio" })).toBeDisabled());
    expect(screen.getByRole("button", { name: "Re-transcribe as Auto" })).toBeDisabled();
    expect(screen.getByText("How are you?")).toBeInTheDocument();
  });

  it("deletes the meeting and returns to the list", async () => {
    await openDetail({ delete_meeting: null });
    fireEvent.click(screen.getByRole("button", { name: "Delete meeting" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete meeting" }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("delete_meeting", { meetingId: "m1" }),
    );
    expect(await screen.findByText("No meetings yet")).toBeInTheDocument();
  });

  it("goes back to the list", async () => {
    await openDetail();
    fireEvent.click(screen.getByRole("button", { name: /Back/ }));
    expect(await screen.findByRole("button", { name: "Start meeting" })).toBeInTheDocument();
  });

  describe("summary", () => {
    it("is absent unless summaries are enabled in settings", async () => {
      await openDetail();
      expect(screen.queryByRole("region", { name: "Summary" })).not.toBeInTheDocument();
      expect(invokeMock).not.toHaveBeenCalledWith("get_meeting_summary", expect.anything());
    });

    it("sends the transcript only after an explicit confirmation", async () => {
      await openDetail(
        { generate_meeting_summary: makeSummary({ overview: "A short catch-up." }) },
        withSummaries,
      );
      const panel = screen.getByRole("region", { name: "Summary" });
      expect(within(panel).getByText(/sends this meeting's full transcript to OpenRouter/)).toBeInTheDocument();

      fireEvent.click(within(panel).getByRole("button", { name: "Generate summary" }));
      expect(invokeMock).not.toHaveBeenCalledWith("generate_meeting_summary", expect.anything());

      fireEvent.click(within(panel).getByRole("button", { name: "Send and summarize" }));
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("generate_meeting_summary", {
          meetingId: "m1",
          confirmed: true,
        }),
      );
      expect(await within(panel).findByText("A short catch-up.")).toBeInTheDocument();
    });

    it("lists decisions, actions and topics and highlights an item's sources", async () => {
      await openDetail(
        {
          get_meeting_summary: makeSummary({
            items: [
              {
                id: "i1",
                kind: "decision",
                text: "Ship on Friday",
                owner: null,
                due_date: null,
                source_segment_ids: ["s2"],
              },
              {
                id: "i2",
                kind: "action",
                text: "Send the notes",
                owner: null,
                due_date: null,
                source_segment_ids: ["s3"],
              },
              {
                id: "i3",
                kind: "action",
                text: "Book the room",
                owner: "Anna",
                due_date: "2026-09-21",
                source_segment_ids: [],
              },
              {
                id: "i4",
                kind: "topic",
                text: "Wellbeing",
                owner: null,
                due_date: null,
                source_segment_ids: ["s1", "s2"],
              },
            ],
          }),
        },
        withSummaries,
      );
      const panel = screen.getByRole("region", { name: "Summary" });
      expect(within(panel).getByText("Decisions")).toBeInTheDocument();
      expect(within(panel).getByText("Action items")).toBeInTheDocument();
      expect(within(panel).getByText("Topics")).toBeInTheDocument();
      expect(within(panel).getByText("Owner: unknown · Due: unknown")).toBeInTheDocument();
      expect(within(panel).getByText("Owner: Anna · Due: 2026-09-21")).toBeInTheDocument();

      fireEvent.click(within(panel).getByRole("button", { name: /Ship on Friday/ }));
      const source = screen.getByText("Doing well, thanks").closest("li")!;
      expect(source.className).toContain("ring-emerald-300");
      expect(screen.getByText("How are you?").closest("li")!.className).not.toContain("ring-");

      // A source that was flagged is revealed so it can be shown.
      fireEvent.click(within(panel).getByRole("button", { name: /Send the notes/ }));
      expect(screen.getByText("Doing well thanks").closest("li")!.className).toContain(
        "ring-emerald-300",
      );
    });
  });
});

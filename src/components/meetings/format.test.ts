import { describe, expect, it } from "vitest";
import {
  dayLabel,
  formatDuration,
  groupByDay,
  hiddenReasonLabel,
  jobPercent,
  statusBadge,
  transcriptAsText,
} from "./format";
import { makeJob, makeSegment } from "./fixtures";

describe("meeting format helpers", () => {
  it("formats durations with hours only when needed", () => {
    expect(formatDuration(0)).toBe("00:00");
    expect(formatDuration(65_400)).toBe("01:05");
    expect(formatDuration(3_725_000)).toBe("1:02:05");
    expect(formatDuration(-5)).toBe("00:00");
  });

  it("labels today, yesterday and older days", () => {
    const now = new Date(2026, 8, 17, 15, 0);
    expect(dayLabel(new Date(2026, 8, 17, 9, 0).toISOString(), now)).toBe("Today");
    expect(dayLabel(new Date(2026, 8, 16, 23, 0).toISOString(), now)).toBe("Yesterday");
    expect(dayLabel(new Date(2026, 8, 10, 9, 0).toISOString(), now)).not.toMatch(/Today|Yesterday/);
    expect(dayLabel(new Date(2025, 8, 10, 9, 0).toISOString(), now)).toContain("2025");
  });

  it("groups by local day, newest first", () => {
    const now = new Date(2026, 8, 17, 15, 0);
    const at = (day: number, hour: number) => new Date(2026, 8, day, hour).toISOString();
    const groups = groupByDay(
      [
        { id: "old", started_at: at(16, 10) },
        { id: "late", started_at: at(17, 14) },
        { id: "early", started_at: at(17, 9) },
      ],
      now,
    );
    expect(groups.map((g) => g.label)).toEqual(["Today", "Yesterday"]);
    expect(groups[0].items.map((m) => m.id)).toEqual(["late", "early"]);
  });

  it("has no percentage until the job has planned its windows", () => {
    expect(jobPercent(null)).toBeNull();
    expect(jobPercent(makeJob({ total: 0 }))).toBeNull();
    expect(jobPercent(makeJob({ done: 1, total: 3 }))).toBe(33);
    expect(jobPercent(makeJob({ done: 9, total: 3 }))).toBe(100);
  });

  it("maps every meeting state to a badge", () => {
    expect(statusBadge({ status: "recording", job: null }).label).toBe("Recording");
    expect(statusBadge({ status: "queued", job: null }).label).toBe("Processing");
    expect(statusBadge({ status: "transcribing", job: makeJob({ done: 1, total: 4 }) }).label).toBe(
      "Processing 25%",
    );
    expect(statusBadge({ status: "ready", job: null })).toEqual({ label: "Ready", tone: "emerald" });
    expect(statusBadge({ status: "ready", job: makeJob({ done: 1, total: 2 }) }).label).toBe(
      "Processing 50%",
    );
    expect(statusBadge({ status: "ready", job: makeJob({ status: "done" }) }).label).toBe("Ready");
    expect(statusBadge({ status: "interrupted", job: null }).tone).toBe("amber");
    expect(statusBadge({ status: "failed", job: null }).tone).toBe("red");
  });

  it("names why a segment is hidden", () => {
    expect(hiddenReasonLabel("echo")).toBe("Echo of the other side");
    expect(hiddenReasonLabel(null)).toBe("Hidden by you");
  });

  it("leaves hidden segments out of the copied transcript", () => {
    const text = transcriptAsText([
      makeSegment({ id: "b", start_ms: 61_000, text: "Second", speaker_label: "Them" }),
      makeSegment({ id: "x", start_ms: 30_000, text: "Noise", hidden: true }),
      makeSegment({ id: "a", start_ms: 0, text: "First" }),
    ]);
    expect(text).toBe("[00:00] Me: First\n[01:01] Them: Second");
  });
});

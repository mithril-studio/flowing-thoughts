// Pure display helpers for the Meetings page. No state, no backend calls.

import type {
  JobProgress,
  MeetingLanguage,
  MeetingListItem,
  Segment,
  SuppressedReason,
} from "../../types/meetings";

const pad = (n: number) => String(n).padStart(2, "0");

/** `mm:ss`, or `h:mm:ss` from one hour. Used for durations and transcript timestamps. */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(seconds)}` : `${pad(minutes)}:${pad(seconds)}`;
}

export function formatClockTime(iso: string): string {
  return new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function formatBytes(n: number): string {
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)} GB`;
  if (n >= 1_000_000) return `${Math.round(n / 1_000_000)} MB`;
  return `${Math.max(1, Math.round(n / 1000))} KB`;
}

export function dayLabel(iso: string, now: Date = new Date()): string {
  const d = new Date(iso);
  if (d.toDateString() === now.toDateString()) return "Today";
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
  return d.toLocaleDateString([], {
    weekday: "short",
    month: "short",
    day: "numeric",
    ...(d.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  });
}

export interface DayGroup<T> {
  key: string;
  label: string;
  items: T[];
}

/** Newest first, one group per local calendar day. */
export function groupByDay<T extends { started_at: string }>(
  items: T[],
  now: Date = new Date(),
): DayGroup<T>[] {
  const sorted = [...items].sort((a, b) => b.started_at.localeCompare(a.started_at));
  const groups: DayGroup<T>[] = [];
  for (const item of sorted) {
    const key = new Date(item.started_at).toDateString();
    const last = groups[groups.length - 1];
    if (last && last.key === key) {
      last.items.push(item);
    } else {
      groups.push({ key, label: dayLabel(item.started_at, now), items: [item] });
    }
  }
  return groups;
}

/** Null until the job has planned its windows. */
export function jobPercent(job: JobProgress | null): number | null {
  if (!job || job.total <= 0) return null;
  return Math.min(100, Math.max(0, Math.round((job.done / job.total) * 100)));
}

export const jobIsActive = (job: JobProgress | null): boolean =>
  job !== null && (job.status === "queued" || job.status === "running");

export type BadgeTone = "red" | "amber" | "emerald" | "zinc";

export interface StatusBadgeInfo {
  label: string;
  tone: BadgeTone;
}

export function statusBadge(meeting: Pick<MeetingListItem, "status" | "job">): StatusBadgeInfo {
  switch (meeting.status) {
    case "recording":
      return { label: "Recording", tone: "red" };
    case "paused":
      return { label: "Paused", tone: "amber" };
    case "queued":
    case "transcribing": {
      const percent = jobPercent(meeting.job);
      return { label: percent === null ? "Processing" : `Processing ${percent}%`, tone: "amber" };
    }
    case "ready":
      // A re-transcription of a finished meeting still shows its progress.
      if (jobIsActive(meeting.job)) {
        const percent = jobPercent(meeting.job);
        return { label: percent === null ? "Processing" : `Processing ${percent}%`, tone: "amber" };
      }
      return { label: "Ready", tone: "emerald" };
    case "interrupted":
      return { label: "Interrupted", tone: "amber" };
    case "failed":
      return { label: "Failed", tone: "red" };
    default:
      return { label: String(meeting.status), tone: "zinc" };
  }
}

export function languageLabel(language: MeetingLanguage | string): string {
  if (language === "nl") return "Dutch";
  if (language === "en") return "English";
  return "Auto";
}

/** Why a segment is folded away. Hidden without a flag means the user hid it. */
export function hiddenReasonLabel(reason: SuppressedReason | null): string {
  switch (reason) {
    case "no_speech":
      return "No speech";
    case "outside_vad":
      return "Outside detected speech";
    case "repeat":
      return "Repetition";
    case "prompt_echo":
      return "Prompt echo";
    case "echo":
      return "Echo of the other side";
    default:
      return "Hidden by you";
  }
}

export function sortSegments(segments: Segment[]): Segment[] {
  return [...segments].sort(
    (a, b) => a.start_ms - b.start_ms || a.end_ms - b.end_ms || a.id.localeCompare(b.id),
  );
}

/** Plain text for the clipboard: visible segments only, one line each. */
export function transcriptAsText(segments: Segment[]): string {
  return sortSegments(segments)
    .filter((s) => !s.hidden)
    .map((s) => `[${formatDuration(s.start_ms)}] ${s.speaker_label}: ${s.text}`)
    .join("\n");
}

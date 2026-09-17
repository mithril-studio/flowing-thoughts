import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import * as meetingsApi from "../../lib/meetingsApi";
import type {
  MeetingDetail as MeetingDetailData,
  MeetingLanguage,
  MeetingSummary,
  Segment,
} from "../../types/meetings";
import type { MeetingsSettings } from "../../types/settings";
import {
  dayLabel,
  formatBytes,
  formatClockTime,
  formatDuration,
  jobIsActive,
  jobPercent,
  languageLabel,
  transcriptAsText,
} from "./format";
import SummaryPanel from "./SummaryPanel";
import Transcript, { segmentDomId } from "./Transcript";
import { ConfirmInline, ErrorBanner, StatusBadge, dangerButton, secondaryButton } from "./ui";

interface MeetingDetailProps {
  meetingId: string;
  settings: MeetingsSettings;
  onBack: () => void;
  /** Removes the meeting (transcript and audio). The page owns the list. */
  onDelete: (meetingId: string) => Promise<void>;
}

const RETRANSCRIBE_CHOICES: { language: MeetingLanguage; label: string }[] = [
  { language: "nl", label: "Dutch" },
  { language: "en", label: "English" },
  { language: "auto", label: "Auto" },
];

const HIGHLIGHT_MS = 2500;

export default function MeetingDetail({ meetingId, settings, onBack, onDelete }: MeetingDetailProps) {
  const [meeting, setMeeting] = useState<MeetingDetailData | null>(null);
  const [segments, setSegments] = useState<Segment[]>([]);
  const [summary, setSummary] = useState<MeetingSummary | null>(null);
  const [title, setTitle] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [showHidden, setShowHidden] = useState(false);
  const [highlightedIds, setHighlightedIds] = useState<ReadonlySet<string>>(new Set());
  const [confirming, setConfirming] = useState<"audio" | "meeting" | null>(null);
  const [busy, setBusy] = useState(false);
  const [generating, setGenerating] = useState(false);
  const [summaryError, setSummaryError] = useState<string | null>(null);
  const titleFocused = useRef(false);
  const highlightTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const summaryEnabled = settings.summary_enabled;

  const load = useCallback(
    async (isCurrent: () => boolean = () => true) => {
      try {
        const [detail, rows, latestSummary] = await Promise.all([
          meetingsApi.getMeeting(meetingId),
          meetingsApi.listMeetingSegments(meetingId),
          summaryEnabled ? meetingsApi.getMeetingSummary(meetingId) : Promise.resolve(null),
        ]);
        if (!isCurrent()) return;
        setMeeting(detail);
        setSegments(rows);
        setSummary(latestSummary);
        if (!titleFocused.current) setTitle(detail.title);
      } catch (e) {
        if (isCurrent()) setError(String(e));
      }
    },
    [meetingId, summaryEnabled],
  );

  useEffect(() => {
    let current = true;
    const isCurrent = () => current;
    void load(isCurrent);
    const unlistenUpdated = meetingsApi.onMeetingUpdated((update) => {
      // "deleted" is the page's business: it closes this view.
      if (update.meeting_id === meetingId && update.change !== "deleted") void load(isCurrent);
    });
    const unlistenProgress = meetingsApi.onMeetingJobProgress((progress) => {
      if (progress.meeting_id !== meetingId) return;
      setMeeting((prev) => (prev ? { ...prev, job: progress } : prev));
    });
    return () => {
      current = false;
      void unlistenUpdated.then((fn) => fn());
      void unlistenProgress.then((fn) => fn());
    };
  }, [meetingId, load]);

  useEffect(
    () => () => {
      if (highlightTimer.current) clearTimeout(highlightTimer.current);
      if (noticeTimer.current) clearTimeout(noticeTimer.current);
    },
    [],
  );

  const flash = (message: string) => {
    setNotice(message);
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    noticeTimer.current = setTimeout(() => setNotice(null), 1500);
  };

  const saveTitle = async () => {
    titleFocused.current = false;
    if (!meeting) return;
    const next = title.trim();
    if (next === meeting.title) return;
    try {
      await meetingsApi.renameMeeting(meetingId, next);
      setMeeting((prev) => (prev ? { ...prev, title: next } : prev));
    } catch (e) {
      setError(String(e));
    }
  };

  const replaceSegment = (updated: Segment) =>
    setSegments((prev) => prev.map((s) => (s.id === updated.id ? updated : s)));

  const editText = async (segmentId: string, text: string | null) => {
    try {
      replaceSegment(await meetingsApi.editMeetingSegmentText(segmentId, text));
    } catch (e) {
      setError(String(e));
    }
  };

  const setHidden = async (segmentId: string, hidden: boolean) => {
    try {
      replaceSegment(await meetingsApi.setMeetingSegmentHidden(segmentId, hidden));
    } catch (e) {
      setError(String(e));
    }
  };

  // Only ever on the user's click; a meeting never reaches the clipboard by itself.
  const copy = async (text: string, done: string) => {
    try {
      await invoke("copy_to_clipboard", { text });
      flash(done);
    } catch (e) {
      setError(String(e));
    }
  };

  const exportMarkdown = async () => {
    try {
      const exported = await meetingsApi.exportMeetingMarkdown(meetingId);
      await copy(exported.markdown, "Markdown copied");
    } catch (e) {
      setError(String(e));
    }
  };

  const retranscribe = async (language: MeetingLanguage) => {
    setBusy(true);
    try {
      const job = await meetingsApi.retranscribeMeeting(meetingId, { language });
      setMeeting((prev) => (prev ? { ...prev, job } : prev));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const deleteAudio = async () => {
    setBusy(true);
    try {
      await meetingsApi.deleteMeetingAudio(meetingId);
      setConfirming(null);
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const deleteMeeting = async () => {
    setBusy(true);
    try {
      await onDelete(meetingId);
    } finally {
      setBusy(false);
      setConfirming(null);
    }
  };

  const generateSummary = async () => {
    setGenerating(true);
    setSummaryError(null);
    try {
      // `true` because SummaryPanel only calls this from its confirm step.
      setSummary(await meetingsApi.generateMeetingSummary(meetingId, true));
    } catch (e) {
      setSummaryError(String(e));
    } finally {
      setGenerating(false);
    }
  };

  const showSources = (segmentIds: string[]) => {
    if (segmentIds.length === 0) return;
    const byId = new Map(segments.map((s) => [s.id, s]));
    if (segmentIds.some((id) => byId.get(id)?.hidden)) setShowHidden(true);
    setHighlightedIds(new Set(segmentIds));
    if (highlightTimer.current) clearTimeout(highlightTimer.current);
    highlightTimer.current = setTimeout(() => setHighlightedIds(new Set()), HIGHLIGHT_MS);
    // After the render that may have just revealed a hidden source.
    setTimeout(() => {
      const el = document.getElementById(segmentDomId(segmentIds[0]));
      el?.scrollIntoView?.({ behavior: "smooth", block: "center" });
    }, 0);
  };

  const live = meeting?.status === "recording" || meeting?.status === "paused";
  const processing =
    meeting !== null &&
    (jobIsActive(meeting.job) || meeting.status === "queued" || meeting.status === "transcribing");
  const percent = meeting ? jobPercent(meeting.job) : null;
  const hasVisibleText = segments.some((s) => !s.hidden);

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between gap-2 border-b border-zinc-200 dark:border-zinc-800 px-4 py-3">
        <button
          type="button"
          onClick={onBack}
          className="text-xs text-zinc-600 transition-colors hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-100"
        >
          &larr; Back
        </button>
        <div className="flex items-center gap-1.5">
          {notice && (
            <span role="status" className="text-[11px] text-emerald-700 dark:text-emerald-300">
              {notice}
            </span>
          )}
          <button
            type="button"
            onClick={() => void copy(transcriptAsText(segments), "Transcript copied")}
            disabled={!hasVisibleText}
            className={secondaryButton}
          >
            Copy transcript
          </button>
          <button
            type="button"
            onClick={() => void exportMarkdown()}
            disabled={!hasVisibleText}
            className={secondaryButton}
          >
            Export Markdown
          </button>
        </div>
      </div>

      <div className="flex-1 space-y-4 overflow-y-auto px-4 py-3">
        {error && <ErrorBanner message={error} onDismiss={() => setError(null)} />}

        {!meeting ? (
          !error && <p className="text-xs text-zinc-500">Loading…</p>
        ) : (
          <>
            <div className="space-y-1">
              <input
                type="text"
                aria-label="Meeting title"
                placeholder="Untitled meeting"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                onFocus={() => {
                  titleFocused.current = true;
                }}
                onBlur={() => void saveTitle()}
                onKeyDown={(e) => {
                  if (e.key === "Enter") e.currentTarget.blur();
                }}
                className="w-full bg-transparent text-sm font-semibold text-zinc-900 placeholder-zinc-500 focus:outline-none dark:text-zinc-100"
              />
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-zinc-500">
                <span>
                  {dayLabel(meeting.started_at)} · {formatClockTime(meeting.started_at)} ·{" "}
                  {formatDuration(meeting.duration_ms)} · {languageLabel(meeting.language)}
                </span>
                <StatusBadge meeting={meeting} />
              </div>
            </div>

            {meeting.status === "failed" && (
              <p className="rounded-xl border border-red-200 dark:border-red-900/60 bg-red-50 dark:bg-red-950/40 p-3 text-xs text-red-700 dark:text-red-300">
                {meeting.error ?? meeting.job?.error ?? "Transcription failed."}
              </p>
            )}
            {meeting.status === "interrupted" && (
              <p className="rounded-xl border border-amber-200 dark:border-amber-900/60 bg-amber-50 dark:bg-amber-950/30 p-3 text-xs text-amber-800 dark:text-amber-300">
                The app closed while this meeting was recording. Everything recorded up to that
                moment was kept.
              </p>
            )}

            {processing && (
              <div className="space-y-1.5">
                <div className="flex items-center justify-between text-xs text-zinc-600 dark:text-zinc-400">
                  <span>
                    {percent === null ? "Waiting to transcribe…" : "Transcribing on this Mac…"}
                  </span>
                  {percent !== null && <span className="tabular-nums">{percent}%</span>}
                </div>
                <div
                  role="progressbar"
                  aria-label="Transcription progress"
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={percent ?? 0}
                  className="h-1.5 overflow-hidden rounded-full bg-zinc-200 dark:bg-zinc-800"
                >
                  <div
                    className="h-full rounded-full bg-emerald-500 transition-all dark:bg-emerald-400"
                    style={{ width: `${percent ?? 0}%` }}
                  />
                </div>
              </div>
            )}

            {summaryEnabled && !live && (
              <SummaryPanel
                summary={summary}
                model={settings.summary_model}
                segments={segments}
                canGenerate={hasVisibleText && !processing}
                generating={generating}
                error={summaryError}
                onGenerate={() => void generateSummary()}
                onShowSources={showSources}
              />
            )}

            {live ? (
              <p className="text-xs text-zinc-500">
                The transcript appears after you stop the meeting.
              </p>
            ) : segments.length === 0 && processing ? (
              <p className="text-xs text-zinc-500">The transcript appears here as it is decoded.</p>
            ) : (
              <Transcript
                segments={segments}
                showHidden={showHidden}
                onShowHiddenChange={setShowHidden}
                highlightedIds={highlightedIds}
                onEditText={editText}
                onSetHidden={setHidden}
              />
            )}

            {!live && (
              <section
                aria-label="Manage meeting"
                className="space-y-3 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-900/60 p-3"
              >
                <div className="space-y-1.5">
                  <p className="text-xs text-zinc-600 dark:text-zinc-400">Re-transcribe as</p>
                  <div className="grid grid-cols-3 gap-2">
                    {RETRANSCRIBE_CHOICES.map(({ language, label }) => (
                      <button
                        key={language}
                        type="button"
                        aria-label={`Re-transcribe as ${label}`}
                        onClick={() => void retranscribe(language)}
                        disabled={busy || processing || !meeting.has_audio}
                        className={secondaryButton}
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                  <p className="text-[11px] text-zinc-500">
                    {meeting.has_audio
                      ? "Decodes the audio again. The current transcript is kept until the new one is ready."
                      : "The audio was deleted, so this meeting cannot be transcribed again."}
                  </p>
                </div>

                <div className="flex flex-wrap items-center justify-end gap-1.5 border-t border-zinc-200 dark:border-zinc-800 pt-3">
                  {confirming === "audio" ? (
                    <ConfirmInline
                      question="Delete the audio? The transcript stays."
                      confirmLabel="Delete audio"
                      busy={busy}
                      onConfirm={() => void deleteAudio()}
                      onCancel={() => setConfirming(null)}
                    />
                  ) : confirming === "meeting" ? (
                    <ConfirmInline
                      question="Delete the meeting, transcript and audio?"
                      confirmLabel="Delete meeting"
                      busy={busy}
                      onConfirm={() => void deleteMeeting()}
                      onCancel={() => setConfirming(null)}
                    />
                  ) : (
                    <>
                      <button
                        type="button"
                        onClick={() => setConfirming("audio")}
                        disabled={busy || processing || !meeting.has_audio}
                        className={secondaryButton}
                      >
                        Delete audio
                        {meeting.has_audio && meeting.audio_bytes > 0
                          ? ` (${formatBytes(meeting.audio_bytes)})`
                          : ""}
                      </button>
                      <button
                        type="button"
                        onClick={() => setConfirming("meeting")}
                        disabled={busy}
                        className={dangerButton}
                      >
                        Delete meeting
                      </button>
                    </>
                  )}
                </div>
              </section>
            )}
          </>
        )}
      </div>
    </div>
  );
}

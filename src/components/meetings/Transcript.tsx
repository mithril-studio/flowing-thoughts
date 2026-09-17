import { useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Segment } from "../../types/meetings";
import { formatDuration, hiddenReasonLabel, sortSegments } from "./format";
import { ghostButton } from "./ui";

/** DOM id of a segment row, so the summary can scroll to its sources. */
export const segmentDomId = (segmentId: string) => `meeting-segment-${segmentId}`;

interface TranscriptProps {
  segments: Segment[];
  showHidden: boolean;
  onShowHiddenChange: (show: boolean) => void;
  highlightedIds: ReadonlySet<string>;
  /** `null` drops the edit and brings the decoded text back. */
  onEditText: (segmentId: string, text: string | null) => Promise<void>;
  onSetHidden: (segmentId: string, hidden: boolean) => Promise<void>;
}

export default function Transcript({
  segments,
  showHidden,
  onShowHiddenChange,
  highlightedIds,
  onEditText,
  onSetHidden,
}: TranscriptProps) {
  const ordered = useMemo(() => sortSegments(segments), [segments]);
  const hiddenCount = ordered.filter((s) => s.hidden).length;
  const shown = showHidden ? ordered : ordered.filter((s) => !s.hidden);

  return (
    <section aria-label="Transcript" className="space-y-2">
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-[11px] font-medium uppercase tracking-wider text-zinc-600 dark:text-zinc-400">
          Transcript
        </h3>
        {hiddenCount > 0 && (
          <button
            type="button"
            aria-expanded={showHidden}
            onClick={() => onShowHiddenChange(!showHidden)}
            className={ghostButton}
          >
            {showHidden ? `Hide ${hiddenCount} hidden` : `Show ${hiddenCount} hidden`}
          </button>
        )}
      </div>
      {shown.length === 0 ? (
        <p className="text-xs text-zinc-500">
          {hiddenCount > 0
            ? "Every segment was flagged and is hidden."
            : "No speech was transcribed."}
        </p>
      ) : (
        <ol className="space-y-1">
          {shown.map((segment) => (
            <SegmentRow
              key={segment.id}
              segment={segment}
              highlighted={highlightedIds.has(segment.id)}
              onEditText={onEditText}
              onSetHidden={onSetHidden}
            />
          ))}
        </ol>
      )}
    </section>
  );
}

function SegmentRow({
  segment,
  highlighted,
  onEditText,
  onSetHidden,
}: {
  segment: Segment;
  highlighted: boolean;
  onEditText: TranscriptProps["onEditText"];
  onSetHidden: TranscriptProps["onSetHidden"];
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(segment.text);
  const cancelled = useRef(false);
  const textarea = useRef<HTMLTextAreaElement>(null);
  const timestamp = formatDuration(segment.start_ms);
  const mine = segment.track_kind === "mic";
  const edited = segment.original_text !== null;

  // Grow the editor with its text; one row is never enough for a long segment.
  useLayoutEffect(() => {
    const el = textarea.current;
    if (!editing || !el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [editing, draft]);

  const beginEdit = () => {
    cancelled.current = false;
    setDraft(segment.text);
    setEditing(true);
  };

  const finishEdit = () => {
    setEditing(false);
    if (cancelled.current) return;
    const next = draft.trim();
    if (next === segment.text) return;
    // Emptying the text, or typing the decoded text back, removes the edit.
    const revert = next === "" || next === segment.original_text;
    if (revert && !edited) return;
    void onEditText(segment.id, revert ? null : next);
  };

  return (
    <li
      id={segmentDomId(segment.id)}
      data-testid="meeting-segment"
      className={`group rounded-lg px-2 py-1.5 transition-colors ${
        highlighted
          ? "bg-emerald-50 ring-1 ring-emerald-300 dark:bg-emerald-950/30 dark:ring-emerald-800"
          : ""
      } ${segment.hidden ? "opacity-50" : ""}`}
    >
      <div className="flex items-center gap-2">
        <span className="font-mono text-[10px] tabular-nums text-zinc-500">{timestamp}</span>
        <span
          className={`text-[11px] font-medium ${
            mine
              ? "text-emerald-700 dark:text-emerald-400"
              : "text-zinc-600 dark:text-zinc-300"
          }`}
        >
          {segment.speaker_label}
        </span>
        {segment.hidden && (
          <span className="rounded-full bg-zinc-200 dark:bg-zinc-800 px-1.5 py-px text-[10px] text-zinc-600 dark:text-zinc-400">
            {hiddenReasonLabel(segment.suppressed_reason)}
          </span>
        )}
        {edited && <span className="text-[10px] text-zinc-500">edited</span>}
        <span className="ml-auto flex items-center opacity-0 transition-opacity focus-within:opacity-100 group-hover:opacity-100">
          {!editing && (
            <button
              type="button"
              aria-label={`Edit segment at ${timestamp}`}
              onClick={beginEdit}
              className={ghostButton}
            >
              Edit
            </button>
          )}
          {edited && !editing && (
            <button
              type="button"
              aria-label={`Revert segment at ${timestamp}`}
              onClick={() => void onEditText(segment.id, null)}
              className={ghostButton}
            >
              Revert
            </button>
          )}
          <button
            type="button"
            aria-label={`${segment.hidden ? "Unhide" : "Hide"} segment at ${timestamp}`}
            onClick={() => void onSetHidden(segment.id, !segment.hidden)}
            className={ghostButton}
          >
            {segment.hidden ? "Unhide" : "Hide"}
          </button>
        </span>
      </div>
      {editing ? (
        <textarea
          ref={textarea}
          autoFocus
          rows={2}
          aria-label={`Segment text at ${timestamp}`}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={finishEdit}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              cancelled.current = true;
              e.currentTarget.blur();
            }
          }}
          className="mt-1 w-full resize-none overflow-hidden rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 p-2 text-xs leading-relaxed text-zinc-900 dark:text-zinc-100 outline-none focus:border-zinc-500"
        />
      ) : (
        <p
          onClick={() => {
            // A drag that selected text is a copy gesture, not an edit.
            if (!window.getSelection()?.toString()) beginEdit();
          }}
          className="mt-0.5 cursor-text select-text text-xs leading-relaxed text-zinc-800 dark:text-zinc-200"
        >
          {segment.text}
        </p>
      )}
    </li>
  );
}

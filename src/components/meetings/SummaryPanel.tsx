import { useState } from "react";
import type { MeetingSummary, Segment, SummaryItem, SummaryItemKind } from "../../types/meetings";
import { formatDuration } from "./format";
import { ghostButton, primaryButton, secondaryButton } from "./ui";

interface SummaryPanelProps {
  summary: MeetingSummary | null;
  /** The OpenRouter model id from settings, named in the consent line. */
  model: string;
  segments: Segment[];
  /** False while there is nothing to summarize yet. */
  canGenerate: boolean;
  generating: boolean;
  error: string | null;
  /** Called only after the user confirmed sending the transcript. */
  onGenerate: () => void;
  onShowSources: (segmentIds: string[]) => void;
}

const GROUPS: { kind: SummaryItemKind; title: string }[] = [
  { kind: "decision", title: "Decisions" },
  { kind: "action", title: "Action items" },
  { kind: "topic", title: "Topics" },
];

export default function SummaryPanel({
  summary,
  model,
  segments,
  canGenerate,
  generating,
  error,
  onGenerate,
  onShowSources,
}: SummaryPanelProps) {
  const [confirming, setConfirming] = useState(false);
  const pending = generating || summary?.status === "pending";
  const done = summary?.status === "done";
  const startMs = new Map(segments.map((s) => [s.id, s.start_ms]));

  return (
    <section
      aria-label="Summary"
      className="space-y-2 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-900/60 p-3"
    >
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-[11px] font-medium uppercase tracking-wider text-zinc-600 dark:text-zinc-400">
          Summary
        </h3>
        {done && summary && (
          <span className="truncate text-[10px] text-zinc-500">
            {summary.provider} · {summary.model}
          </span>
        )}
      </div>

      {done && summary && (
        <div className="space-y-3">
          {summary.overview && (
            <p className="text-xs leading-relaxed text-zinc-800 dark:text-zinc-200">
              {summary.overview}
            </p>
          )}
          {GROUPS.map(({ kind, title }) => {
            const items = summary.items.filter((item) => item.kind === kind);
            if (items.length === 0) return null;
            return (
              <div key={kind} className="space-y-1">
                <h4 className="text-xs font-medium text-zinc-700 dark:text-zinc-300">{title}</h4>
                <ul className="space-y-1">
                  {items.map((item) => (
                    <SummaryItemRow
                      key={item.id}
                      item={item}
                      startMs={startMs}
                      onShowSources={onShowSources}
                    />
                  ))}
                </ul>
              </div>
            );
          })}
          {summary.items.length === 0 && !summary.overview && (
            <p className="text-xs text-zinc-500">The summary came back empty.</p>
          )}
        </div>
      )}

      {summary?.status === "failed" && !pending && (
        <p className="text-xs text-red-700 dark:text-red-300">
          {summary.error ?? "The summary failed."}
        </p>
      )}
      {error && <p className="text-xs text-red-700 dark:text-red-300">{error}</p>}

      <p className="text-[11px] leading-relaxed text-zinc-500">
        Generating a summary sends this meeting's full transcript to OpenRouter ({model}) with
        your own API key. Nothing is sent until you confirm.
      </p>

      {pending ? (
        <p role="status" className="text-xs text-zinc-600 dark:text-zinc-400">
          Summarizing…
        </p>
      ) : confirming ? (
        <div
          role="group"
          aria-label="Confirm sending the transcript"
          className="flex flex-wrap items-center gap-1.5"
        >
          <span className="text-xs text-zinc-700 dark:text-zinc-300">
            Send the transcript to OpenRouter?
          </span>
          <button
            type="button"
            onClick={() => {
              setConfirming(false);
              onGenerate();
            }}
            className={primaryButton}
          >
            Send and summarize
          </button>
          <button type="button" onClick={() => setConfirming(false)} className={ghostButton}>
            Cancel
          </button>
        </div>
      ) : (
        <button
          type="button"
          onClick={() => setConfirming(true)}
          disabled={!canGenerate}
          className={done ? secondaryButton : primaryButton}
        >
          {done ? "Regenerate summary" : "Generate summary"}
        </button>
      )}
    </section>
  );
}

function SummaryItemRow({
  item,
  startMs,
  onShowSources,
}: {
  item: SummaryItem;
  startMs: Map<string, number>;
  onShowSources: (segmentIds: string[]) => void;
}) {
  // Only sources that are still part of the shown run can be scrolled to.
  const sources = item.source_segment_ids
    .filter((id) => startMs.has(id))
    .sort((a, b) => (startMs.get(a) ?? 0) - (startMs.get(b) ?? 0));
  const first = sources.length > 0 ? formatDuration(startMs.get(sources[0]) ?? 0) : null;

  const body = (
    <>
      <span className="block text-xs leading-snug text-zinc-800 dark:text-zinc-200">
        {item.text}
      </span>
      {item.kind === "action" && (
        <span className="block text-[11px] text-zinc-500">
          Owner: {item.owner ?? "unknown"} · Due: {item.due_date ?? "unknown"}
        </span>
      )}
      {first && (
        <span className="block text-[11px] text-emerald-700 dark:text-emerald-400">
          {sources.length === 1 ? `Source at ${first}` : `${sources.length} sources from ${first}`}
        </span>
      )}
    </>
  );

  return (
    <li className="border-t border-zinc-200/70 dark:border-zinc-800/60 pt-1">
      {first ? (
        <button
          type="button"
          onClick={() => onShowSources(sources)}
          className="w-full rounded-lg px-1 py-0.5 text-left hover:bg-zinc-100 dark:hover:bg-zinc-800/60"
        >
          {body}
        </button>
      ) : (
        <div className="px-1 py-0.5">{body}</div>
      )}
    </li>
  );
}

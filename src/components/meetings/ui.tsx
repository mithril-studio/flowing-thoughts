// Small shared pieces of the Meetings UI: button looks, the state badge, the
// error banner and the in-page confirm step (the app never uses browser dialogs).

import type { MeetingListItem } from "../../types/meetings";
import { statusBadge, type BadgeTone } from "./format";

export const primaryButton =
  "rounded-lg bg-zinc-900 px-3 py-1.5 text-xs font-medium text-white hover:bg-zinc-800 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-white disabled:cursor-not-allowed disabled:opacity-50";

export const secondaryButton =
  "rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-900 px-2.5 py-1.5 text-xs text-zinc-800 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-50";

export const dangerButton =
  "rounded-lg border border-red-200 dark:border-red-900/60 bg-red-50 dark:bg-red-950/40 px-2.5 py-1.5 text-xs text-red-700 dark:text-red-300 hover:bg-red-100 dark:hover:bg-red-950/70 disabled:cursor-not-allowed disabled:opacity-50";

export const ghostButton =
  "rounded-lg px-2 py-1 text-xs text-zinc-500 hover:bg-zinc-100 hover:text-zinc-900 dark:hover:bg-zinc-800 dark:hover:text-zinc-200 disabled:cursor-not-allowed disabled:opacity-50";

const BADGE_TONES: Record<BadgeTone, string> = {
  red: "bg-red-100 text-red-700 dark:bg-red-950/60 dark:text-red-300",
  amber: "bg-amber-100 text-amber-800 dark:bg-amber-950/50 dark:text-amber-300",
  emerald: "bg-emerald-100 text-emerald-700 dark:bg-emerald-900/60 dark:text-emerald-300",
  zinc: "bg-zinc-200 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-400",
};

export function StatusBadge({ meeting }: { meeting: Pick<MeetingListItem, "status" | "job"> }) {
  const { label, tone } = statusBadge(meeting);
  return (
    <span
      data-testid="meeting-status"
      className={`shrink-0 rounded-full px-2 py-px text-[10px] font-medium ${BADGE_TONES[tone]}`}
    >
      {label}
    </span>
  );
}

export function ErrorBanner({ message, onDismiss }: { message: string; onDismiss: () => void }) {
  return (
    <div
      role="alert"
      className="flex items-start justify-between gap-2 rounded-xl border border-red-200 dark:border-red-900/60 bg-red-50 dark:bg-red-950/40 p-3 text-xs text-red-700 dark:text-red-300"
    >
      <span className="min-w-0 break-words">{message}</span>
      <button
        type="button"
        onClick={onDismiss}
        className="shrink-0 text-red-600 hover:text-red-800 dark:text-red-400 dark:hover:text-red-200"
      >
        Dismiss
      </button>
    </div>
  );
}

/** The second step of a destructive action, shown in place of its button. */
export function ConfirmInline({
  question,
  confirmLabel,
  busy,
  onConfirm,
  onCancel,
}: {
  question: string;
  confirmLabel: string;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <div role="group" aria-label={question} className="flex flex-wrap items-center justify-end gap-1.5">
      <span className="text-xs text-zinc-600 dark:text-zinc-400">{question}</span>
      <button type="button" onClick={onConfirm} disabled={busy} className={dangerButton}>
        {confirmLabel}
      </button>
      <button type="button" onClick={onCancel} disabled={busy} className={ghostButton}>
        Cancel
      </button>
    </div>
  );
}

import { useState } from "react";
import type { MeetingListItem } from "../../types/meetings";
import { formatClockTime, formatDuration, groupByDay } from "./format";
import { ConfirmInline, StatusBadge } from "./ui";

interface MeetingListProps {
  meetings: MeetingListItem[];
  onOpen: (meetingId: string) => void;
  onDelete: (meetingId: string) => Promise<void>;
}

export default function MeetingList({ meetings, onOpen, onDelete }: MeetingListProps) {
  const [confirmingId, setConfirmingId] = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);

  if (meetings.length === 0) {
    return (
      <div className="flex h-full flex-col items-center justify-center px-6 text-center text-zinc-500">
        <p className="text-sm">No meetings yet</p>
        <p className="mt-1 text-xs leading-relaxed">
          Start a meeting to record your microphone and the call. It is transcribed on this Mac
          after you stop.
        </p>
      </div>
    );
  }

  const confirmDelete = async (meetingId: string) => {
    setDeletingId(meetingId);
    try {
      await onDelete(meetingId);
    } finally {
      setDeletingId(null);
      setConfirmingId(null);
    }
  };

  return (
    <div className="space-y-4">
      {groupByDay(meetings).map((group) => (
        <section key={group.key} aria-label={group.label}>
          <h3 className="mb-2 text-xs font-medium text-zinc-500">{group.label}</h3>
          <ul className="space-y-2">
            {group.items.map((meeting) => {
              const live = meeting.status === "recording" || meeting.status === "paused";
              const title = meeting.title.trim() || "Untitled meeting";
              return (
                <li
                  key={meeting.id}
                  className="group rounded-lg border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 transition-colors hover:border-zinc-400 dark:hover:border-zinc-600"
                >
                  {confirmingId === meeting.id ? (
                    <div className="flex items-center justify-between gap-2 p-3">
                      <p className="min-w-0 truncate text-sm text-zinc-800 dark:text-zinc-200">
                        {title}
                      </p>
                      <ConfirmInline
                        question="Delete meeting and audio?"
                        confirmLabel="Delete"
                        busy={deletingId === meeting.id}
                        onConfirm={() => void confirmDelete(meeting.id)}
                        onCancel={() => setConfirmingId(null)}
                      />
                    </div>
                  ) : (
                    <div className="flex items-center gap-2 pr-2">
                      <button
                        type="button"
                        onClick={() => onOpen(meeting.id)}
                        className="min-w-0 flex-1 rounded-lg p-3 text-left"
                      >
                        <span className="flex items-center gap-2">
                          <span className="min-w-0 truncate text-sm text-zinc-800 dark:text-zinc-200">
                            {title}
                          </span>
                          <StatusBadge meeting={meeting} />
                        </span>
                        <span className="mt-0.5 block text-xs text-zinc-500">
                          {formatClockTime(meeting.started_at)}
                          {!live && ` · ${formatDuration(meeting.duration_ms)}`}
                        </span>
                      </button>
                      {!live && (
                        <button
                          type="button"
                          aria-label={`Delete ${title}`}
                          onClick={() => setConfirmingId(meeting.id)}
                          className="shrink-0 rounded-lg bg-zinc-100 dark:bg-zinc-800 px-2 py-1 text-xs text-red-600 dark:text-red-400 opacity-0 transition-opacity hover:text-red-700 dark:hover:text-red-300 focus-visible:opacity-100 group-hover:opacity-100"
                        >
                          Delete
                        </button>
                      )}
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        </section>
      ))}
    </div>
  );
}

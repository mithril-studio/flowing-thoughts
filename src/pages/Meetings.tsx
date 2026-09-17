// Placeholder. OWNER: WP8 replaces this file with the real Meetings page
// (list by day, detail with start/pause/stop, progress, Me/Them transcript),
// built on src/lib/meetingsApi.ts. App.tsx already mounts it behind
// settings.meetings.enabled; WP8 does not need to touch App.tsx or TabBar.tsx.

import type { AppSettings } from "../types/settings";

interface MeetingsProps {
  settings: AppSettings;
}

export default function Meetings({ settings }: MeetingsProps) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-2 px-8 text-center">
      <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">Meetings</h2>
      <p className="text-xs leading-relaxed text-zinc-500">
        Record a call, then transcribe it on this Mac with {settings.meetings.model}. Coming
        soon.
      </p>
    </div>
  );
}

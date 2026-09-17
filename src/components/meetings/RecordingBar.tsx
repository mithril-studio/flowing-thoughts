import { useEffect, useMemo, useState } from "react";
import type { PermissionStatus, RecordingStatus } from "../../types/meetings";
import { formatDuration } from "./format";
import { primaryButton, secondaryButton } from "./ui";

interface RecordingBarProps {
  status: RecordingStatus;
  permission: PermissionStatus | null;
  busy: boolean;
  onPause: () => void;
  onResume: () => void;
  onStop: () => void;
  onOpenSystemAudioSettings: () => void;
}

/**
 * The backend reports recorded time at the moment of each `meeting-state`
 * event; between events the bar only ticks the display on from there.
 */
function useElapsedMs(status: RecordingStatus): number {
  const receivedAt = useMemo(() => Date.now(), [status]);
  const [now, setNow] = useState(receivedAt);
  const ticking = status.phase === "recording";

  useEffect(() => {
    setNow(Date.now());
    if (!ticking) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [ticking, receivedAt]);

  return status.elapsed_ms + (ticking ? Math.max(0, now - receivedAt) : 0);
}

const PHASE_LABEL: Record<RecordingStatus["phase"], string> = {
  idle: "Idle",
  starting: "Starting…",
  recording: "Recording",
  paused: "Paused",
  stopping: "Finishing…",
};

export default function RecordingBar({
  status,
  permission,
  busy,
  onPause,
  onResume,
  onStop,
  onOpenSystemAudioSettings,
}: RecordingBarProps) {
  const elapsedMs = useElapsedMs(status);
  const { phase } = status;
  const live = phase === "recording" || phase === "paused";
  const denied = permission?.state === "denied";
  const micOnly = live && !status.tracks.includes("system");
  const showSilent = status.system_audio_silent && !denied;

  return (
    <section
      aria-label="Meeting recording"
      className="space-y-2 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-900/60 p-3"
    >
      <div className="flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <span
            aria-hidden="true"
            className={`h-2.5 w-2.5 shrink-0 rounded-full ${
              phase === "recording"
                ? "animate-pulse bg-red-500 dark:bg-red-400"
                : phase === "paused"
                  ? "bg-amber-500 dark:bg-amber-400"
                  : "bg-zinc-400 dark:bg-zinc-600"
            }`}
          />
          <span className="text-xs text-zinc-600 dark:text-zinc-400">{PHASE_LABEL[phase]}</span>
          <span
            aria-label="Elapsed time"
            className="font-mono text-sm tabular-nums text-zinc-900 dark:text-zinc-100"
          >
            {formatDuration(elapsedMs)}
          </span>
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          {phase === "paused" ? (
            <button type="button" onClick={onResume} disabled={busy} className={secondaryButton}>
              Resume
            </button>
          ) : (
            <button
              type="button"
              onClick={onPause}
              disabled={busy || phase !== "recording"}
              className={secondaryButton}
            >
              Pause
            </button>
          )}
          <button type="button" onClick={onStop} disabled={busy || !live} className={primaryButton}>
            Stop
          </button>
        </div>
      </div>

      {(status.echo_risk || showSilent || denied || micOnly) && (
        <div role="status" className="space-y-1.5 border-t border-zinc-200 dark:border-zinc-800 pt-2">
          {status.echo_risk && (
            <p className="text-xs text-zinc-600 dark:text-zinc-400">
              Use headphones for best results. Over speakers, the other side leaks into your
              microphone.
            </p>
          )}
          {denied ? (
            <div className="flex items-center justify-between gap-2">
              <p className="text-xs text-amber-700 dark:text-amber-300">
                System audio permission denied, recording microphone only.
              </p>
              <button
                type="button"
                onClick={onOpenSystemAudioSettings}
                className={`shrink-0 ${secondaryButton}`}
              >
                Open System Settings
              </button>
            </div>
          ) : (
            micOnly && (
              <p className="text-xs text-amber-700 dark:text-amber-300">
                System audio is unavailable, recording microphone only.
              </p>
            )
          )}
          {showSilent && (
            <div className="flex items-center justify-between gap-2">
              <p className="text-xs text-amber-700 dark:text-amber-300">
                No system audio detected. If the call is not silent, allow System Audio Recording
                for FlowingThoughts.
              </p>
              <button
                type="button"
                onClick={onOpenSystemAudioSettings}
                className={`shrink-0 ${secondaryButton}`}
              >
                Open System Settings
              </button>
            </div>
          )}
        </div>
      )}
    </section>
  );
}

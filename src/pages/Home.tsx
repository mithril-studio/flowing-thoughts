import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import RecordingIndicator from "../components/RecordingIndicator";

interface TranscriptionEntry {
  id: number;
  text: string;
  timestamp: string;
}

type SessionPhase = "idle" | "recording" | "transcribing" | "injecting" | "error";

interface PersistedStateView {
  history: { session_id: number; text: string; timestamp: string }[];
}

export default function Home() {
  const [phase, setPhase] = useState<SessionPhase>("idle");
  const [transcriptions, setTranscriptions] = useState<TranscriptionEntry[]>([]);
  const [lastError, setLastError] = useState<string | null>(null);

  useEffect(() => {
    invoke<PersistedStateView>("get_persisted_state")
      .then((state) => {
        setTranscriptions(
          state.history.map((entry) => ({
            id: entry.session_id,
            text: entry.text,
            timestamp: entry.timestamp,
          }))
        );
      })
      .catch(() => {
        // Keep empty state if loading fails.
      });

    const unlistenPhase = listen<{ phase: SessionPhase }>(
      "session-phase",
      (event) => {
        setPhase(event.payload.phase);
      }
    );
    const unlistenComplete = listen<{
      session_id: number;
      text: string;
      timestamp: string;
    }>("transcription-complete", (event) => {
      setTranscriptions((prev) => [
        {
          id: event.payload.session_id,
          text: event.payload.text,
          timestamp: event.payload.timestamp,
        },
        ...prev,
      ]);
      setPhase("idle");
    });
    const unlistenErrorDetails = listen<{ stage: string; message: string }>(
      "pipeline-error",
      (event) => {
        setLastError(`[${event.payload.stage}] ${event.payload.message}`);
        setPhase("error");
      }
    );

    return () => {
      unlistenPhase.then((fn) => fn());
      unlistenComplete.then((fn) => fn());
      unlistenErrorDetails.then((fn) => fn());
    };
  }, []);

  const formatTime = (iso: string) => {
    const d = new Date(iso);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  };

  const formatDate = (iso: string) => {
    const d = new Date(iso);
    const today = new Date();
    if (d.toDateString() === today.toDateString()) return "Today";
    const yesterday = new Date(today);
    yesterday.setDate(today.getDate() - 1);
    if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
    return d.toLocaleDateString([], { month: "short", day: "numeric" });
  };

  // Group transcriptions by date
  const grouped = transcriptions.reduce<Record<string, TranscriptionEntry[]>>(
    (acc, t) => {
      const key = formatDate(t.timestamp);
      if (!acc[key]) acc[key] = [];
      acc[key].push(t);
      return acc;
    },
    {}
  );

  return (
    <div className="flex flex-col h-full">
      {/* Recording indicator */}
      <div className="flex justify-center py-4">
        <RecordingIndicator mode={phase} />
      </div>

      {lastError && (
        <div className="px-4 pb-3">
          <div className="rounded-lg border border-red-900 bg-red-950/40 p-3 text-xs text-red-300 flex items-start justify-between gap-2">
            <span>{lastError}</span>
            <button
              onClick={() => setLastError(null)}
              className="text-red-400 hover:text-red-200 transition-colors"
            >
              Dismiss
            </button>
          </div>
        </div>
      )}

      {/* Timeline */}
      <div className="flex-1 overflow-y-auto px-4 pb-4">
        {Object.keys(grouped).length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full text-neutral-500">
            <p className="text-sm">No transcriptions yet</p>
            <p className="text-xs mt-1">
              Hold <kbd className="px-1 py-0.5 bg-neutral-800 rounded">Cmd+Shift+Space</kbd> and speak
            </p>
            {(phase === "transcribing" || phase === "injecting") && (
              <p className="text-xs mt-1 text-amber-400">
                {phase === "injecting" ? "Typing into focused app..." : "Working on transcription..."}
              </p>
            )}
          </div>
        ) : (
          Object.entries(grouped).map(([date, entries]) => (
            <div key={date} className="mb-4">
              <h3 className="text-xs text-neutral-500 font-medium mb-2 sticky top-0 bg-neutral-950 py-1">
                {date}
              </h3>
              <div className="space-y-2">
                {entries.map((entry) => (
                  <button
                    key={entry.id}
                    onClick={() => navigator.clipboard.writeText(entry.text)}
                    className="w-full text-left p-3 rounded-lg bg-neutral-900 border border-neutral-800 hover:border-neutral-700 transition-colors group"
                  >
                    <p className="text-sm text-neutral-200 leading-relaxed">
                      {entry.text}
                    </p>
                    <div className="flex justify-between items-center mt-2">
                      <span className="text-xs text-neutral-600">
                        {formatTime(entry.timestamp)}
                      </span>
                      <span className="text-xs text-neutral-600 opacity-0 group-hover:opacity-100 transition-opacity">
                        Click to copy
                      </span>
                    </div>
                  </button>
                ))}
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

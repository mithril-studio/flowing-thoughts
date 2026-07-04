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

interface HomeProps {
  shortcutLabel: string;
}

export default function Home({ shortcutLabel }: HomeProps) {
  const [phase, setPhase] = useState<SessionPhase>("idle");
  const [amplitude, setAmplitude] = useState(0);
  const [transcriptions, setTranscriptions] = useState<TranscriptionEntry[]>([]);
  const [lastError, setLastError] = useState<string | null>(null);
  const [copiedId, setCopiedId] = useState<number | null>(null);
  const [editingId, setEditingId] = useState<number | null>(null);
  const [editDraft, setEditDraft] = useState<string>("");
  const [editSaving, setEditSaving] = useState(false);

  const handleCopy = (id: number, text: string) => {
    void invoke("copy_to_clipboard", { text })
      .then(() => {
        setCopiedId(id);
        setTimeout(() => {
          setCopiedId((current) => (current === id ? null : current));
        }, 1500);
      })
      .catch((e) => setLastError(String(e)));
  };

  const beginEdit = (entry: TranscriptionEntry) => {
    setEditingId(entry.id);
    setEditDraft(entry.text);
  };

  const cancelEdit = () => {
    setEditingId(null);
    setEditDraft("");
  };

  const saveEdit = async (entry: TranscriptionEntry) => {
    const original = entry.text;
    const edited = editDraft;
    if (edited === original) {
      cancelEdit();
      return;
    }
    setEditSaving(true);
    try {
      await invoke("update_history_text", {
        sessionId: entry.id,
        newText: edited,
      });
      setTranscriptions((prev) =>
        prev.map((t) => (t.id === entry.id ? { ...t, text: edited } : t))
      );
      await invoke("save_correction_from_edit", {
        sessionId: entry.id,
        dictationId: null,
        model: null,
        original,
        edited,
      });
      cancelEdit();
    } catch (e) {
      setLastError(String(e));
    } finally {
      setEditSaving(false);
    }
  };

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
        if (event.payload.phase !== "recording") {
          setAmplitude(0);
        }
      }
    );
    const unlistenAmplitude = listen<{ amplitude: number }>(
      "recording-amplitude",
      (event) => {
        setAmplitude(event.payload.amplitude ?? 0);
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
      setAmplitude(0);
    });
    const unlistenErrorDetails = listen<{ stage: string; message: string }>(
      "pipeline-error",
      (event) => {
        setLastError(`[${event.payload.stage}] ${event.payload.message}`);
        setPhase("error");
        setAmplitude(0);
      }
    );

    return () => {
      unlistenPhase.then((fn) => fn());
      unlistenAmplitude.then((fn) => fn());
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
      <div className="flex justify-center py-6">
        <RecordingIndicator
          mode={phase}
          amplitude={amplitude}
          idleLabel={`Hold ${shortcutLabel} to speak`}
        />
      </div>

      {lastError && (
        <div className="px-4 pb-3">
          <div className="rounded-lg border border-red-200 dark:border-red-900 bg-red-50 dark:bg-red-950/40 p-3 text-xs text-red-700 dark:text-red-300 flex items-start justify-between gap-2">
            <span>{lastError}</span>
            <button
              onClick={() => setLastError(null)}
              className="text-red-600 dark:text-red-400 hover:text-red-200 transition-colors"
            >
              Dismiss
            </button>
          </div>
        </div>
      )}

      {/* Timeline */}
      <div className="flex-1 overflow-y-auto px-4 pb-4">
        {Object.keys(grouped).length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full text-zinc-500">
            <p className="text-sm">No transcriptions yet</p>
            <p className="text-xs mt-1">
              Hold <kbd className="px-1 py-0.5 bg-zinc-200 dark:bg-zinc-800 rounded">{shortcutLabel}</kbd> and speak
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
              <h3 className="text-xs text-zinc-500 font-medium mb-2 sticky top-0 bg-white dark:bg-zinc-950 py-1">
                {date}
              </h3>
              <div className="space-y-2">
                {entries.map((entry) => {
                  const isEditing = editingId === entry.id;
                  return (
                    <div
                      key={entry.id}
                      className="w-full p-3 rounded-lg bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 hover:border-zinc-700 transition-colors group"
                    >
                      {isEditing ? (
                        <textarea
                          value={editDraft}
                          onChange={(e) => setEditDraft(e.target.value)}
                          rows={Math.min(
                            8,
                            Math.max(2, editDraft.split("\n").length)
                          )}
                          disabled={editSaving}
                          className="w-full text-sm text-zinc-900 dark:text-zinc-100 bg-white dark:bg-zinc-950 border border-zinc-300 dark:border-zinc-700 rounded p-2 outline-none focus:border-zinc-500 resize-y"
                        />
                      ) : (
                        <p className="text-sm text-zinc-800 dark:text-zinc-200 leading-relaxed select-text cursor-text">
                          {entry.text}
                        </p>
                      )}
                      <div className="flex justify-between items-center mt-2">
                        <span className="text-xs text-zinc-600">
                          {formatTime(entry.timestamp)}
                        </span>
                        <div className="flex items-center gap-1">
                          {isEditing ? (
                            <>
                              <button
                                type="button"
                                onClick={cancelEdit}
                                disabled={editSaving}
                                className="text-xs px-2 py-0.5 rounded text-zinc-500 hover:text-zinc-900 dark:hover:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:opacity-50"
                              >
                                Cancel
                              </button>
                              <button
                                type="button"
                                onClick={() => void saveEdit(entry)}
                                disabled={editSaving}
                                className="text-xs px-2 py-0.5 rounded bg-emerald-100 dark:bg-emerald-800/60 text-emerald-800 dark:text-emerald-200 hover:bg-emerald-200 dark:hover:bg-emerald-700/60 disabled:opacity-50"
                              >
                                {editSaving ? "Saving…" : "Save"}
                              </button>
                            </>
                          ) : (
                            <>
                              <button
                                type="button"
                                onClick={() => beginEdit(entry)}
                                className="text-xs px-2 py-0.5 rounded text-zinc-500 hover:text-zinc-900 dark:hover:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800"
                              >
                                Edit
                              </button>
                              <button
                                type="button"
                                onClick={() => handleCopy(entry.id, entry.text)}
                                className={`text-xs px-2 py-0.5 rounded transition-colors ${
                                  copiedId === entry.id
                                    ? "bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-300"
                                    : "text-zinc-500 hover:text-zinc-900 dark:hover:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800"
                                }`}
                              >
                                {copiedId === entry.id ? "Copied" : "Copy"}
                              </button>
                            </>
                          )}
                        </div>
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

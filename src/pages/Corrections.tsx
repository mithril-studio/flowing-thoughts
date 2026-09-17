import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface MistranscribedWord {
  model: string;
  wrong_text: string;
  intended_text: string;
  occurrences: number;
}

interface Dictation {
  id: string;
  session_id: number;
  started_at: string;
  wav_path: string | null;
  duration_ms: number | null;
  sample_rate: number | null;
}

interface TranscriptionRow {
  id: string;
  dictation_id: string;
  model: string;
  text: string | null;
  latency_ms: number | null;
  error: string | null;
  created_at: string;
}

interface LabSessionSummary {
  dictation: Dictation;
  transcriptions: TranscriptionRow[];
}

interface CorrectionRow {
  id: string;
  dictation_id: string;
  model: string;
  wrong_text: string;
  intended_text: string;
  context_snippet: string | null;
  created_at: string;
}

function formatLatency(ms: number | null): string {
  if (ms == null) return "—";
  if (ms < 1000) return `${ms} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

/**
 * Everything FlowingThoughts has learned from your edits: corrections that
 * get applied to future dictations, plus recent dictation runs for debugging.
 */
export default function Corrections() {
  const [topWords, setTopWords] = useState<MistranscribedWord[]>([]);
  const [recent, setRecent] = useState<LabSessionSummary[]>([]);
  const [corrections, setCorrections] = useState<CorrectionRow[]>([]);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [w, r, c] = await Promise.all([
        invoke<MistranscribedWord[]>("get_top_mistranscribed", { limit: 10 }),
        invoke<LabSessionSummary[]>("list_lab_sessions", { limit: 15 }),
        invoke<CorrectionRow[]>("list_corrections"),
      ]);
      setTopWords(w);
      setRecent(r);
      setCorrections(c);
      setErrorMsg(null);
    } catch (e) {
      setErrorMsg(String(e));
    }
  }, []);

  const deleteCorrection = async (id: string) => {
    try {
      await invoke("delete_correction", { id });
      setCorrections((prev) => prev.filter((c) => c.id !== id));
    } catch (e) {
      setErrorMsg(String(e));
    }
  };

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <div className="h-full overflow-y-auto px-4 py-4 space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">Learned words</h2>
          <p className="text-xs text-zinc-500 mt-0.5">
            Fix a dictation once — in the target app or on Home — and it's
            applied to every future dictation.
          </p>
        </div>
        <button
          type="button"
          onClick={() => void refresh()}
          className="rounded-lg border border-zinc-200 dark:border-zinc-800 px-2.5 py-1 text-[11px] text-zinc-600 dark:text-zinc-400 hover:bg-zinc-900 hover:text-zinc-900 dark:hover:text-zinc-200"
        >
          Refresh
        </button>
      </div>

      {errorMsg && (
        <div className="rounded-xl border border-red-200 dark:border-red-900/60 bg-red-50 dark:bg-red-950/40 p-3 text-xs text-red-700 dark:text-red-300">
          {errorMsg}
        </div>
      )}

      <section className="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-900/60 p-3 space-y-2">
        <h3 className="text-[11px] font-medium text-zinc-600 dark:text-zinc-400 uppercase tracking-wider">
          Corrections
        </h3>
        {corrections.length === 0 ? (
          <p className="text-xs text-zinc-500">
            Nothing learned yet. Fix a word in the pasted text (within 10
            minutes, before your next dictation) or edit a dictation on Home.
          </p>
        ) : (
          <ul className="space-y-1">
            {corrections.map((c) => (
              <li
                key={c.id}
                className="flex items-center justify-between gap-2 border-t border-zinc-200/70 dark:border-zinc-800/60 pt-1.5 text-xs"
              >
                <div className="flex min-w-0 flex-col gap-0.5">
                  <span className="truncate font-mono text-emerald-700 dark:text-emerald-300">
                    {c.wrong_text} → {c.intended_text}
                  </span>
                  <span className="text-[10px] text-zinc-600">
                    {new Date(c.created_at).toLocaleString([], {
                      dateStyle: "short",
                      timeStyle: "short",
                    })}{" "}
                    · {c.model === "user-edit" ? "Home edit" : c.model}
                  </span>
                  {c.context_snippet && (
                    <span
                      className="truncate text-[10px] italic text-zinc-500"
                      title={c.context_snippet}
                    >
                      “{c.context_snippet}”
                    </span>
                  )}
                </div>
                <button
                  type="button"
                  onClick={() => void deleteCorrection(c.id)}
                  className="shrink-0 rounded-md px-2 py-0.5 text-[11px] text-zinc-500 hover:bg-red-50 dark:hover:bg-red-950/40 hover:text-red-600 dark:hover:text-red-300"
                >
                  Delete
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      {topWords.length > 0 && (
        <section className="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-900/60 p-3 space-y-2">
          <h3 className="text-[11px] font-medium text-zinc-600 dark:text-zinc-400 uppercase tracking-wider">
            Most corrected
          </h3>
          <ul className="space-y-1">
            {topWords.map((w, i) => (
              <li
                key={`${w.model}-${w.wrong_text}-${i}`}
                className="flex items-center justify-between gap-2 border-t border-zinc-200/70 dark:border-zinc-800/60 pt-1.5 text-xs"
              >
                <span className="truncate font-mono text-amber-700 dark:text-amber-300">
                  {w.wrong_text} → {w.intended_text}
                </span>
                <span className="shrink-0 text-zinc-500">×{w.occurrences}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-900/60 p-3 space-y-2">
        <h3 className="text-[11px] font-medium text-zinc-600 dark:text-zinc-400 uppercase tracking-wider">
          Recent dictations
        </h3>
        {recent.length === 0 ? (
          <p className="text-xs text-zinc-500">Nothing yet. Dictate once to populate.</p>
        ) : (
          <ul className="space-y-2">
            {recent.map((s) => {
              const started = new Date(s.dictation.started_at);
              const row = s.transcriptions[0];
              return (
                <li
                  key={s.dictation.id}
                  className="rounded-lg border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-950/80 p-2.5 space-y-1"
                >
                  <div className="flex items-center justify-between text-[11px] text-zinc-500">
                    <span>
                      {started.toLocaleTimeString([], {
                        hour: "2-digit",
                        minute: "2-digit",
                      })}{" "}
                      · {((s.dictation.duration_ms ?? 0) / 1000).toFixed(1)}s
                    </span>
                    <span>
                      {row ? `${row.model} · ${formatLatency(row.latency_ms)}` : ""}
                    </span>
                  </div>
                  {row && (
                    <p
                      className={`text-xs leading-snug ${
                        row.error ? "italic text-red-600 dark:text-red-400" : "text-zinc-800 dark:text-zinc-200"
                      }`}
                    >
                      {row.error ?? row.text ?? "(empty)"}
                    </p>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </section>
    </div>
  );
}

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface ModelTally {
  model: string;
  wins: number;
  appearances: number;
  avg_latency_ms: number | null;
}

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

interface LabChoice {
  dictation_id: string;
  chosen_model: string | null;
  ground_truth: string | null;
  chosen_at: string;
}

interface LabSessionSummary {
  dictation: Dictation;
  transcriptions: TranscriptionRow[];
  choice: LabChoice | null;
}

const MODEL_LABELS: Record<string, string> = {
  "groq-api": "Groq API",
  "openai-api": "OpenAI API",
  "whisper-tiny-en": "Whisper tiny.en",
  "whisper-base-en": "Whisper base.en",
  "distil-small-en": "Distil small.en",
};

const MODEL_ORDER = [
  "groq-api",
  "openai-api",
  "whisper-tiny-en",
  "whisper-base-en",
  "distil-small-en",
];

function modelLabel(id: string): string {
  return MODEL_LABELS[id] ?? id;
}

function formatLatency(ms: number | null): string {
  if (ms == null) return "—";
  if (ms < 1000) return `${ms} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

export default function Lab() {
  const [tally, setTally] = useState<ModelTally[]>([]);
  const [topWords, setTopWords] = useState<MistranscribedWord[]>([]);
  const [recent, setRecent] = useState<LabSessionSummary[]>([]);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const refreshAnalysis = useCallback(async () => {
    try {
      const [t, w, r] = await Promise.all([
        invoke<ModelTally[]>("get_model_tally"),
        invoke<MistranscribedWord[]>("get_top_mistranscribed", { limit: 10 }),
        invoke<LabSessionSummary[]>("list_lab_sessions", { limit: 20 }),
      ]);
      setTally(t);
      setTopWords(w);
      setRecent(r);
      setErrorMsg(null);
    } catch (e) {
      setErrorMsg(String(e));
    }
  }, []);

  useEffect(() => {
    void refreshAnalysis();
  }, [refreshAnalysis]);

  return (
    <div className="h-full overflow-y-auto px-4 py-4 space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-medium text-white">Model Lab</h2>
        <button
          type="button"
          onClick={() => void refreshAnalysis()}
          className="text-[11px] text-neutral-400 hover:text-neutral-200"
        >
          Refresh
        </button>
      </div>

      <p className="text-[11px] text-neutral-500">
        Every dictation runs all 4 models in parallel. The primary output (API
        or Distil small.en, based on Settings) is auto-injected; all 4 rows are
        stored in SQLite for offline analysis.
      </p>

      {errorMsg && (
        <div className="rounded-lg border border-red-900 bg-red-950/40 p-3 text-xs text-red-300">
          {errorMsg}
        </div>
      )}

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-2">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Per-model stats</h3>
        {tally.length === 0 ? (
          <p className="text-xs text-neutral-500">No dictations recorded yet.</p>
        ) : (
          <div className="space-y-1">
            <div className="grid grid-cols-4 gap-2 text-[11px] text-neutral-500 uppercase tracking-wide">
              <span>Model</span>
              <span className="text-right">Wins</span>
              <span className="text-right">Runs</span>
              <span className="text-right">Avg latency</span>
            </div>
            {tally.map((t) => (
              <div
                key={t.model}
                className="grid grid-cols-4 gap-2 text-xs text-neutral-200 py-1 border-t border-neutral-800/60"
              >
                <span className="truncate">{modelLabel(t.model)}</span>
                <span className="text-right text-emerald-300">{t.wins}</span>
                <span className="text-right text-neutral-400">{t.appearances}</span>
                <span className="text-right text-neutral-400">
                  {formatLatency(t.avg_latency_ms != null ? Math.round(t.avg_latency_ms) : null)}
                </span>
              </div>
            ))}
          </div>
        )}
      </section>

      {topWords.length > 0 && (
        <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-2">
          <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Top mistranscriptions</h3>
          <ul className="space-y-1">
            {topWords.map((w, i) => (
              <li
                key={`${w.model}-${w.wrong_text}-${i}`}
                className="flex flex-col gap-0.5 text-xs border-t border-neutral-800/60 pt-1"
              >
                <div className="flex items-center justify-between gap-2">
                  <span className="font-mono text-amber-300 truncate">
                    {w.wrong_text} → {w.intended_text}
                  </span>
                  <span className="text-neutral-500 shrink-0">×{w.occurrences}</span>
                </div>
                <span className="text-[10px] text-neutral-600">{modelLabel(w.model)}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-2">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Recent dictations</h3>
        {recent.length === 0 ? (
          <p className="text-xs text-neutral-500">Nothing yet. Dictate once to populate.</p>
        ) : (
          <ul className="space-y-3">
            {recent.map((s) => {
              const started = new Date(s.dictation.started_at);
              const byModel = new Map(s.transcriptions.map((t) => [t.model, t]));
              return (
                <li
                  key={s.dictation.id}
                  className="rounded-md border border-neutral-800 bg-neutral-950 p-2 space-y-2"
                >
                  <div className="flex items-center justify-between text-[11px] text-neutral-500">
                    <span>
                      {started.toLocaleTimeString([], {
                        hour: "2-digit",
                        minute: "2-digit",
                      })}{" "}
                      · {((s.dictation.duration_ms ?? 0) / 1000).toFixed(1)}s
                    </span>
                    <span className="font-mono text-neutral-600">
                      {s.dictation.id.slice(0, 8)}
                    </span>
                  </div>
                  <div className="grid grid-cols-1 gap-1">
                    {MODEL_ORDER.filter((m) => byModel.has(m)).map((m) => {
                      const row = byModel.get(m)!;
                      return (
                        <div
                          key={m}
                          className="grid grid-cols-[140px_1fr_auto] gap-2 text-[11px] leading-snug"
                        >
                          <span className="text-neutral-500 truncate">
                            {modelLabel(m)}
                          </span>
                          <span
                            className={
                              row.error
                                ? "text-red-400 italic truncate"
                                : "text-neutral-200 truncate"
                            }
                            title={row.error ?? row.text ?? ""}
                          >
                            {row.error ?? row.text ?? "(empty)"}
                          </span>
                          <span className="text-neutral-500">
                            {formatLatency(row.latency_ms)}
                          </span>
                        </div>
                      );
                    })}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </section>
    </div>
  );
}

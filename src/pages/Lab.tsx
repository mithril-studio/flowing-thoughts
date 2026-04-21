import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface LabResult {
  model: string;
  text: string | null;
  latency_ms: number;
  error: string | null;
}

interface LabResultsReadyEvent {
  session_id: number;
  dictation_id: string;
  duration_ms: number;
  results: LabResult[];
}

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
  "openai-api": "OpenAI API",
  "whisper-tiny-en": "Whisper tiny.en",
  "whisper-base-en": "Whisper base.en",
  "distil-small-en": "Distil small.en",
};

function modelLabel(id: string): string {
  return MODEL_LABELS[id] ?? id;
}

function formatLatency(ms: number | null): string {
  if (ms == null) return "—";
  if (ms < 1000) return `${ms} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

export default function Lab() {
  const [current, setCurrent] = useState<LabResultsReadyEvent | null>(null);
  const [pickedModel, setPickedModel] = useState<string | null>(null);
  const [pickedText, setPickedText] = useState<string | null>(null);
  const [rejectMode, setRejectMode] = useState(false);
  const [groundTruth, setGroundTruth] = useState("");
  const [pickBusy, setPickBusy] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const [correctingWord, setCorrectingWord] = useState<string | null>(null);
  const [correctionInput, setCorrectionInput] = useState("");
  const [correctionStatus, setCorrectionStatus] = useState<string | null>(null);

  const [tally, setTally] = useState<ModelTally[]>([]);
  const [topWords, setTopWords] = useState<MistranscribedWord[]>([]);
  const [recent, setRecent] = useState<LabSessionSummary[]>([]);

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
    } catch (e) {
      setErrorMsg(String(e));
    }
  }, []);

  useEffect(() => {
    void refreshAnalysis();
    const unlisten = listen<LabResultsReadyEvent>("lab-results-ready", (event) => {
      setCurrent(event.payload);
      setPickedModel(null);
      setPickedText(null);
      setRejectMode(false);
      setGroundTruth("");
      setErrorMsg(null);
      setCorrectingWord(null);
      setCorrectionInput("");
      setCorrectionStatus(null);
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [refreshAnalysis]);

  const pickWinner = async (model: string) => {
    if (!current) return;
    setPickBusy(true);
    setErrorMsg(null);
    try {
      const finalText = await invoke<string>("pick_lab_winner", {
        dictationId: current.dictation_id,
        chosenModel: model,
      });
      setPickedModel(model);
      setPickedText(finalText);
      await refreshAnalysis();
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setPickBusy(false);
    }
  };

  const submitRejectAll = async () => {
    if (!current) return;
    setPickBusy(true);
    setErrorMsg(null);
    try {
      await invoke("reject_all_lab", {
        dictationId: current.dictation_id,
        groundTruth: groundTruth.trim() ? groundTruth.trim() : null,
      });
      setCurrent(null);
      setRejectMode(false);
      setGroundTruth("");
      await refreshAnalysis();
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setPickBusy(false);
    }
  };

  const submitCorrection = async () => {
    if (!current || !pickedModel || !correctingWord) return;
    const intended = correctionInput.trim();
    if (!intended) return;
    setErrorMsg(null);
    try {
      await invoke("save_correction", {
        dictationId: current.dictation_id,
        model: pickedModel,
        wrongText: correctingWord,
        intendedText: intended,
        contextSnippet: pickedText,
      });
      setCorrectionStatus(`Saved: "${correctingWord}" → "${intended}"`);
      setCorrectingWord(null);
      setCorrectionInput("");
      await refreshAnalysis();
    } catch (e) {
      setErrorMsg(String(e));
    }
  };

  const pickedWords = useMemo(() => {
    if (!pickedText) return [];
    return pickedText.split(/(\s+)/);
  }, [pickedText]);

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

      {errorMsg && (
        <div className="rounded-lg border border-red-900 bg-red-950/40 p-3 text-xs text-red-300">
          {errorMsg}
        </div>
      )}

      {!current && !pickedModel && (
        <div className="rounded-lg border border-neutral-800 bg-neutral-900 p-6 text-center">
          <p className="text-sm text-neutral-300">Waiting for dictation…</p>
          <p className="text-[11px] text-neutral-500 mt-1">
            Hold your shortcut and speak. All 4 models transcribe in parallel.
          </p>
        </div>
      )}

      {current && !pickedModel && (
        <section className="space-y-2">
          <p className="text-[11px] text-neutral-500">
            Audio: {(current.duration_ms / 1000).toFixed(1)}s · Dictation{" "}
            <span className="font-mono">{current.dictation_id.slice(0, 8)}</span>
          </p>
          <div className="grid grid-cols-1 gap-2">
            {current.results.map((r) => (
              <div
                key={r.model}
                className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-2"
              >
                <div className="flex items-center justify-between gap-2">
                  <div className="min-w-0">
                    <p className="text-sm text-neutral-200">{modelLabel(r.model)}</p>
                    <p className="text-[11px] text-neutral-500">
                      {formatLatency(r.latency_ms)}
                    </p>
                  </div>
                  <button
                    type="button"
                    onClick={() => pickWinner(r.model)}
                    disabled={pickBusy || r.text == null}
                    className="rounded-md bg-white px-3 py-1.5 text-xs font-medium text-black hover:bg-neutral-200 disabled:opacity-40"
                  >
                    Pick
                  </button>
                </div>
                {r.error ? (
                  <p className="text-xs text-red-300">Error: {r.error}</p>
                ) : (
                  <p className="text-sm text-neutral-100 leading-relaxed">
                    {r.text ?? "(empty)"}
                  </p>
                )}
              </div>
            ))}
          </div>

          {!rejectMode ? (
            <button
              type="button"
              onClick={() => setRejectMode(true)}
              disabled={pickBusy}
              className="w-full rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-sm text-neutral-300 hover:bg-neutral-900 disabled:opacity-60"
            >
              None of these are right
            </button>
          ) : (
            <div className="rounded-lg border border-neutral-800 bg-neutral-950 p-3 space-y-2">
              <p className="text-xs text-neutral-400">
                Optional: type what you actually said (for ground-truth analysis).
              </p>
              <textarea
                value={groundTruth}
                onChange={(e) => setGroundTruth(e.target.value)}
                rows={3}
                className="w-full rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-sm text-neutral-200"
                placeholder="What did you say?"
              />
              <div className="flex gap-2">
                <button
                  type="button"
                  onClick={() => setRejectMode(false)}
                  disabled={pickBusy}
                  className="flex-1 rounded-md border border-neutral-700 bg-neutral-900 px-3 py-2 text-sm text-neutral-200 hover:bg-neutral-800 disabled:opacity-60"
                >
                  Cancel
                </button>
                <button
                  type="button"
                  onClick={() => void submitRejectAll()}
                  disabled={pickBusy}
                  className="flex-1 rounded-md bg-white px-3 py-2 text-sm font-medium text-black hover:bg-neutral-200 disabled:opacity-60"
                >
                  {pickBusy ? "Saving…" : "Save & discard"}
                </button>
              </div>
            </div>
          )}
        </section>
      )}

      {pickedModel && pickedText && current && (
        <section className="space-y-2">
          <p className="text-[11px] text-neutral-500">
            Picked: {modelLabel(pickedModel)} · Click any word to flag a mistranscription.
          </p>
          <div className="rounded-lg border border-emerald-900 bg-emerald-950/20 p-3">
            <p className="text-sm text-neutral-100 leading-relaxed">
              {pickedWords.map((token, i) => {
                if (/^\s+$/.test(token)) return <span key={i}>{token}</span>;
                const stripped = token.replace(/[^\p{L}\p{N}']/gu, "");
                if (!stripped) return <span key={i}>{token}</span>;
                return (
                  <button
                    key={i}
                    type="button"
                    onClick={() => {
                      setCorrectingWord(stripped);
                      setCorrectionInput("");
                      setCorrectionStatus(null);
                    }}
                    className="rounded px-0.5 hover:bg-amber-900/40 hover:text-amber-200"
                  >
                    {token}
                  </button>
                );
              })}
            </p>
          </div>
          {correctingWord && (
            <div className="rounded-lg border border-neutral-800 bg-neutral-950 p-3 space-y-2">
              <p className="text-xs text-neutral-400">
                Flagging <span className="font-mono text-amber-300">{correctingWord}</span>. What
                did you intend?
              </p>
              <input
                type="text"
                value={correctionInput}
                onChange={(e) => setCorrectionInput(e.target.value)}
                placeholder="Intended text"
                className="w-full rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-sm text-neutral-200"
              />
              <div className="flex gap-2">
                <button
                  type="button"
                  onClick={() => {
                    setCorrectingWord(null);
                    setCorrectionInput("");
                  }}
                  className="flex-1 rounded-md border border-neutral-700 bg-neutral-900 px-3 py-2 text-xs text-neutral-200 hover:bg-neutral-800"
                >
                  Cancel
                </button>
                <button
                  type="button"
                  onClick={() => void submitCorrection()}
                  disabled={!correctionInput.trim()}
                  className="flex-1 rounded-md bg-white px-3 py-2 text-xs font-medium text-black hover:bg-neutral-200 disabled:opacity-60"
                >
                  Save correction
                </button>
              </div>
            </div>
          )}
          {correctionStatus && (
            <p className="text-[11px] text-emerald-400">{correctionStatus}</p>
          )}
          <button
            type="button"
            onClick={() => {
              setCurrent(null);
              setPickedModel(null);
              setPickedText(null);
            }}
            className="w-full rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-xs text-neutral-300 hover:bg-neutral-900"
          >
            Done
          </button>
        </section>
      )}

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-2">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Scoreboard</h3>
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

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-2">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Top mistranscriptions</h3>
        {topWords.length === 0 ? (
          <p className="text-xs text-neutral-500">No corrections logged yet.</p>
        ) : (
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
        )}
      </section>

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-2">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Recent sessions</h3>
        {recent.length === 0 ? (
          <p className="text-xs text-neutral-500">Nothing yet.</p>
        ) : (
          <ul className="space-y-2">
            {recent.slice(0, 10).map((s) => {
              const chosen = s.choice?.chosen_model;
              const chosenText = chosen
                ? s.transcriptions.find((t) => t.model === chosen)?.text ?? "(no text)"
                : null;
              const started = new Date(s.dictation.started_at);
              return (
                <li
                  key={s.dictation.id}
                  className="rounded-md border border-neutral-800 bg-neutral-950 p-2 space-y-1"
                >
                  <div className="flex items-center justify-between text-[11px] text-neutral-500">
                    <span>{started.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</span>
                    <span>
                      {chosen ? (
                        <span className="text-emerald-300">{modelLabel(chosen)}</span>
                      ) : s.choice ? (
                        <span className="text-amber-300">rejected</span>
                      ) : (
                        <span className="text-neutral-500">pending</span>
                      )}
                    </span>
                  </div>
                  <p className="text-xs text-neutral-300 line-clamp-2">
                    {chosenText ?? s.choice?.ground_truth ?? "—"}
                  </p>
                </li>
              );
            })}
          </ul>
        )}
      </section>
    </div>
  );
}

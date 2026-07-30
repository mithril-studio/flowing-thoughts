import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AppSettings } from "../types/settings";

interface CoachingResult {
  tips: string;
  sample_count: number;
  generated_at: string;
}

interface CoachProps {
  settings: AppSettings;
}

/** Split the model's plain-text bullets into individual tips. */
function parseTips(raw: string): string[] {
  return raw
    .split("\n")
    .map((line) => line.replace(/^[-*•]\s*/, "").trim())
    .filter((line) => line.length > 0);
}

/**
 * On-demand English coaching. Sends the last N dictations to an OpenRouter
 * model and shows concise tips — filler words, repetition, and phrasing.
 */
export default function Coach({ settings }: CoachProps) {
  const [result, setResult] = useState<CoachingResult | null>(null);
  const [keyConfigured, setKeyConfigured] = useState<boolean | null>(null);
  const [loading, setLoading] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const enabled = settings.coaching.enabled;
  const batchSize = settings.coaching.batch_size;

  useEffect(() => {
    invoke<{ openrouter_api_key_configured: boolean }>("get_persisted_state")
      .then((s) => setKeyConfigured(Boolean(s.openrouter_api_key_configured)))
      .catch(() => setKeyConfigured(false));
    invoke<CoachingResult | null>("get_cached_coaching_tips")
      .then((cached) => {
        if (cached) setResult(cached);
      })
      .catch(() => {
        // Non-blocking — no cached tips yet.
      });
  }, []);

  const getTips = useCallback(async () => {
    setLoading(true);
    setErrorMsg(null);
    try {
      const res = await invoke<CoachingResult>("get_coaching_tips");
      setResult(res);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const ready = enabled && keyConfigured === true;

  return (
    <div className="h-full overflow-y-auto px-4 py-4 space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
            English coach
          </h2>
          <p className="text-xs text-zinc-500 mt-0.5">
            Concise tips from your last {batchSize} dictations — filler words,
            repetition, and phrasing.
          </p>
        </div>
        <button
          type="button"
          onClick={() => void getTips()}
          disabled={!ready || loading}
          className="rounded-lg bg-zinc-900 px-3 py-1.5 text-xs font-medium text-white hover:bg-zinc-800 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-white disabled:opacity-50"
        >
          {loading ? "Analyzing…" : "Get tips"}
        </button>
      </div>

      {!ready && (
        <div className="rounded-xl border border-amber-200 dark:border-amber-900/60 bg-amber-50 dark:bg-amber-950/30 p-3 text-xs text-amber-800 dark:text-amber-300">
          {enabled
            ? "Add your OpenRouter API key in Settings → Coaching to get tips."
            : "Turn on Coaching in Settings → Coaching, then add your OpenRouter API key."}
        </div>
      )}

      {errorMsg && (
        <div className="rounded-xl border border-red-200 dark:border-red-900/60 bg-red-50 dark:bg-red-950/40 p-3 text-xs text-red-700 dark:text-red-300">
          {errorMsg}
        </div>
      )}

      {result ? (
        <section className="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-900/60 p-3 space-y-2">
          <div className="flex items-center justify-between">
            <h3 className="text-[11px] font-medium text-zinc-600 dark:text-zinc-400 uppercase tracking-wider">
              Tips
            </h3>
            <span className="text-[10px] text-zinc-500">
              {result.sample_count} dictations ·{" "}
              {new Date(result.generated_at).toLocaleString([], {
                dateStyle: "short",
                timeStyle: "short",
              })}
            </span>
          </div>
          <ul className="space-y-1.5">
            {parseTips(result.tips).map((tip, i) => (
              <li
                key={i}
                className="flex gap-2 border-t border-zinc-200/70 dark:border-zinc-800/60 pt-1.5 text-xs leading-snug text-zinc-800 dark:text-zinc-200"
              >
                <span className="text-emerald-600 dark:text-emerald-400">•</span>
                <span>{tip}</span>
              </li>
            ))}
          </ul>
        </section>
      ) : (
        ready &&
        !loading && (
          <p className="text-xs text-zinc-500">
            Click "Get tips" to analyze your recent dictations.
          </p>
        )
      )}
    </div>
  );
}

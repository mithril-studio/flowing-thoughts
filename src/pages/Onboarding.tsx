import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  type AppSettings,
  type AppSettingsUpdateResult,
  defaultAppSettings,
} from "../types/settings";

const RECOMMENDED_MODEL = "whisper-small-q5";

interface AccessibilityHelpInfo {
  executable_path: string;
  is_dev_build: boolean;
  note: string;
}

interface InstalledModel {
  id: string;
  display_name: string;
  installed: boolean;
  expected_size_bytes: number;
}

interface DownloadProgress {
  id: string;
  bytes_downloaded: number;
  total_bytes: number;
}

interface PersistedStateView {
  onboarding_complete: boolean;
  settings?: {
    extras?: {
      dangerously_skip_permissions?: boolean;
    };
  };
}

interface OnboardingProps {
  onComplete: () => void;
}

type LanguageMode = "system" | "en" | "nl";

export default function Onboarding({ onComplete }: OnboardingProps) {
  const [step, setStep] = useState(1);
  const [language, setLanguage] = useState<LanguageMode>("system");
  const [modelInstalled, setModelInstalled] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [accessibilityGranted, setAccessibilityGranted] = useState(false);
  const [dangerouslySkipPermissions, setDangerouslySkipPermissions] = useState(false);
  const [accessibilityHelp, setAccessibilityHelp] = useState<AccessibilityHelpInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const invokeWithTimeout = async <T,>(
    command: string,
    args?: Record<string, unknown>,
    timeoutMs = 8000,
  ): Promise<T> => {
    return Promise.race([
      invoke<T>(command, args),
      new Promise<T>((_, reject) =>
        setTimeout(
          () => reject(new Error(`"${command}" timed out after ${timeoutMs}ms`)),
          timeoutMs,
        ),
      ),
    ]);
  };

  useEffect(() => {
    invoke<PersistedStateView>("get_persisted_state")
      .then((state) => {
        setDangerouslySkipPermissions(
          Boolean(state.settings?.extras?.dangerously_skip_permissions),
        );
      })
      .catch(() => {
        // Keep defaults when backend command is unavailable.
      });

    invoke<InstalledModel[]>("list_installed_models")
      .then((models) => {
        const recommended = models.find((m) => m.id === RECOMMENDED_MODEL);
        setModelInstalled(Boolean(recommended?.installed));
      })
      .catch(() => {
        // Non-blocking.
      });

    invoke<AccessibilityHelpInfo>("get_accessibility_help_info")
      .then((info) => setAccessibilityHelp(info))
      .catch(() => {
        // Non-blocking helper info.
      });
  }, []);

  useEffect(() => {
    const progressUnlisten = listen<DownloadProgress>("model-download-progress", (event) => {
      if (event.payload.id === RECOMMENDED_MODEL) setProgress(event.payload);
    });
    const completeUnlisten = listen<{ id: string }>("model-download-complete", (event) => {
      if (event.payload.id === RECOMMENDED_MODEL) {
        setDownloading(false);
        setProgress(null);
        setModelInstalled(true);
      }
    });
    const errorUnlisten = listen<{ id: string; message: string }>(
      "model-download-error",
      (event) => {
        if (event.payload.id === RECOMMENDED_MODEL) {
          setDownloading(false);
          setProgress(null);
          setError(event.payload.message);
        }
      },
    );
    return () => {
      void progressUnlisten.then((fn) => fn());
      void completeUnlisten.then((fn) => fn());
      void errorUnlisten.then((fn) => fn());
    };
  }, []);

  const persistSettings = async (useLocal: boolean) => {
    // Best-effort: onboarding shouldn't dead-end on a settings write.
    try {
      const current = await invoke<AppSettings>("get_app_settings").catch(
        () => defaultAppSettings,
      );
      await invoke<AppSettingsUpdateResult>("update_app_settings", {
        settings: {
          ...current,
          language: { ...current.language, mode: language },
          transcription: useLocal
            ? { provider: "local", local_model: RECOMMENDED_MODEL }
            : current.transcription,
        },
      });
    } catch {
      // Ignore — the user can adjust everything in Settings later.
    }
  };

  const startDownload = async () => {
    setError(null);
    setDownloading(true);
    try {
      await invoke("download_model", { modelId: RECOMMENDED_MODEL });
    } catch (e) {
      setError(String(e));
      setDownloading(false);
    }
  };

  const continueToPermissions = async (useLocal: boolean) => {
    await persistSettings(useLocal);
    setStep(2);
  };

  const checkAccessibility = async () => {
    if (dangerouslySkipPermissions) {
      setStep(3);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const granted = await invokeWithTimeout<boolean>("check_accessibility_permission");
      setAccessibilityGranted(granted);
      if (granted) setStep(3);
      if (!granted) {
        setError(
          "Accessibility is still disabled. Enable it in System Settings or continue anyway.",
        );
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const finish = async (runTest: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await invoke("save_onboarding_state", {
        licenseKey: null,
        onboardingComplete: true,
      });
      if (runTest && !dangerouslySkipPermissions) {
        await invoke("run_injection_test");
      }
      onComplete();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const percent =
    progress && progress.total_bytes > 0
      ? Math.min(100, Math.floor((progress.bytes_downloaded / progress.total_bytes) * 100))
      : 0;

  return (
    <div className="flex h-full items-center justify-center px-6 py-8 text-zinc-100">
      <div className="w-full max-w-md rounded-2xl border border-zinc-800 bg-zinc-900/70 p-5">
        <h1 className="text-lg font-semibold">Welcome to FlowingThoughts</h1>
        <p className="mt-1 text-xs text-zinc-400">Step {step} of 3</p>

        {step === 1 && (
          <div className="mt-5 space-y-4">
            <div>
              <p className="mb-2 text-sm text-zinc-200">Which language do you speak?</p>
              <div className="grid grid-cols-3 gap-2" role="radiogroup" aria-label="Language">
                {(
                  [
                    { id: "system", label: "Auto" },
                    { id: "en", label: "English" },
                    { id: "nl", label: "Nederlands" },
                  ] as { id: LanguageMode; label: string }[]
                ).map((option) => (
                  <button
                    key={option.id}
                    type="button"
                    role="radio"
                    aria-checked={language === option.id}
                    onClick={() => setLanguage(option.id)}
                    className={`rounded-xl border px-3 py-2 text-sm transition-colors ${
                      language === option.id
                        ? "border-emerald-500 bg-emerald-950/30 text-emerald-300"
                        : "border-zinc-700 bg-zinc-950 text-zinc-300 hover:border-zinc-600"
                    }`}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
            </div>

            <div>
              <p className="mb-1 text-sm text-zinc-200">Download the speech model</p>
              <p className="mb-3 text-xs text-zinc-500">
                Everything runs on your Mac — private, free, works offline. One-time
                download of 190 MB.
              </p>
              {modelInstalled ? (
                <p className="text-xs text-emerald-400">Model installed and ready.</p>
              ) : downloading ? (
                <div className="space-y-1">
                  <div className="h-1.5 w-full rounded-full bg-zinc-800">
                    <div
                      className="h-full rounded-full bg-emerald-500 transition-all"
                      style={{ width: `${percent}%` }}
                    />
                  </div>
                  <p className="text-[11px] text-zinc-500">Downloading… {percent}%</p>
                </div>
              ) : (
                <button
                  type="button"
                  onClick={() => void startDownload()}
                  className="w-full rounded-xl bg-zinc-100 py-2 text-sm font-medium text-zinc-900 hover:bg-white"
                >
                  Download model (190 MB)
                </button>
              )}
            </div>

            <button
              type="button"
              onClick={() => void continueToPermissions(true)}
              disabled={!modelInstalled || downloading}
              className="w-full rounded-xl bg-emerald-600 py-2 text-sm font-medium text-white hover:bg-emerald-500 disabled:opacity-50"
            >
              Continue
            </button>
            <button
              type="button"
              onClick={() => void continueToPermissions(false)}
              className="w-full text-xs text-zinc-500 underline hover:text-zinc-300"
            >
              Skip for now — I'll use a cloud API key instead
            </button>
          </div>
        )}

        {step === 2 && (
          <div className="mt-5">
            <p className="mb-2 text-sm text-zinc-200">
              Grant Accessibility permission so FlowingThoughts can paste into apps
            </p>
            <div className="flex gap-2">
              <button
                onClick={() =>
                  invoke("open_accessibility_settings").catch((e) => setError(String(e)))
                }
                className="flex-1 rounded-xl bg-zinc-800 py-2 text-sm text-zinc-200 hover:bg-zinc-700"
              >
                Open Settings
              </button>
              <button
                onClick={checkAccessibility}
                disabled={busy}
                className="flex-1 rounded-xl bg-zinc-100 py-2 text-sm font-medium text-zinc-900 hover:bg-white disabled:opacity-60"
              >
                {busy ? "Checking..." : "Check Access"}
              </button>
            </div>
            <button
              onClick={() => setStep(3)}
              className="mt-2 w-full rounded-xl border border-zinc-700 bg-zinc-900 py-2 text-sm text-zinc-300 hover:bg-zinc-800"
            >
              Continue Anyway
            </button>
            <button
              onClick={() =>
                invoke("reveal_current_executable").catch((e) => setError(String(e)))
              }
              className="mt-2 w-full rounded-xl border border-zinc-700 bg-zinc-900 py-2 text-sm text-zinc-300 hover:bg-zinc-800"
            >
              Reveal Running App in Finder
            </button>
            {accessibilityHelp && (
              <div className="mt-2 rounded-xl border border-zinc-800 bg-zinc-950 p-2">
                <p className="text-[11px] text-zinc-400">{accessibilityHelp.note}</p>
                <p className="mt-1 break-all text-[11px] text-zinc-500">
                  {accessibilityHelp.executable_path}
                </p>
              </div>
            )}
            {dangerouslySkipPermissions && (
              <p className="mt-2 text-xs text-amber-300">
                Permission checks are skipped by settings.
              </p>
            )}
            {accessibilityGranted && (
              <p className="mt-2 text-xs text-emerald-400">
                Accessibility permission detected.
              </p>
            )}
          </div>
        )}

        {step === 3 && (
          <div className="mt-5">
            <p className="mb-2 text-sm text-zinc-200">
              Click a text field in any app, then press the button — FlowingThoughts
              will paste a test sentence there to confirm everything works.
            </p>
            <button
              onClick={() => finish(true)}
              disabled={busy}
              className="w-full rounded-xl bg-zinc-100 py-2 text-sm font-medium text-zinc-900 hover:bg-white disabled:opacity-60"
            >
              {busy ? "Running..." : "Run End-to-End Test"}
            </button>
            <button
              onClick={() => finish(false)}
              disabled={busy}
              className="mt-2 w-full rounded-xl border border-zinc-700 bg-zinc-900 py-2 text-sm text-zinc-300 hover:bg-zinc-800 disabled:opacity-60"
            >
              Skip Test and Finish
            </button>
          </div>
        )}

        {error && <p className="mt-3 text-xs text-red-400">{error}</p>}
      </div>
    </div>
  );
}

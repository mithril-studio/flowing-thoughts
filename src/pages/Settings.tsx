import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import {
  type AppSettings,
  type AppSettingsUpdateResult,
  defaultAppSettings,
} from "../types/settings";
import type { MeetingLanguage, PermissionStatus } from "../types/meetings";
import {
  checkSystemAudioPermission,
  meetingsSupported,
  openSystemAudioSettings,
} from "../lib/meetingsApi";

interface AppVersion {
  version: string;
  commit: string;
}

type UpdateCheckStatus =
  | "idle"
  | "checking"
  | "latest"
  | "available"
  | "installing"
  | "error";

interface SettingsProps {
  settings: AppSettings;
  onSettingsChange: (settings: AppSettings) => void;
}

interface AccessibilityHelpInfo {
  executable_path: string;
  is_dev_build: boolean;
  note: string;
}

interface InstalledModel {
  id: string;
  display_name: string;
  description: string;
  filename: string;
  installed: boolean;
  expected_size_bytes: number;
  local_path: string | null;
  multilingual: boolean;
  custom: boolean;
  hidden: boolean;
}

interface DownloadProgress {
  id: string;
  bytes_downloaded: number;
  total_bytes: number;
}

function formatBytes(n: number): string {
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(2)} GB`;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(0)} MB`;
  return `${(n / 1000).toFixed(0)} KB`;
}

type Provider = "groq" | "openai";

interface ProviderMeta {
  label: string;
  placeholder: string;
  prefix: string;
  docsUrl: string;
}

const PROVIDER_META: Record<Provider, ProviderMeta> = {
  groq: {
    label: "Groq",
    placeholder: "gsk_...",
    prefix: "gsk_",
    docsUrl: "https://console.groq.com/keys",
  },
  openai: {
    label: "OpenAI",
    placeholder: "sk-...",
    prefix: "sk-",
    docsUrl: "https://platform.openai.com/api-keys",
  },
};

interface PersistedStateView {
  active_provider: Provider;
  groq_api_key_configured: boolean;
  openai_api_key_configured: boolean;
  openrouter_api_key_configured: boolean;
}

const COACHING_MODEL_SUGGESTIONS = [
  "openai/gpt-4o-mini",
  "google/gemini-2.5-flash-lite",
  "meta-llama/llama-3.1-8b-instruct",
];

const MEETING_LANGUAGES: { value: MeetingLanguage; label: string }[] = [
  { value: "auto", label: "Auto" },
  { value: "en", label: "English" },
  { value: "nl", label: "Nederlands" },
];

const AUTO_DELETE_AUDIO_DAY_CHOICES = [7, 30, 90];
/** What the auto-delete toggle switches on. */
const DEFAULT_AUTO_DELETE_AUDIO_DAYS = 30;

export default function Settings({ settings, onSettingsChange }: SettingsProps) {
  const [local, setLocal] = useState<AppSettings>(settings ?? defaultAppSettings);
  const [busy, setBusy] = useState(false);
  const [apiBusy, setApiBusy] = useState(false);
  const [setupBusy, setSetupBusy] = useState(false);
  const [apiKey, setApiKey] = useState("");
  const [editingProvider, setEditingProvider] = useState<Provider>("groq");
  const [activeProvider, setActiveProvider] = useState<Provider>("groq");
  const [groqConfigured, setGroqConfigured] = useState(false);
  const [openaiConfigured, setOpenaiConfigured] = useState(false);
  const [apiMessage, setApiMessage] = useState<string | null>(null);
  const [openrouterKey, setOpenrouterKey] = useState("");
  const [openrouterConfigured, setOpenrouterConfigured] = useState(false);
  const [coachBusy, setCoachBusy] = useState(false);
  const [coachMessage, setCoachMessage] = useState<string | null>(null);
  const [accessibilityGranted, setAccessibilityGranted] = useState<boolean | null>(null);
  const [inputMonitoringGranted, setInputMonitoringGranted] = useState<boolean | null>(null);
  const [helpInfo, setHelpInfo] = useState<AccessibilityHelpInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);
  const [models, setModels] = useState<InstalledModel[]>([]);
  const [modelProgress, setModelProgress] = useState<Record<string, DownloadProgress>>({});
  const [downloadingModel, setDownloadingModel] = useState<string | null>(null);
  const [modelError, setModelError] = useState<string | null>(null);
  const [customSource, setCustomSource] = useState("");
  const [showHidden, setShowHidden] = useState(false);
  const [addingCustom, setAddingCustom] = useState(false);
  const [appVersion, setAppVersion] = useState<AppVersion | null>(null);
  const [updateStatus, setUpdateStatus] = useState<UpdateCheckStatus>("idle");
  const [pendingUpdate, setPendingUpdate] = useState<Update | null>(null);
  const [updateMessage, setUpdateMessage] = useState<string | null>(null);
  const [versionCopied, setVersionCopied] = useState(false);
  const [meetingsOsSupported, setMeetingsOsSupported] = useState<boolean | null>(null);
  const [systemAudio, setSystemAudio] = useState<PermissionStatus | null>(null);

  useEffect(() => {
    setLocal(settings ?? defaultAppSettings);
  }, [settings]);

  const refreshProviderState = () => {
    invoke<PersistedStateView>("get_persisted_state")
      .then((state) => {
        setGroqConfigured(Boolean(state.groq_api_key_configured));
        setOpenaiConfigured(Boolean(state.openai_api_key_configured));
        setOpenrouterConfigured(Boolean(state.openrouter_api_key_configured));
        if (state.active_provider === "openai" || state.active_provider === "groq") {
          setActiveProvider(state.active_provider);
          setEditingProvider(state.active_provider);
        }
      })
      .catch(() => {
        // Non-blocking.
      });
  };

  useEffect(() => {
    refreshProviderState();
  }, []);

  useEffect(() => {
    invoke<AccessibilityHelpInfo>("get_accessibility_help_info")
      .then((info) => setHelpInfo(info))
      .catch(() => {
        // Non-blocking helper content.
      });
  }, []);

  useEffect(() => {
    invoke<AppVersion>("get_app_version")
      .then(setAppVersion)
      .catch(() => setAppVersion(null));
  }, []);

  const checkForUpdates = async () => {
    setUpdateStatus("checking");
    setUpdateMessage(null);
    setPendingUpdate(null);
    try {
      const result = await check();
      if (result) {
        setPendingUpdate(result);
        setUpdateStatus("available");
      } else {
        setUpdateStatus("latest");
      }
    } catch (e) {
      const message = String(e);
      setUpdateMessage(
        message.includes("Could not fetch a valid release JSON")
          ? "No published release found yet — you're on the latest build."
          : message,
      );
      setUpdateStatus("error");
    }
  };

  const installPendingUpdate = async () => {
    if (!pendingUpdate) return;
    setUpdateStatus("installing");
    setUpdateMessage(null);
    try {
      await pendingUpdate.downloadAndInstall();
      await relaunch();
    } catch (e) {
      setUpdateMessage(String(e));
      setUpdateStatus("error");
    }
  };

  const copyVersion = () => {
    if (!appVersion) return;
    const label = `v${appVersion.version} (${appVersion.commit})`;
    void invoke("copy_to_clipboard", { text: label }).then(() => {
      setVersionCopied(true);
      setTimeout(() => setVersionCopied(false), 1500);
    });
  };

  const fetchModels = async () => {
    try {
      const list = await invoke<InstalledModel[]>("list_installed_models");
      setModels(list);
    } catch (e) {
      setModelError(String(e));
    }
  };

  useEffect(() => {
    void fetchModels();
    const progressUnlisten = listen<DownloadProgress>("model-download-progress", (event) => {
      setModelProgress((prev) => ({ ...prev, [event.payload.id]: event.payload }));
    });
    const completeUnlisten = listen<{ id: string }>("model-download-complete", (event) => {
      setDownloadingModel((current) => (current === event.payload.id ? null : current));
      setModelProgress((prev) => {
        const next = { ...prev };
        delete next[event.payload.id];
        return next;
      });
      void fetchModels();
    });
    const errorUnlisten = listen<{ id: string; message: string }>(
      "model-download-error",
      (event) => {
        setDownloadingModel((current) => (current === event.payload.id ? null : current));
        setModelError(`${event.payload.id}: ${event.payload.message}`);
        setModelProgress((prev) => {
          const next = { ...prev };
          delete next[event.payload.id];
          return next;
        });
      },
    );
    return () => {
      void progressUnlisten.then((fn) => fn());
      void completeUnlisten.then((fn) => fn());
      void errorUnlisten.then((fn) => fn());
    };
  }, []);

  const startDownload = async (id: string) => {
    setModelError(null);
    setDownloadingModel(id);
    try {
      await invoke("download_model", { modelId: id });
    } catch (e) {
      setModelError(String(e));
      setDownloadingModel(null);
    }
  };

  const addCustomModel = async () => {
    const source = customSource.trim();
    if (!source) return;
    setModelError(null);
    setAddingCustom(true);
    try {
      const id = await invoke<string>("add_custom_model", { source });
      setDownloadingModel(id);
      setCustomSource("");
    } catch (e) {
      setModelError(String(e));
    } finally {
      setAddingCustom(false);
    }
  };

  const hideModel = async (id: string) => {
    setModelError(null);
    try {
      await invoke("hide_model", { modelId: id });
      await fetchModels();
    } catch (e) {
      setModelError(String(e));
    }
  };

  const unhideModel = async (id: string) => {
    setModelError(null);
    try {
      await invoke("unhide_model", { modelId: id });
      await fetchModels();
    } catch (e) {
      setModelError(String(e));
    }
  };

  const removeModel = async (id: string) => {
    setModelError(null);
    try {
      await invoke("delete_model", { modelId: id });
      await fetchModels();
    } catch (e) {
      setModelError(String(e));
    }
  };

  const persist = async (next: AppSettings) => {
    setBusy(true);
    setError(null);
    try {
      const result = await invoke<AppSettingsUpdateResult>("update_app_settings", {
        settings: next,
      });
      setLocal(result.settings);
      onSettingsChange(result.settings);
      setWarnings(result.warnings);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const update = (next: AppSettings) => {
    setLocal(next);
    void persist(next);
  };

  const editingMeta = PROVIDER_META[editingProvider];

  const saveApiKey = async () => {
    if (!apiKey.trim().startsWith(editingMeta.prefix)) {
      setApiMessage(`Please enter a valid ${editingMeta.label} API key.`);
      return;
    }
    setApiBusy(true);
    setApiMessage(null);
    try {
      await invoke("set_api_key", {
        provider: editingProvider,
        key: apiKey.trim(),
      });
      setApiKey("");
      setApiMessage(`${editingMeta.label} API key saved.`);
      refreshProviderState();
    } catch (e) {
      setApiMessage(String(e));
    } finally {
      setApiBusy(false);
    }
  };

  const switchActiveProvider = async (next: Provider) => {
    if (next === activeProvider) return;
    setApiBusy(true);
    setApiMessage(null);
    try {
      await invoke("set_active_provider", { provider: next });
      setActiveProvider(next);
      setEditingProvider(next);
      setApiMessage(`Switched to ${PROVIDER_META[next].label}.`);
    } catch (e) {
      setApiMessage(String(e));
    } finally {
      setApiBusy(false);
    }
  };

  const saveOpenrouterKey = async () => {
    const trimmed = openrouterKey.trim();
    if (!trimmed) {
      setCoachMessage("Please enter your OpenRouter API key.");
      return;
    }
    setCoachBusy(true);
    setCoachMessage(null);
    try {
      await invoke("set_openrouter_api_key", { key: trimmed });
      setOpenrouterKey("");
      setCoachMessage("OpenRouter API key saved.");
      refreshProviderState();
    } catch (e) {
      setCoachMessage(String(e));
    } finally {
      setCoachBusy(false);
    }
  };

  // Silent status polling — no system prompts. Statuses flip to green by
  // themselves once the user flips the toggles in System Settings.
  useEffect(() => {
    let cancelled = false;
    const refresh = () => {
      invoke<boolean>("check_accessibility_permission", { prompt: false })
        .then((granted) => {
          if (!cancelled) setAccessibilityGranted(granted);
        })
        .catch(() => {});
      invoke<boolean>("check_input_monitoring_permission", { prompt: false })
        .then((granted) => {
          if (!cancelled) setInputMonitoringGranted(granted);
        })
        .catch(() => {});
    };
    refresh();
    const timer = setInterval(refresh, 3000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  // Same silent polling for system audio, only while Meetings is on. macOS
  // reports a denial as silence, so "unknown" is a normal answer here.
  const meetingsEnabled = local.meetings.enabled;
  useEffect(() => {
    if (!meetingsEnabled) return;
    let cancelled = false;
    meetingsSupported()
      .then((ok) => {
        if (!cancelled) setMeetingsOsSupported(ok);
      })
      .catch(() => {});
    const refresh = () => {
      checkSystemAudioPermission()
        .then((status) => {
          if (!cancelled) setSystemAudio(status);
        })
        .catch(() => {});
    };
    refresh();
    const timer = setInterval(refresh, 3000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [meetingsEnabled]);

  // Explicit request — shows the macOS permission dialogs when not granted.
  const requestPermissions = async () => {
    setSetupBusy(true);
    setError(null);
    try {
      const accessibility = await invoke<boolean>("check_accessibility_permission", {
        prompt: true,
      });
      setAccessibilityGranted(accessibility);
      const inputMonitoring = await invoke<boolean>(
        "check_input_monitoring_permission",
        { prompt: true },
      );
      setInputMonitoringGranted(inputMonitoring);
    } catch (e) {
      setError(String(e));
    } finally {
      setSetupBusy(false);
    }
  };

  const selectedLocalModel = local.transcription.local_model;

  // Meetings decode long-form through Whisper, so Parakeet models are left out.
  const meetingModels = models.filter(
    (m) => m.installed && !m.hidden && !m.id.startsWith("parakeet"),
  );
  const meetingModelListed = meetingModels.some((m) => m.id === local.meetings.model);
  const systemAudioState = systemAudio?.state ?? "unknown";

  return (
    <div className="h-full space-y-4 overflow-y-auto px-4 py-4">
      <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">Settings</h2>

      <Section title="Transcription">
        <div className="grid grid-cols-2 gap-2">
          <ProviderTile
            title="On-device"
            subtitle="Private, free, works offline"
            selected={local.transcription.provider === "local"}
            disabled={busy}
            onClick={() =>
              update({
                ...local,
                transcription: { ...local.transcription, provider: "local" },
              })
            }
          />
          <ProviderTile
            title="Cloud API"
            subtitle="Groq / OpenAI, needs a key"
            selected={local.transcription.provider === "api"}
            disabled={busy}
            onClick={() =>
              update({
                ...local,
                transcription: { ...local.transcription, provider: "api" },
              })
            }
          />
        </div>

        {local.transcription.provider === "local" && (
        <div className="space-y-2 pt-1">
          <p className="text-xs text-zinc-600 dark:text-zinc-400">Local models</p>
          {models.length === 0 && <p className="text-xs text-zinc-500">Loading…</p>}
          {models.filter((m) => !m.hidden).map((model) => {
            const progress = modelProgress[model.id];
            const isDownloading = downloadingModel === model.id;
            const isSelected = selectedLocalModel === model.id;
            const percent =
              progress && progress.total_bytes > 0
                ? Math.min(
                    100,
                    Math.floor((progress.bytes_downloaded / progress.total_bytes) * 100),
                  )
                : 0;
            return (
              <div
                key={model.id}
                className={`space-y-2 rounded-xl border p-3 transition-colors ${
                  isSelected && model.installed
                    ? "border-emerald-600/60 bg-emerald-50 dark:bg-emerald-950/20"
                    : "border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-950/80"
                }`}
              >
                <div className="flex items-center justify-between gap-2">
                  <div className="min-w-0">
                    <div className="flex items-center gap-1.5">
                      <p className="truncate text-sm text-zinc-800 dark:text-zinc-200">{model.display_name}</p>
                      {model.multilingual && (
                        <span className="shrink-0 rounded-full border border-sky-200 dark:border-sky-800 bg-sky-100 dark:bg-sky-950/60 px-1.5 py-px text-[9px] font-medium uppercase tracking-wide text-sky-700 dark:text-sky-300">
                          EN + NL
                        </span>
                      )}
                      {model.custom && (
                        <span className="shrink-0 rounded-full border border-amber-200 dark:border-amber-800 bg-amber-100 dark:bg-amber-950/60 px-1.5 py-px text-[9px] font-medium uppercase tracking-wide text-amber-700 dark:text-amber-300">
                          Custom
                        </span>
                      )}
                    </div>
                    <p className="text-[11px] text-zinc-500">
                      {model.description} · {formatBytes(model.expected_size_bytes)}
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-1.5">
                    {model.installed ? (
                      <>
                        {isSelected ? (
                          <span className="rounded-lg bg-emerald-100 dark:bg-emerald-900/60 px-2.5 py-1.5 text-xs text-emerald-700 dark:text-emerald-300">
                            Active
                          </span>
                        ) : (
                          <button
                            type="button"
                            disabled={busy}
                            onClick={() =>
                              update({
                                ...local,
                                transcription: {
                                  provider: "local",
                                  local_model: model.id,
                                },
                              })
                            }
                            className="rounded-lg border border-zinc-300 dark:border-zinc-700 px-2.5 py-1.5 text-xs text-zinc-800 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800"
                          >
                            Use
                          </button>
                        )}
                        <button
                          type="button"
                          onClick={() => removeModel(model.id)}
                          disabled={isSelected}
                          title={
                            isSelected
                              ? "Switch to another model before deleting the active one"
                              : `Remove ${model.filename} from disk`
                          }
                          className="rounded-lg border border-red-200 dark:border-red-900/60 px-2.5 py-1.5 text-xs text-red-600 dark:text-red-300 hover:bg-red-50 dark:hover:bg-red-950/40 disabled:cursor-not-allowed disabled:opacity-40"
                        >
                          Delete
                        </button>
                      </>
                    ) : (
                      <>
                        <button
                          type="button"
                          onClick={() => startDownload(model.id)}
                          disabled={isDownloading}
                          className="rounded-lg border border-zinc-300 dark:border-zinc-700 px-2.5 py-1.5 text-xs text-zinc-800 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:opacity-60"
                        >
                          {isDownloading ? "Downloading…" : "Download"}
                        </button>
                        <button
                          type="button"
                          onClick={() => hideModel(model.id)}
                          disabled={isDownloading}
                          title="Remove from this list. You can restore it below."
                          className="rounded-lg border border-red-200 dark:border-red-900/60 px-2.5 py-1.5 text-xs text-red-600 dark:text-red-300 hover:bg-red-50 dark:hover:bg-red-950/40 disabled:opacity-40"
                        >
                          Remove
                        </button>
                      </>
                    )}
                  </div>
                </div>
                {isDownloading && progress && (
                  <div className="space-y-1">
                    <div className="h-1.5 w-full rounded-full bg-zinc-200 dark:bg-zinc-800">
                      <div
                        className="h-full rounded-full bg-emerald-500 transition-all"
                        style={{ width: `${percent}%` }}
                      />
                    </div>
                    <p className="text-[11px] text-zinc-500">
                      {formatBytes(progress.bytes_downloaded)} /{" "}
                      {formatBytes(progress.total_bytes)} ({percent}%)
                    </p>
                  </div>
                )}
              </div>
            );
          })}
          {downloadingModel && !models.some((m) => m.id === downloadingModel) && (() => {
            const progress = modelProgress[downloadingModel];
            const percent =
              progress && progress.total_bytes > 0
                ? Math.min(100, Math.floor((progress.bytes_downloaded / progress.total_bytes) * 100))
                : 0;
            return (
              <div className="space-y-2 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-950/80 p-3">
                <p className="truncate text-sm text-zinc-800 dark:text-zinc-200">
                  Downloading {downloadingModel}…
                </p>
                <div className="h-1.5 w-full rounded-full bg-zinc-200 dark:bg-zinc-800">
                  <div
                    className="h-full rounded-full bg-emerald-500 transition-all"
                    style={{ width: `${percent}%` }}
                  />
                </div>
                {progress && (
                  <p className="text-[11px] text-zinc-500">
                    {formatBytes(progress.bytes_downloaded)} / {formatBytes(progress.total_bytes)} ({percent}%)
                  </p>
                )}
              </div>
            );
          })()}
          {models.some((m) => m.hidden) && (
            <div className="space-y-1.5">
              <button
                type="button"
                onClick={() => setShowHidden((v) => !v)}
                className="text-[11px] text-zinc-500 underline-offset-2 hover:underline"
              >
                {showHidden ? "Hide" : "Show"} {models.filter((m) => m.hidden).length} removed
                model{models.filter((m) => m.hidden).length === 1 ? "" : "s"}
              </button>
              {showHidden && (
                <ul className="space-y-1">
                  {models
                    .filter((m) => m.hidden)
                    .map((m) => (
                      <li
                        key={m.id}
                        className="flex items-center justify-between gap-2 rounded-lg border border-zinc-200 dark:border-zinc-800 px-3 py-1.5 text-xs text-zinc-600 dark:text-zinc-400"
                      >
                        <span className="truncate">{m.display_name}</span>
                        <button
                          type="button"
                          onClick={() => unhideModel(m.id)}
                          className="shrink-0 rounded-lg border border-zinc-300 dark:border-zinc-700 px-2 py-1 text-[11px] text-zinc-800 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800"
                        >
                          Restore
                        </button>
                      </li>
                    ))}
                </ul>
              )}
            </div>
          )}
          <form
            className="space-y-1.5 rounded-xl border border-dashed border-zinc-300 dark:border-zinc-700 p-3"
            onSubmit={(e) => {
              e.preventDefault();
              void addCustomModel();
            }}
          >
            <label htmlFor="custom-model-source" className="block text-xs text-zinc-600 dark:text-zinc-400">
              Add a whisper.cpp model
            </label>
            <div className="flex gap-2">
              <input
                id="custom-model-source"
                type="text"
                value={customSource}
                onChange={(e) => setCustomSource(e.target.value)}
                placeholder="medium-q5_0 or a Hugging Face .bin URL"
                disabled={addingCustom}
                className="min-w-0 flex-1 rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 px-2.5 py-1.5 text-xs text-zinc-800 dark:text-zinc-200 placeholder:text-zinc-400"
              />
              <button
                type="submit"
                disabled={addingCustom || customSource.trim() === ""}
                className="rounded-lg border border-zinc-300 dark:border-zinc-700 px-2.5 py-1.5 text-xs text-zinc-800 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:opacity-60"
              >
                {addingCustom ? "Adding…" : "Add"}
              </button>
            </div>
            <p className="text-[11px] text-zinc-500">
              Names are fetched from ggerganov/whisper.cpp on Hugging Face (medium, medium-q5_0,
              large-v3-q5_0, large-v3-turbo-q8_0, …). Any model you add can be deleted here.
            </p>
          </form>
          {modelError && <p className="text-xs text-red-700 dark:text-red-300">{modelError}</p>}
        </div>
        )}

        {local.transcription.provider === "api" && (
        <div className="space-y-3 pt-1">
          <div className="space-y-2">
            <p className="text-xs text-zinc-600 dark:text-zinc-400">Active provider</p>
            <div className="grid grid-cols-2 gap-2" role="radiogroup" aria-label="Active provider">
              {(Object.keys(PROVIDER_META) as Provider[]).map((key) => {
                const selected = activeProvider === key;
                const configured = key === "groq" ? groqConfigured : openaiConfigured;
                return (
                  <button
                    key={key}
                    type="button"
                    role="radio"
                    aria-checked={selected}
                    disabled={apiBusy}
                    onClick={() => switchActiveProvider(key)}
                    className={`rounded-xl border px-3 py-2 text-left text-sm transition-colors ${
                      selected
                        ? "border-emerald-500 bg-emerald-50 dark:bg-emerald-950/30 text-emerald-700 dark:text-emerald-300"
                        : "border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 text-zinc-700 dark:text-zinc-300 hover:border-zinc-400 dark:hover:border-zinc-600"
                    } ${apiBusy ? "cursor-not-allowed opacity-60" : ""}`}
                  >
                    <div className="font-medium">{PROVIDER_META[key].label}</div>
                    <div className="text-[11px] text-zinc-500">
                      {configured ? "configured" : "not configured"}
                    </div>
                  </button>
                );
              })}
            </div>
          </div>

          <div className="space-y-2">
            <p className="text-xs text-zinc-600 dark:text-zinc-400">Edit key for</p>
            <div className="grid grid-cols-2 gap-2">
              {(Object.keys(PROVIDER_META) as Provider[]).map((key) => {
                const selected = editingProvider === key;
                return (
                  <button
                    key={key}
                    type="button"
                    onClick={() => {
                      setEditingProvider(key);
                      setApiKey("");
                      setApiMessage(null);
                    }}
                    className={`rounded-lg border px-3 py-1.5 text-xs transition-colors ${
                      selected
                        ? "border-zinc-500 bg-zinc-200 dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100"
                        : "border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 text-zinc-600 dark:text-zinc-400 hover:border-zinc-400 dark:hover:border-zinc-600"
                    }`}
                  >
                    {PROVIDER_META[key].label}
                  </button>
                );
              })}
            </div>
          </div>

          <input
            type="password"
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
            placeholder={editingMeta.placeholder}
            className="w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 px-3 py-2 text-sm text-zinc-800 dark:text-zinc-200 placeholder-zinc-500"
          />
          <button
            type="button"
            onClick={saveApiKey}
            disabled={apiBusy}
            className="w-full rounded-lg bg-zinc-900 px-3 py-2 text-sm font-medium text-white hover:bg-zinc-800 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-white disabled:opacity-60"
          >
            {apiBusy ? "Saving..." : `Save ${editingMeta.label} API Key`}
          </button>
          <a
            href={editingMeta.docsUrl}
            target="_blank"
            rel="noopener noreferrer"
            className="block text-[11px] text-zinc-500 underline hover:text-zinc-700 dark:hover:text-zinc-300"
          >
            How to get a {editingMeta.label} API key →
          </a>
          {apiMessage && <p className="text-xs text-zinc-700 dark:text-zinc-300">{apiMessage}</p>}
        </div>
        )}
      </Section>

      <Section title="Language">
        <div className="grid grid-cols-3 gap-2">
          <Choice
            label="Auto"
            selected={local.language.mode === "system"}
            onClick={() =>
              update({ ...local, language: { ...local.language, mode: "system" } })
            }
            disabled={busy}
          />
          <Choice
            label="English"
            selected={local.language.mode === "en"}
            onClick={() =>
              update({ ...local, language: { ...local.language, mode: "en" } })
            }
            disabled={busy}
          />
          <Choice
            label="Nederlands"
            selected={local.language.mode === "nl"}
            onClick={() =>
              update({ ...local, language: { ...local.language, mode: "nl" } })
            }
            disabled={busy}
          />
        </div>
        {local.transcription.provider === "local" &&
          local.language.mode !== "en" &&
          models.some((m) => m.id === selectedLocalModel && !m.multilingual) && (
            <p className="text-xs text-amber-700 dark:text-amber-300">
              The active local model is English-only. Pick a model marked EN + NL for
              Dutch.
            </p>
          )}
        {local.transcription.provider === "local" &&
          local.language.mode !== "system" &&
          selectedLocalModel.startsWith("parakeet") && (
            <p className="text-xs text-zinc-500">
              Parakeet detects the language from your voice, so this choice only applies
              to Whisper models.
            </p>
          )}
      </Section>

      <Section title="Shortcut">
        <div className="grid grid-cols-2 gap-2">
          <Choice
            label="Fn (hold)"
            selected={local.shortcuts.preset === "fn"}
            onClick={() =>
              update({ ...local, shortcuts: { ...local.shortcuts, preset: "fn" } })
            }
            disabled={busy}
          />
          <Choice
            label="⌘⇧Space (hold)"
            selected={local.shortcuts.preset === "cmd_shift_space"}
            onClick={() =>
              update({
                ...local,
                shortcuts: { ...local.shortcuts, preset: "cmd_shift_space" },
              })
            }
            disabled={busy}
          />
        </div>
        <p className="text-[11px] text-zinc-500">
          Hold the shortcut anywhere, speak, release — the text is typed into the
          focused app.
        </p>
      </Section>

      <Section title="General">
        <div className="space-y-1">
          <p className="text-xs text-zinc-600 dark:text-zinc-400">Theme</p>
          <div className="grid grid-cols-3 gap-2">
            <Choice
              label="Light"
              selected={local.general.theme === "light"}
              onClick={() =>
                update({ ...local, general: { ...local.general, theme: "light" } })
              }
              disabled={busy}
            />
            <Choice
              label="Dark"
              selected={local.general.theme === "dark"}
              onClick={() =>
                update({ ...local, general: { ...local.general, theme: "dark" } })
              }
              disabled={busy}
            />
            <Choice
              label="System"
              selected={local.general.theme === "system"}
              onClick={() =>
                update({ ...local, general: { ...local.general, theme: "system" } })
              }
              disabled={busy}
            />
          </div>
        </div>
        <Toggle
          label="Window movable"
          checked={local.general.window_movable}
          onChange={(checked) =>
            update({
              ...local,
              general: { ...local.general, window_movable: checked },
            })
          }
          disabled={busy}
        />
        <Toggle
          label="Launch app at login"
          checked={local.general.launch_at_login}
          onChange={(checked) =>
            update({
              ...local,
              general: { ...local.general, launch_at_login: checked },
            })
          }
          disabled={busy}
        />
        <Toggle
          label="Show app in dock"
          checked={local.general.show_in_dock}
          onChange={(checked) =>
            update({
              ...local,
              general: { ...local.general, show_in_dock: checked },
            })
          }
          disabled={busy}
        />
        <div className="space-y-1">
          <p className="text-xs text-zinc-600 dark:text-zinc-400">Window position</p>
          <select
            value={local.general.window_position}
            disabled={busy}
            onChange={(e) =>
              update({
                ...local,
                general: { ...local.general, window_position: e.target.value },
              })
            }
            className="w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 px-3 py-2 text-sm text-zinc-800 dark:text-zinc-200"
          >
            <option value="center">Center</option>
            <option value="top_left">Top left</option>
            <option value="top_right">Top right</option>
            <option value="bottom_left">Bottom left</option>
            <option value="bottom_right">Bottom right</option>
          </select>
        </div>
      </Section>

      <Section title="Extras">
        <Toggle
          label="Smart formatting"
          checked={local.extras.smart_formatting}
          onChange={(checked) =>
            update({
              ...local,
              extras: { ...local.extras, smart_formatting: checked },
            })
          }
          disabled={busy}
        />
        <div className="space-y-1">
          <Toggle
            label="Developer dictionary"
            checked={local.extras.developer_dictionary}
            onChange={(checked) =>
              update({
                ...local,
                extras: { ...local.extras, developer_dictionary: checked },
              })
            }
            disabled={busy}
          />
          <p className="pl-1 text-xs text-zinc-500">
            Recognize developer and AI terms — API, JSON, GitHub, TypeScript,
            Kubernetes — and fix their spelling and casing in dictations.
          </p>
        </div>
        <div className="space-y-1">
          <Toggle
            label="Learn from my edits"
            checked={local.extras.auto_learn_corrections}
            onChange={(checked) =>
              update({
                ...local,
                extras: { ...local.extras, auto_learn_corrections: checked },
              })
            }
            disabled={busy}
          />
          <p className="pl-1 text-xs text-zinc-500">
            When you fix a dictation in the target app or on the Home page,
            FlowingThoughts remembers the correction and applies it next time.
          </p>
        </div>
        <div className="space-y-1">
          <Toggle
            label="Keep my dictations for evaluation"
            checked={local.extras.keep_audio_for_eval}
            onChange={(checked) =>
              update({
                ...local,
                extras: { ...local.extras, keep_audio_for_eval: checked },
              })
            }
            disabled={busy}
          />
          <p className="pl-1 text-xs text-zinc-500">
            Off by default. When on, the audio and raw transcript of every
            dictation are saved on this Mac (never uploaded) so transcription
            quality can be measured. Turn it off again when you have enough
            samples.
          </p>
        </div>
        <Toggle
          label="Dangerously skip permissions"
          checked={local.extras.dangerously_skip_permissions}
          onChange={(checked) =>
            update({
              ...local,
              extras: { ...local.extras, dangerously_skip_permissions: checked },
            })
          }
          disabled={busy}
        />
        {local.extras.dangerously_skip_permissions && (
          <p className="text-xs text-amber-700 dark:text-amber-300">
            Warning: This bypasses permission checks in onboarding and may cause
            injection failures.
          </p>
        )}
      </Section>

      <Section title="Coaching">
        <div className="space-y-1">
          <Toggle
            label="English coach"
            checked={local.coaching.enabled}
            onChange={(checked) =>
              update({
                ...local,
                coaching: { ...local.coaching, enabled: checked },
              })
            }
            disabled={busy}
          />
          <p className="pl-1 text-xs text-zinc-500">
            Adds a Coach tab that sends your recent dictations to an OpenRouter
            model and returns concise tips on filler words, repetition, and
            phrasing. On-demand only — nothing runs automatically.
          </p>
        </div>

        {local.coaching.enabled && (
          <div className="space-y-3 pt-1">
            <div className="space-y-2">
              <div className="flex items-center justify-between">
                <p className="text-xs text-zinc-600 dark:text-zinc-400">
                  OpenRouter API key
                </p>
                <span className="text-[11px] text-zinc-500">
                  {openrouterConfigured ? "configured" : "not configured"}
                </span>
              </div>
              <input
                type="password"
                value={openrouterKey}
                onChange={(e) => setOpenrouterKey(e.target.value)}
                placeholder="sk-or-..."
                className="w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 px-3 py-2 text-sm text-zinc-800 dark:text-zinc-200 placeholder-zinc-500"
              />
              <button
                type="button"
                onClick={saveOpenrouterKey}
                disabled={coachBusy}
                className="w-full rounded-lg bg-zinc-900 px-3 py-2 text-sm font-medium text-white hover:bg-zinc-800 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-white disabled:opacity-60"
              >
                {coachBusy ? "Saving..." : "Save OpenRouter API Key"}
              </button>
              <a
                href="https://openrouter.ai/keys"
                target="_blank"
                rel="noopener noreferrer"
                className="block text-[11px] text-zinc-500 underline hover:text-zinc-700 dark:hover:text-zinc-300"
              >
                How to get an OpenRouter API key →
              </a>
              {coachMessage && (
                <p className="text-xs text-zinc-700 dark:text-zinc-300">{coachMessage}</p>
              )}
            </div>

            <div className="space-y-2">
              <p className="text-xs text-zinc-600 dark:text-zinc-400">Model</p>
              <input
                type="text"
                value={local.coaching.model}
                onChange={(e) =>
                  setLocal({
                    ...local,
                    coaching: { ...local.coaching, model: e.target.value },
                  })
                }
                onBlur={() => update(local)}
                placeholder="openai/gpt-4o-mini"
                className="w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 px-3 py-2 text-sm text-zinc-800 dark:text-zinc-200 placeholder-zinc-500"
              />
              <div className="flex flex-wrap gap-1.5">
                {COACHING_MODEL_SUGGESTIONS.map((m) => (
                  <button
                    key={m}
                    type="button"
                    onClick={() =>
                      update({
                        ...local,
                        coaching: { ...local.coaching, model: m },
                      })
                    }
                    className={`rounded-md border px-2 py-0.5 text-[11px] transition-colors ${
                      local.coaching.model === m
                        ? "border-emerald-500 bg-emerald-50 dark:bg-emerald-950/30 text-emerald-700 dark:text-emerald-300"
                        : "border-zinc-300 dark:border-zinc-700 text-zinc-600 dark:text-zinc-400 hover:border-zinc-400 dark:hover:border-zinc-600"
                    }`}
                  >
                    {m}
                  </button>
                ))}
              </div>
              <p className="text-[11px] text-zinc-500">
                Any OpenRouter model id. Cheap ones work well for short tips.
              </p>
            </div>

            <div className="space-y-2">
              <p className="text-xs text-zinc-600 dark:text-zinc-400">
                Dictations analyzed: {local.coaching.batch_size}
              </p>
              <input
                type="range"
                min={5}
                max={100}
                step={5}
                value={local.coaching.batch_size}
                onChange={(e) =>
                  setLocal({
                    ...local,
                    coaching: {
                      ...local.coaching,
                      batch_size: Number(e.target.value),
                    },
                  })
                }
                onMouseUp={() => update(local)}
                onKeyUp={() => update(local)}
                className="w-full accent-emerald-500"
              />
              <p className="text-[11px] text-zinc-500">
                Fewer is cheaper and more current; more captures broader patterns.
              </p>
            </div>
          </div>
        )}
      </Section>

      <Section title="Meetings">
        <div className="space-y-1">
          <Toggle
            label="Meeting recording"
            checked={local.meetings.enabled}
            onChange={(checked) =>
              update({
                ...local,
                meetings: { ...local.meetings, enabled: checked },
              })
            }
            disabled={busy}
          />
          <p className="pl-1 text-xs text-zinc-500">
            Adds a Meetings tab that records your microphone and the call as two
            tracks, then transcribes them on this Mac after you stop. Audio stays
            on disk until you delete it.
          </p>
        </div>

        {local.meetings.enabled && (
          <div className="space-y-3 pt-1">
            {meetingsOsSupported === false ? (
              <p className="rounded-xl border border-amber-200 dark:border-amber-900/60 bg-amber-50 dark:bg-amber-950/30 p-3 text-xs text-amber-800 dark:text-amber-300">
                Meetings need macOS 14.4 or later to record system audio.
              </p>
            ) : (
              <PermissionRow
                title="System Audio Recording"
                description={
                  systemAudioState === "unknown"
                    ? "Lets meetings record the other side of the call. macOS only reports this once a meeting has recorded; without it, meetings record your microphone only."
                    : "Lets meetings record the other side of the call. Without it, meetings record your microphone only."
                }
                granted={
                  systemAudioState === "granted"
                    ? true
                    : systemAudioState === "denied"
                      ? false
                      : null
                }
                pendingLabel={systemAudio === null ? undefined : "Not checked yet"}
                onOpenSettings={() =>
                  openSystemAudioSettings().catch((e) => setError(String(e)))
                }
              />
            )}

            <div className="space-y-2">
              <p className="text-xs text-zinc-600 dark:text-zinc-400">Meeting language</p>
              <div className="grid grid-cols-3 gap-2" role="group" aria-label="Meeting language">
                {MEETING_LANGUAGES.map(({ value, label }) => (
                  <Choice
                    key={value}
                    label={label}
                    selected={local.meetings.language === value}
                    onClick={() =>
                      update({
                        ...local,
                        meetings: { ...local.meetings, language: value },
                      })
                    }
                    disabled={busy}
                  />
                ))}
              </div>
              <p className="text-[11px] text-zinc-500">
                Auto picks English or Dutch once per track. You can re-transcribe a
                meeting in another language later.
              </p>
            </div>

            <div className="space-y-2">
              <label
                htmlFor="meetings-model"
                className="block text-xs text-zinc-600 dark:text-zinc-400"
              >
                Meeting model
              </label>
              <select
                id="meetings-model"
                value={local.meetings.model}
                onChange={(e) =>
                  update({
                    ...local,
                    meetings: { ...local.meetings, model: e.target.value },
                  })
                }
                disabled={busy}
                className="w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 px-3 py-2 text-sm text-zinc-800 dark:text-zinc-200 disabled:opacity-60"
              >
                {!meetingModelListed && (
                  <option value={local.meetings.model}>
                    {local.meetings.model || "Same as dictation"} (not downloaded)
                  </option>
                )}
                {meetingModels.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.display_name}
                  </option>
                ))}
              </select>
              <p className="text-[11px] text-zinc-500">
                Downloaded Whisper models from Transcription. Meetings are decoded
                after you stop, so a larger model costs time, not live latency.
              </p>
            </div>

            <div className="space-y-2">
              <Toggle
                label="Meeting summaries"
                checked={local.meetings.summary_enabled}
                onChange={(checked) =>
                  update({
                    ...local,
                    meetings: { ...local.meetings, summary_enabled: checked },
                  })
                }
                disabled={busy}
              />
              <p className="pl-1 text-xs text-zinc-500">
                Optional. A summary sends that meeting's transcript to OpenRouter
                with your own API key. It never runs by itself: each summary asks
                first.
              </p>
              {local.meetings.summary_enabled && (
                <div className="space-y-2">
                  <label
                    htmlFor="meetings-summary-model"
                    className="block text-xs text-zinc-600 dark:text-zinc-400"
                  >
                    Summary model
                  </label>
                  <input
                    id="meetings-summary-model"
                    type="text"
                    value={local.meetings.summary_model}
                    onChange={(e) =>
                      setLocal({
                        ...local,
                        meetings: { ...local.meetings, summary_model: e.target.value },
                      })
                    }
                    onBlur={() => update(local)}
                    placeholder="openai/gpt-4o-mini"
                    className="w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 px-3 py-2 text-sm text-zinc-800 dark:text-zinc-200 placeholder-zinc-500"
                  />
                  <div className="flex items-center justify-between">
                    <label
                      htmlFor="meetings-openrouter-key"
                      className="text-xs text-zinc-600 dark:text-zinc-400"
                    >
                      OpenRouter API key
                    </label>
                    <span className="text-[11px] text-zinc-500">
                      {openrouterConfigured ? "configured" : "not configured"}
                    </span>
                  </div>
                  <input
                    id="meetings-openrouter-key"
                    type="password"
                    value={openrouterKey}
                    onChange={(e) => setOpenrouterKey(e.target.value)}
                    placeholder="sk-or-..."
                    className="w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 px-3 py-2 text-sm text-zinc-800 dark:text-zinc-200 placeholder-zinc-500"
                  />
                  <SecondaryButton onClick={saveOpenrouterKey} disabled={coachBusy}>
                    {coachBusy ? "Saving..." : "Save OpenRouter API Key"}
                  </SecondaryButton>
                  <p className="text-[11px] text-zinc-500">
                    The same key as Coaching; it is stored once.
                  </p>
                  {coachMessage && (
                    <p className="text-xs text-zinc-700 dark:text-zinc-300">{coachMessage}</p>
                  )}
                </div>
              )}
            </div>

            <div className="space-y-2">
              <Toggle
                label="Auto-delete meeting audio"
                checked={local.meetings.auto_delete_audio_days > 0}
                onChange={(checked) =>
                  update({
                    ...local,
                    meetings: {
                      ...local.meetings,
                      auto_delete_audio_days: checked ? DEFAULT_AUTO_DELETE_AUDIO_DAYS : 0,
                    },
                  })
                }
                disabled={busy}
              />
              {local.meetings.auto_delete_audio_days > 0 ? (
                <div className="space-y-2">
                  <div
                    className="grid grid-cols-3 gap-2"
                    role="group"
                    aria-label="Delete audio after"
                  >
                    {AUTO_DELETE_AUDIO_DAY_CHOICES.map((days) => (
                      <Choice
                        key={days}
                        label={`${days} days`}
                        selected={local.meetings.auto_delete_audio_days === days}
                        onClick={() =>
                          update({
                            ...local,
                            meetings: { ...local.meetings, auto_delete_audio_days: days },
                          })
                        }
                        disabled={busy}
                      />
                    ))}
                  </div>
                  <p className="pl-1 text-xs text-zinc-500">
                    Audio is deleted {local.meetings.auto_delete_audio_days} days after a
                    meeting is transcribed. Transcripts are always kept.
                  </p>
                </div>
              ) : (
                <p className="pl-1 text-xs text-zinc-500">
                  Off: audio is kept until you delete it (about 230 MB per hour).
                  Transcripts are always kept.
                </p>
              )}
            </div>
          </div>
        )}
      </Section>

      <Section title="Permissions">
        <PermissionRow
          title="Accessibility"
          description="Lets FlowingThoughts type dictations into other apps."
          granted={accessibilityGranted}
          onOpenSettings={() =>
            invoke("open_accessibility_settings").catch((e) => setError(String(e)))
          }
        />
        <PermissionRow
          title="Input Monitoring"
          description="Lets the hotkey work while you're in other apps."
          granted={inputMonitoringGranted}
          onOpenSettings={() =>
            invoke("open_input_monitoring_settings").catch((e) => setError(String(e)))
          }
        />
        <p className="text-[11px] text-zinc-500">
          Statuses refresh automatically. Toggle on but still "Not granted"?
          Remove FlowingThoughts from the list with the − button, add it again,
          and restart the app — after an app update macOS can treat it as a new
          app.
        </p>
        <div className="grid grid-cols-1 gap-2">
          <SecondaryButton onClick={requestPermissions} disabled={setupBusy}>
            {setupBusy ? "Requesting…" : "Request Permission Prompts"}
          </SecondaryButton>
          <SecondaryButton
            onClick={() =>
              invoke("reveal_current_executable").catch((e) => setError(String(e)))
            }
          >
            Reveal Running App in Finder
          </SecondaryButton>
        </div>
        {helpInfo && (
          <p className="break-all text-[11px] text-zinc-500">
            {helpInfo.note} {helpInfo.executable_path}
          </p>
        )}
      </Section>

      <Section title="About">
        <div className="space-y-2 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-950/80 p-3">
          <div className="flex items-center justify-between gap-2">
            <div className="min-w-0">
              <p className="text-sm text-zinc-800 dark:text-zinc-200">FlowingThoughts</p>
              <p className="font-mono text-[11px] text-zinc-500">
                {appVersion ? `v${appVersion.version} (${appVersion.commit})` : "Loading…"}
              </p>
            </div>
            <button
              type="button"
              onClick={copyVersion}
              disabled={!appVersion}
              className={`rounded-lg border px-2 py-1 text-xs transition-colors ${
                versionCopied
                  ? "border-emerald-300 dark:border-emerald-600 bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-300"
                  : "border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800"
              } disabled:opacity-50`}
            >
              {versionCopied ? "Copied" : "Copy"}
            </button>
          </div>
          <div className="flex items-center justify-between gap-2 pt-1">
            <p className="text-xs text-zinc-600 dark:text-zinc-400">
              {updateStatus === "idle" && "Check for a newer release."}
              {updateStatus === "checking" && "Checking for updates…"}
              {updateStatus === "latest" && "You're on the latest version."}
              {updateStatus === "available" &&
                pendingUpdate &&
                `Update available — v${pendingUpdate.version}.`}
              {updateStatus === "installing" && "Installing update…"}
              {updateStatus === "error" && (updateMessage ?? "Update check failed.")}
            </p>
            {updateStatus === "available" ? (
              <button
                type="button"
                onClick={installPendingUpdate}
                className="rounded-lg bg-emerald-700 px-3 py-1 text-xs text-white hover:bg-emerald-600"
              >
                Install & restart
              </button>
            ) : (
              <button
                type="button"
                onClick={checkForUpdates}
                disabled={updateStatus === "checking" || updateStatus === "installing"}
                className="rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-900 px-3 py-1 text-xs text-zinc-800 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:opacity-50"
              >
                {updateStatus === "checking" ? "Checking…" : "Check for updates"}
              </button>
            )}
          </div>
          <button
            type="button"
            onClick={() => invoke("open_logs_folder").catch((e) => setError(String(e)))}
            className="text-[11px] text-zinc-500 underline hover:text-zinc-700 dark:hover:text-zinc-300"
          >
            Open logs folder
          </button>
        </div>
      </Section>

      {warnings.length > 0 && (
        <div className="rounded-xl border border-amber-200 dark:border-amber-900 bg-amber-50 dark:bg-amber-950/40 p-3 text-xs text-amber-700 dark:text-amber-300">
          {warnings.map((warning, index) => (
            <p key={index}>{warning}</p>
          ))}
        </div>
      )}

      {error && (
        <div className="rounded-xl border border-red-200 dark:border-red-900 bg-red-50 dark:bg-red-950/40 p-3 text-xs text-red-700 dark:text-red-300">
          {error}
        </div>
      )}
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="space-y-3 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-900/60 p-3">
      <h3 className="text-[11px] font-medium uppercase tracking-wider text-zinc-600 dark:text-zinc-400">
        {title}
      </h3>
      {children}
    </section>
  );
}

function PermissionRow({
  title,
  description,
  granted,
  pendingLabel = "Checking…",
  onOpenSettings,
}: {
  title: string;
  description: string;
  granted: boolean | null;
  /** Shown while `granted` is null. System audio can stay unknown for good. */
  pendingLabel?: string;
  onOpenSettings: () => void;
}) {
  return (
    <div
      className={`flex items-center justify-between gap-3 rounded-xl border p-3 transition-colors ${
        granted
          ? "border-emerald-300 bg-emerald-50 dark:border-emerald-800 dark:bg-emerald-950/30"
          : "border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-950/80"
      }`}
    >
      <div className="min-w-0">
        <div className="flex items-center gap-2">
          <p className="text-sm text-zinc-800 dark:text-zinc-200">{title}</p>
          <span
            className={`shrink-0 rounded-full px-2 py-px text-[10px] font-medium ${
              granted === null
                ? "bg-zinc-200 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-400"
                : granted
                  ? "bg-emerald-100 text-emerald-700 dark:bg-emerald-900/60 dark:text-emerald-300"
                  : "bg-red-100 text-red-700 dark:bg-red-950/60 dark:text-red-300"
            }`}
          >
            {granted === null ? pendingLabel : granted ? "✓ Granted" : "Not granted"}
          </span>
        </div>
        <p className="text-[11px] text-zinc-500">{description}</p>
      </div>
      {!granted && (
        <button
          type="button"
          onClick={onOpenSettings}
          className="shrink-0 rounded-lg border border-zinc-300 bg-white px-2.5 py-1.5 text-xs text-zinc-800 hover:bg-zinc-100 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-200 dark:hover:bg-zinc-800"
        >
          Open Settings
        </button>
      )}
    </div>
  );
}

function SecondaryButton({
  onClick,
  disabled,
  children,
}: {
  onClick: () => void;
  disabled?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-900 px-3 py-2 text-sm text-zinc-800 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:opacity-60"
    >
      {children}
    </button>
  );
}

function Toggle({
  label,
  checked,
  disabled,
  onChange,
}: {
  label: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="flex items-center justify-between gap-3 text-sm text-zinc-800 dark:text-zinc-200">
      <span className="min-w-0 flex-1">{label}</span>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        aria-label={label}
        disabled={disabled}
        onClick={() => onChange(!checked)}
        className={`flex h-6 w-11 shrink-0 items-center rounded-full px-0.5 transition-colors ${
          checked ? "bg-emerald-500" : "bg-zinc-300 dark:bg-zinc-700"
        } ${disabled ? "cursor-not-allowed opacity-60" : ""}`}
      >
        <span
          className={`h-5 w-5 rounded-full bg-white shadow transition-all ${
            checked ? "ml-auto" : ""
          }`}
        />
      </button>
    </label>
  );
}

function Choice({
  label,
  selected,
  disabled,
  onClick,
}: {
  label: string;
  selected: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={`w-full rounded-xl border px-3 py-2 text-left text-sm transition-colors ${
        selected
          ? "border-emerald-500 bg-emerald-50 dark:bg-emerald-950/30 text-emerald-700 dark:text-emerald-300"
          : "border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 text-zinc-700 dark:text-zinc-300 hover:border-zinc-400 dark:hover:border-zinc-600"
      } ${disabled ? "cursor-not-allowed opacity-60" : ""}`}
    >
      {label}
    </button>
  );
}

function ProviderTile({
  title,
  subtitle,
  selected,
  disabled,
  onClick,
}: {
  title: string;
  subtitle: string;
  selected: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={`rounded-xl border px-3 py-3 text-left transition-colors ${
        selected
          ? "border-emerald-500 bg-emerald-50 dark:bg-emerald-950/30"
          : "border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-950 hover:border-zinc-400 dark:hover:border-zinc-600"
      } ${disabled ? "cursor-not-allowed opacity-60" : ""}`}
    >
      <p className={`text-sm font-medium ${selected ? "text-emerald-700 dark:text-emerald-300" : "text-zinc-800 dark:text-zinc-200"}`}>
        {title}
      </p>
      <p className="text-[11px] text-zinc-500">{subtitle}</p>
    </button>
  );
}

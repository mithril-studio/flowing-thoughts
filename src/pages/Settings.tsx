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
}

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
  const [accessibilityGranted, setAccessibilityGranted] = useState<boolean | null>(null);
  const [inputMonitoringGranted, setInputMonitoringGranted] = useState<boolean | null>(null);
  const [helpInfo, setHelpInfo] = useState<AccessibilityHelpInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);
  const [models, setModels] = useState<InstalledModel[]>([]);
  const [modelProgress, setModelProgress] = useState<Record<string, DownloadProgress>>({});
  const [downloadingModel, setDownloadingModel] = useState<string | null>(null);
  const [modelError, setModelError] = useState<string | null>(null);
  const [appVersion, setAppVersion] = useState<AppVersion | null>(null);
  const [updateStatus, setUpdateStatus] = useState<UpdateCheckStatus>("idle");
  const [pendingUpdate, setPendingUpdate] = useState<Update | null>(null);
  const [updateMessage, setUpdateMessage] = useState<string | null>(null);
  const [versionCopied, setVersionCopied] = useState(false);

  useEffect(() => {
    setLocal(settings ?? defaultAppSettings);
  }, [settings]);

  const refreshProviderState = () => {
    invoke<PersistedStateView>("get_persisted_state")
      .then((state) => {
        setGroqConfigured(Boolean(state.groq_api_key_configured));
        setOpenaiConfigured(Boolean(state.openai_api_key_configured));
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

  const checkAccessibility = async () => {
    setSetupBusy(true);
    setError(null);
    try {
      const granted = await invoke<boolean>("check_accessibility_permission");
      setAccessibilityGranted(granted);
    } catch (e) {
      setError(String(e));
      setAccessibilityGranted(false);
    } finally {
      setSetupBusy(false);
    }
  };

  const checkInputMonitoring = async () => {
    setSetupBusy(true);
    setError(null);
    try {
      const granted = await invoke<boolean>("check_input_monitoring_permission", {
        prompt: true,
      });
      setInputMonitoringGranted(granted);
    } catch (e) {
      setError(String(e));
      setInputMonitoringGranted(false);
    } finally {
      setSetupBusy(false);
    }
  };

  // Passive status read on mount (no system prompt).
  useEffect(() => {
    invoke<boolean>("check_input_monitoring_permission", { prompt: false })
      .then(setInputMonitoringGranted)
      .catch(() => {
        // Non-blocking.
      });
  }, []);

  const selectedLocalModel = local.transcription.local_model;

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
          {models.map((model) => {
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
                          className="rounded-lg px-2 py-1.5 text-xs text-zinc-500 hover:bg-red-50 dark:hover:bg-red-950/40 hover:text-red-600 dark:hover:text-red-300"
                        >
                          Delete
                        </button>
                      </>
                    ) : (
                      <button
                        type="button"
                        onClick={() => startDownload(model.id)}
                        disabled={isDownloading}
                        className="rounded-lg border border-zinc-300 dark:border-zinc-700 px-2.5 py-1.5 text-xs text-zinc-800 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:opacity-60"
                      >
                        {isDownloading ? "Downloading…" : "Download"}
                      </button>
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

      <Section title="Permissions">
        <p className="text-xs text-zinc-600 dark:text-zinc-400">
          Accessibility (typing into apps):{" "}
          {accessibilityGranted === null
            ? "not checked"
            : accessibilityGranted
              ? "granted"
              : "not granted"}
        </p>
        <p className="text-xs text-zinc-600 dark:text-zinc-400">
          Input Monitoring (hotkey outside the app):{" "}
          {inputMonitoringGranted === null
            ? "not checked"
            : inputMonitoringGranted
              ? "granted"
              : "not granted"}
        </p>
        {inputMonitoringGranted === false && (
          <p className="text-xs text-amber-700 dark:text-amber-300">
            Without Input Monitoring the dictation hotkey only works while
            FlowingThoughts itself is focused. Enable it, then restart the app.
          </p>
        )}
        <div className="grid grid-cols-1 gap-2">
          <SecondaryButton onClick={checkAccessibility} disabled={setupBusy}>
            {setupBusy ? "Checking..." : "Check Accessibility"}
          </SecondaryButton>
          <SecondaryButton onClick={checkInputMonitoring} disabled={setupBusy}>
            {setupBusy ? "Checking..." : "Check Input Monitoring"}
          </SecondaryButton>
          <SecondaryButton
            onClick={() =>
              invoke("open_accessibility_settings").catch((e) => setError(String(e)))
            }
          >
            Open Accessibility Settings
          </SecondaryButton>
          <SecondaryButton
            onClick={() =>
              invoke("open_input_monitoring_settings").catch((e) => setError(String(e)))
            }
          >
            Open Input Monitoring Settings
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

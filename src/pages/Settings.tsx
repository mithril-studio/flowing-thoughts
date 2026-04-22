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
  filename: string;
  installed: boolean;
  expected_size_bytes: number;
  local_path: string | null;
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
      setUpdateMessage(String(e));
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
    const errorUnlisten = listen<{ id: string; error: string }>("model-download-error", (event) => {
      setDownloadingModel((current) => (current === event.payload.id ? null : current));
      setModelError(`${event.payload.id}: ${event.payload.error}`);
      setModelProgress((prev) => {
        const next = { ...prev };
        delete next[event.payload.id];
        return next;
      });
    });
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

  const anyModelInstalled = models.some((m) => m.installed);

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

  return (
    <div className="h-full overflow-y-auto px-4 py-4 space-y-4">
      <h2 className="text-sm font-medium text-white">Settings</h2>

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-3">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Setup Steps</h3>
        <div className="rounded-md border border-neutral-800 bg-neutral-950 p-3 space-y-2">
          <p className="text-sm text-neutral-200">Accessibility</p>
          <p className="text-xs text-neutral-400">
            Status:{" "}
            {accessibilityGranted === null
              ? "Not checked"
              : accessibilityGranted
                ? "Granted"
                : "Not granted"}
          </p>
          <div className="grid grid-cols-1 gap-2">
            <button
              type="button"
              onClick={checkAccessibility}
              disabled={setupBusy}
              className="rounded-md border border-neutral-700 bg-neutral-900 px-3 py-2 text-sm text-neutral-200 hover:bg-neutral-800 disabled:opacity-60"
            >
              {setupBusy ? "Checking..." : "Check Accessibility"}
            </button>
            <button
              type="button"
              onClick={() => invoke("open_accessibility_settings").catch((e) => setError(String(e)))}
              className="rounded-md border border-neutral-700 bg-neutral-900 px-3 py-2 text-sm text-neutral-200 hover:bg-neutral-800"
            >
              Open Accessibility Settings
            </button>
            <button
              type="button"
              onClick={() => invoke("reveal_current_executable").catch((e) => setError(String(e)))}
              className="rounded-md border border-neutral-700 bg-neutral-900 px-3 py-2 text-sm text-neutral-200 hover:bg-neutral-800"
            >
              Reveal Running App in Finder
            </button>
          </div>
          {helpInfo && (
            <p className="text-[11px] text-neutral-500 break-all">
              {helpInfo.note} {helpInfo.executable_path}
            </p>
          )}
        </div>

        <div className="rounded-md border border-neutral-800 bg-neutral-950 p-3 space-y-2">
          <p className="text-sm text-neutral-200">Shortcut Step</p>
          <p className="text-xs text-neutral-400">
            Active shortcut: {local.shortcuts.preset === "fn" ? "Fn (hold)" : "Cmd+Shift+Space (hold)"}
          </p>
          <button
            type="button"
            onClick={() => invoke("open_input_monitoring_settings").catch((e) => setError(String(e)))}
            className="w-full rounded-md border border-neutral-700 bg-neutral-900 px-3 py-2 text-sm text-neutral-200 hover:bg-neutral-800"
          >
            Open Input Monitoring Settings
          </button>
          <p className="text-[11px] text-neutral-500">
            Ensure your terminal or app is allowed in both Accessibility and Input Monitoring.
          </p>
        </div>
      </section>

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-3">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">General</h3>
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
          <p className="text-xs text-neutral-400">Window position</p>
          <select
            value={local.general.window_position}
            disabled={busy}
            onChange={(e) =>
              update({
                ...local,
                general: { ...local.general, window_position: e.target.value },
              })
            }
            className="w-full rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-sm text-neutral-200"
          >
            <option value="center">Center</option>
            <option value="top_left">Top left</option>
            <option value="top_right">Top right</option>
            <option value="bottom_left">Bottom left</option>
            <option value="bottom_right">Bottom right</option>
          </select>
        </div>
      </section>

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-3">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Shortcuts</h3>
        <Choice
          label="Cmd+Shift+Space (hold)"
          selected={local.shortcuts.preset === "cmd_shift_space"}
          onClick={() =>
            update({
              ...local,
              shortcuts: { ...local.shortcuts, preset: "cmd_shift_space" },
            })
          }
          disabled={busy}
        />
        <Choice
          label="Fn (hold)"
          selected={local.shortcuts.preset === "fn"}
          onClick={() =>
            update({
              ...local,
              shortcuts: { ...local.shortcuts, preset: "fn" },
            })
          }
          disabled={busy}
        />
      </section>

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-3">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Transcription</h3>
        <div className="grid grid-cols-2 gap-2">
          <ProviderTile
            title="API"
            subtitle="OpenAI Whisper"
            selected={local.transcription.provider === "api"}
            disabled={busy}
            onClick={() =>
              update({
                ...local,
                transcription: { ...local.transcription, provider: "api" },
              })
            }
          />
          <ProviderTile
            title="Local"
            subtitle={anyModelInstalled ? "On-device" : "Install a model first"}
            selected={local.transcription.provider === "local"}
            disabled={busy || !anyModelInstalled}
            onClick={() =>
              update({
                ...local,
                transcription: { ...local.transcription, provider: "local" },
              })
            }
          />
        </div>
        {local.transcription.provider === "local" && (
          <p className="text-[11px] text-neutral-500">
            Lab mode active: every dictation runs on API + 3 local models. Pick the best result.
          </p>
        )}
        <div className="space-y-2 pt-1">
          <p className="text-xs text-neutral-400">Local models</p>
          {models.length === 0 && (
            <p className="text-xs text-neutral-500">Loading…</p>
          )}
          {models.map((model) => {
            const progress = modelProgress[model.id];
            const isDownloading = downloadingModel === model.id;
            const percent =
              progress && progress.total_bytes > 0
                ? Math.min(100, Math.floor((progress.bytes_downloaded / progress.total_bytes) * 100))
                : 0;
            return (
              <div
                key={model.id}
                className="rounded-md border border-neutral-800 bg-neutral-950 p-3 space-y-2"
              >
                <div className="flex items-center justify-between gap-2">
                  <div className="min-w-0">
                    <p className="text-sm text-neutral-200">{model.display_name}</p>
                    <p className="text-[11px] text-neutral-500">
                      {formatBytes(model.expected_size_bytes)}{" "}
                      {model.installed ? (
                        <span className="text-emerald-400">• installed</span>
                      ) : (
                        <span className="text-neutral-500">• not installed</span>
                      )}
                    </p>
                  </div>
                  {model.installed ? (
                    <button
                      type="button"
                      onClick={() => removeModel(model.id)}
                      className="rounded-md border border-neutral-700 bg-neutral-900 px-3 py-1.5 text-xs text-neutral-200 hover:bg-neutral-800"
                    >
                      Delete
                    </button>
                  ) : (
                    <button
                      type="button"
                      onClick={() => startDownload(model.id)}
                      disabled={isDownloading}
                      className="rounded-md border border-neutral-700 bg-neutral-900 px-3 py-1.5 text-xs text-neutral-200 hover:bg-neutral-800 disabled:opacity-60"
                    >
                      {isDownloading ? "Downloading…" : "Download"}
                    </button>
                  )}
                </div>
                {isDownloading && progress && (
                  <div className="space-y-1">
                    <div className="h-1.5 w-full rounded-full bg-neutral-800">
                      <div
                        className="h-full rounded-full bg-emerald-500 transition-all"
                        style={{ width: `${percent}%` }}
                      />
                    </div>
                    <p className="text-[11px] text-neutral-500">
                      {formatBytes(progress.bytes_downloaded)} / {formatBytes(progress.total_bytes)} ({percent}%)
                    </p>
                  </div>
                )}
              </div>
            );
          })}
          {modelError && (
            <p className="text-xs text-red-300">{modelError}</p>
          )}
        </div>
      </section>

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-3">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">API</h3>

        <div className="space-y-2">
          <p className="text-xs text-neutral-400">Active provider</p>
          <div className="grid grid-cols-2 gap-2" role="radiogroup" aria-label="Active provider">
            {(Object.keys(PROVIDER_META) as Provider[]).map((key) => {
              const selected = activeProvider === key;
              const configured =
                key === "groq" ? groqConfigured : openaiConfigured;
              return (
                <button
                  key={key}
                  type="button"
                  role="radio"
                  aria-checked={selected}
                  disabled={apiBusy}
                  onClick={() => switchActiveProvider(key)}
                  className={`rounded-md border px-3 py-2 text-left text-sm transition-colors ${
                    selected
                      ? "border-emerald-500 bg-emerald-950/30 text-emerald-300"
                      : "border-neutral-700 bg-neutral-950 text-neutral-300 hover:border-neutral-600"
                  } ${apiBusy ? "opacity-60 cursor-not-allowed" : ""}`}
                >
                  <div className="font-medium">{PROVIDER_META[key].label}</div>
                  <div className="text-[11px] text-neutral-500">
                    {configured ? "configured" : "not configured"}
                  </div>
                </button>
              );
            })}
          </div>
        </div>

        <div className="space-y-2">
          <p className="text-xs text-neutral-400">Edit key for</p>
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
                  className={`rounded-md border px-3 py-1.5 text-xs transition-colors ${
                    selected
                      ? "border-neutral-500 bg-neutral-800 text-neutral-100"
                      : "border-neutral-700 bg-neutral-950 text-neutral-400 hover:border-neutral-600"
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
          className="w-full rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-sm text-neutral-200 placeholder-neutral-500"
        />
        <button
          type="button"
          onClick={saveApiKey}
          disabled={apiBusy}
          className="w-full rounded-md bg-white px-3 py-2 text-sm font-medium text-black hover:bg-neutral-200 disabled:opacity-60"
        >
          {apiBusy ? "Saving..." : `Save ${editingMeta.label} API Key`}
        </button>
        <a
          href={editingMeta.docsUrl}
          target="_blank"
          rel="noopener noreferrer"
          className="block text-[11px] text-neutral-500 hover:text-neutral-300 underline"
        >
          How to get a {editingMeta.label} API key →
        </a>
        {apiMessage && <p className="text-xs text-neutral-300">{apiMessage}</p>}
      </section>

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-2">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Microphone</h3>
        <p className="text-xs text-neutral-300">Input device: System default</p>
        <DisabledToggle label="Noise suppression" />
      </section>

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-3">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Language</h3>
        <Choice
          label="System"
          selected={local.language.mode === "system"}
          onClick={() =>
            update({
              ...local,
              language: { ...local.language, mode: "system" },
            })
          }
          disabled={busy}
        />
        <Choice
          label="English"
          selected={local.language.mode === "en"}
          onClick={() =>
            update({
              ...local,
              language: { ...local.language, mode: "en" },
            })
          }
          disabled={busy}
        />
      </section>

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-2">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Sound Settings</h3>
        <DisabledToggle label="Feedback sounds" />
      </section>

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-3">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">Extras</h3>
        <DisabledToggle label="Auto-add to dictionary" />
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
          <p className="text-xs text-amber-300">
            Warning: This bypasses permission checks in onboarding and may cause injection failures.
          </p>
        )}
      </section>

      <section className="rounded-lg border border-neutral-800 bg-neutral-900 p-3 space-y-3">
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">About</h3>
        <div className="rounded-md border border-neutral-800 bg-neutral-950 p-3 space-y-2">
          <div className="flex items-center justify-between gap-2">
            <div className="min-w-0">
              <p className="text-sm text-neutral-200">FlowingThoughts</p>
              <p className="text-[11px] text-neutral-500 font-mono">
                {appVersion
                  ? `v${appVersion.version} (${appVersion.commit})`
                  : "Loading…"}
              </p>
            </div>
            <button
              type="button"
              onClick={copyVersion}
              disabled={!appVersion}
              className={`text-xs px-2 py-1 rounded border transition-colors ${
                versionCopied
                  ? "border-emerald-600 bg-emerald-900/40 text-emerald-300"
                  : "border-neutral-700 bg-neutral-900 text-neutral-300 hover:bg-neutral-800"
              } disabled:opacity-50`}
            >
              {versionCopied ? "Copied" : "Copy"}
            </button>
          </div>
          <div className="flex items-center justify-between gap-2 pt-1">
            <p className="text-xs text-neutral-400">
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
                className="text-xs px-3 py-1 rounded bg-emerald-700 hover:bg-emerald-600 text-white"
              >
                Install & restart
              </button>
            ) : (
              <button
                type="button"
                onClick={checkForUpdates}
                disabled={updateStatus === "checking" || updateStatus === "installing"}
                className="text-xs px-3 py-1 rounded border border-neutral-700 bg-neutral-900 text-neutral-200 hover:bg-neutral-800 disabled:opacity-50"
              >
                {updateStatus === "checking" ? "Checking…" : "Check for updates"}
              </button>
            )}
          </div>
        </div>
      </section>

      {warnings.length > 0 && (
        <div className="rounded-lg border border-amber-900 bg-amber-950/40 p-3 text-xs text-amber-300">
          {warnings.map((warning, index) => (
            <p key={index}>{warning}</p>
          ))}
        </div>
      )}

      {error && (
        <div className="rounded-lg border border-red-900 bg-red-950/40 p-3 text-xs text-red-300">
          {error}
        </div>
      )}
    </div>
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
    <label className="flex items-center justify-between gap-3 text-sm text-neutral-200">
      <span className="min-w-0 flex-1">{label}</span>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        disabled={disabled}
        onClick={() => onChange(!checked)}
        className={`h-6 w-11 shrink-0 rounded-full px-0.5 flex items-center transition-colors ${
          checked ? "bg-emerald-500" : "bg-neutral-700"
        } ${disabled ? "opacity-60 cursor-not-allowed" : ""}`}
      >
        <span
          className={`h-5 w-5 rounded-full bg-white transition-all ${
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
      className={`w-full rounded-md border px-3 py-2 text-left text-sm transition-colors ${
        selected
          ? "border-emerald-500 bg-emerald-950/30 text-emerald-300"
          : "border-neutral-700 bg-neutral-950 text-neutral-300 hover:border-neutral-600"
      } ${disabled ? "opacity-60 cursor-not-allowed" : ""}`}
    >
      {label}
    </button>
  );
}

function DisabledToggle({ label }: { label: string }) {
  return (
    <div className="flex items-center justify-between gap-3 text-sm text-neutral-500">
      <span>{label}</span>
      <span className="text-[10px] uppercase tracking-wide text-neutral-600">Coming soon</span>
    </div>
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
      className={`rounded-md border px-3 py-3 text-left transition-colors ${
        selected
          ? "border-emerald-500 bg-emerald-950/30"
          : "border-neutral-700 bg-neutral-950 hover:border-neutral-600"
      } ${disabled ? "opacity-60 cursor-not-allowed" : ""}`}
    >
      <p className={`text-sm font-medium ${selected ? "text-emerald-300" : "text-neutral-200"}`}>
        {title}
      </p>
      <p className="text-[11px] text-neutral-500">{subtitle}</p>
    </button>
  );
}

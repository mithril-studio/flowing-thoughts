import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  type AppSettings,
  type AppSettingsUpdateResult,
  defaultAppSettings,
} from "../types/settings";

interface SettingsProps {
  settings: AppSettings;
  onSettingsChange: (settings: AppSettings) => void;
}

interface AccessibilityHelpInfo {
  executable_path: string;
  is_dev_build: boolean;
  note: string;
}

export default function Settings({ settings, onSettingsChange }: SettingsProps) {
  const [local, setLocal] = useState<AppSettings>(settings ?? defaultAppSettings);
  const [busy, setBusy] = useState(false);
  const [apiBusy, setApiBusy] = useState(false);
  const [setupBusy, setSetupBusy] = useState(false);
  const [apiKey, setApiKey] = useState("");
  const [hasApiKey, setHasApiKey] = useState(false);
  const [apiMessage, setApiMessage] = useState<string | null>(null);
  const [accessibilityGranted, setAccessibilityGranted] = useState<boolean | null>(null);
  const [helpInfo, setHelpInfo] = useState<AccessibilityHelpInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);

  useEffect(() => {
    setLocal(settings ?? defaultAppSettings);
  }, [settings]);

  useEffect(() => {
    invoke<boolean>("has_openai_api_key")
      .then((value) => setHasApiKey(Boolean(value)))
      .catch(() => {
        setHasApiKey(false);
      });
  }, []);

  useEffect(() => {
    invoke<AccessibilityHelpInfo>("get_accessibility_help_info")
      .then((info) => setHelpInfo(info))
      .catch(() => {
        // Non-blocking helper content.
      });
  }, []);

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

  const saveApiKey = async () => {
    if (!apiKey.trim().startsWith("sk-")) {
      setApiMessage("Please enter a valid OpenAI API key.");
      return;
    }
    setApiBusy(true);
    setApiMessage(null);
    try {
      await invoke("set_openai_api_key", { key: apiKey.trim() });
      setHasApiKey(true);
      setApiKey("");
      setApiMessage("API key saved.");
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
        <h3 className="text-xs text-neutral-400 uppercase tracking-wide">API</h3>
        <p className="text-xs text-neutral-300">
          OpenAI API key:{" "}
          <span className={hasApiKey ? "text-emerald-300" : "text-neutral-400"}>
            {hasApiKey ? "configured" : "not configured"}
          </span>
        </p>
        <input
          type="password"
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
          placeholder="sk-..."
          className="w-full rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-sm text-neutral-200 placeholder-neutral-500"
        />
        <button
          type="button"
          onClick={saveApiKey}
          disabled={apiBusy}
          className="w-full rounded-md bg-white px-3 py-2 text-sm font-medium text-black hover:bg-neutral-200 disabled:opacity-60"
        >
          {apiBusy ? "Saving..." : "Save API Key"}
        </button>
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

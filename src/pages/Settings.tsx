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

export default function Settings({ settings, onSettingsChange }: SettingsProps) {
  const [local, setLocal] = useState<AppSettings>(settings ?? defaultAppSettings);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);

  useEffect(() => {
    setLocal(settings ?? defaultAppSettings);
  }, [settings]);

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

  return (
    <div className="h-full overflow-y-auto px-4 py-4 space-y-4">
      <h2 className="text-sm font-medium text-white">Settings</h2>

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

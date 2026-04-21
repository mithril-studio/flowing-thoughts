import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Provider = "groq" | "openai";

interface PersistedStateView {
  onboarding_complete: boolean;
  groq_api_key_configured: boolean;
  openai_api_key_configured: boolean;
  active_provider: Provider;
  settings?: {
    extras?: {
      dangerously_skip_permissions?: boolean;
    };
  };
}

interface AccessibilityHelpInfo {
  executable_path: string;
  is_dev_build: boolean;
  note: string;
}

interface OnboardingProps {
  onComplete: () => void;
}

interface ProviderMeta {
  label: string;
  placeholder: string;
  prefix: string;
  docsUrl: string;
  tagline: string;
}

const PROVIDER_META: Record<Provider, ProviderMeta> = {
  groq: {
    label: "Groq",
    placeholder: "gsk_...",
    prefix: "gsk_",
    docsUrl: "https://console.groq.com/keys",
    tagline: "Fast Whisper, generous free tier.",
  },
  openai: {
    label: "OpenAI",
    placeholder: "sk-...",
    prefix: "sk-",
    docsUrl: "https://platform.openai.com/api-keys",
    tagline: "Official Whisper-1, pay-as-you-go.",
  },
};

export default function Onboarding({ onComplete }: OnboardingProps) {
  const [step, setStep] = useState(1);
  const [provider, setProvider] = useState<Provider>("groq");
  const [apiKey, setApiKey] = useState("");
  const [accessibilityGranted, setAccessibilityGranted] = useState(false);
  const [dangerouslySkipPermissions, setDangerouslySkipPermissions] = useState(false);
  const [accessibilityHelp, setAccessibilityHelp] = useState<AccessibilityHelpInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const invokeWithTimeout = async <T,>(
    command: string,
    args?: Record<string, unknown>,
    timeoutMs = 8000
  ): Promise<T> => {
    return Promise.race([
      invoke<T>(command, args),
      new Promise<T>((_, reject) =>
        setTimeout(() => reject(new Error(`"${command}" timed out after ${timeoutMs}ms`)), timeoutMs)
      ),
    ]);
  };

  useEffect(() => {
    invoke<PersistedStateView>("get_persisted_state")
      .then((state) => {
        const skipPermissions = Boolean(
          state.settings?.extras?.dangerously_skip_permissions
        );
        setDangerouslySkipPermissions(skipPermissions);

        const hasAnyKey =
          state.groq_api_key_configured || state.openai_api_key_configured;
        if (state.active_provider === "openai" || state.active_provider === "groq") {
          setProvider(state.active_provider);
        }
        if (!hasAnyKey) {
          setStep(1);
          return;
        }
        setStep(skipPermissions ? 3 : 2);
      })
      .catch(() => {
        // Keep default step when backend command is unavailable.
      });

    invoke<AccessibilityHelpInfo>("get_accessibility_help_info")
      .then((info) => setAccessibilityHelp(info))
      .catch(() => {
        // Non-blocking helper info.
      });
  }, []);

  const meta = PROVIDER_META[provider];

  const saveApiKey = async () => {
    if (!apiKey.trim().startsWith(meta.prefix)) {
      setError(`Please enter a valid ${meta.label} API key.`);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await invoke("set_api_key", { provider, key: apiKey.trim() });
      setStep(2);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
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
          "Accessibility is still disabled. Enable it in System Settings or continue anyway."
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

  return (
    <div className="h-full bg-neutral-950 text-white px-6 py-8 flex items-center justify-center">
      <div className="w-full max-w-md rounded-xl border border-neutral-800 bg-neutral-900 p-5">
        <h1 className="text-lg font-semibold">Setup FlowingThoughts</h1>
        <p className="text-xs text-neutral-400 mt-1">Step {step} of 3</p>

        {step === 1 && (
          <div className="mt-5">
            <p className="text-sm text-neutral-200 mb-2">Choose your transcription provider</p>
            <div className="grid grid-cols-2 gap-2 mb-3" role="radiogroup" aria-label="Provider">
              {(Object.keys(PROVIDER_META) as Provider[]).map((key) => {
                const selected = provider === key;
                return (
                  <button
                    key={key}
                    type="button"
                    role="radio"
                    aria-checked={selected}
                    onClick={() => {
                      setProvider(key);
                      setError(null);
                      setApiKey("");
                    }}
                    className={`rounded-md border px-3 py-2 text-sm text-left transition-colors ${
                      selected
                        ? "border-emerald-500 bg-emerald-950/30 text-emerald-300"
                        : "border-neutral-700 bg-neutral-950 text-neutral-300 hover:border-neutral-600"
                    }`}
                  >
                    <div className="font-medium">{PROVIDER_META[key].label}</div>
                    <div className="text-[11px] text-neutral-500">
                      {PROVIDER_META[key].tagline}
                    </div>
                  </button>
                );
              })}
            </div>
            <p className="text-sm text-neutral-200 mb-2">Enter your {meta.label} API key</p>
            <p className="text-xs text-neutral-500 mb-3">
              FlowingThoughts uses your own key for transcription — you can switch providers anytime in Settings.
            </p>
            <input
              type="password"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder={meta.placeholder}
              className="w-full text-sm px-3 py-2 rounded-md bg-neutral-800 border border-neutral-700 text-white placeholder-neutral-500 focus:outline-none"
            />
            <button
              onClick={saveApiKey}
              disabled={busy}
              className="w-full mt-3 text-sm py-2 rounded-md bg-white text-black font-medium hover:bg-neutral-200 disabled:opacity-60"
            >
              {busy ? "Saving..." : "Save API Key"}
            </button>
            <a
              href={meta.docsUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="block mt-2 text-xs text-neutral-500 hover:text-neutral-300 underline"
            >
              How to get a {meta.label} API key →
            </a>
          </div>
        )}

        {step === 2 && (
          <div className="mt-5">
            <p className="text-sm text-neutral-200 mb-2">
              Grant Accessibility permission so FlowingThoughts can paste into apps
            </p>
            <div className="flex gap-2">
              <button
                onClick={() => invoke("open_accessibility_settings").catch((e) => setError(String(e)))}
                className="flex-1 text-sm py-2 rounded-md bg-neutral-800 text-neutral-200 hover:bg-neutral-700"
              >
                Open Settings
              </button>
              <button
                onClick={checkAccessibility}
                disabled={busy}
                className="flex-1 text-sm py-2 rounded-md bg-white text-black font-medium hover:bg-neutral-200 disabled:opacity-60"
              >
                {busy ? "Checking..." : "Check Access"}
              </button>
            </div>
            <button
              onClick={() => setStep(3)}
              className="w-full mt-2 text-sm py-2 rounded-md border border-neutral-700 bg-neutral-900 text-neutral-300 hover:bg-neutral-800"
            >
              Continue Anyway
            </button>
            <button
              onClick={() => invoke("reveal_current_executable").catch((e) => setError(String(e)))}
              className="w-full mt-2 text-sm py-2 rounded-md border border-neutral-700 bg-neutral-900 text-neutral-300 hover:bg-neutral-800"
            >
              Reveal Running App in Finder
            </button>
            {accessibilityHelp && (
              <div className="mt-2 rounded-md border border-neutral-800 bg-neutral-950 p-2">
                <p className="text-[11px] text-neutral-400">{accessibilityHelp.note}</p>
                <p className="text-[11px] text-neutral-500 mt-1 break-all">
                  {accessibilityHelp.executable_path}
                </p>
              </div>
            )}
            {dangerouslySkipPermissions && (
              <p className="text-xs text-amber-300 mt-2">
                Permission checks are skipped by settings.
              </p>
            )}
            {accessibilityGranted && (
              <p className="text-xs text-emerald-400 mt-2">Accessibility permission detected.</p>
            )}
          </div>
        )}

        {step === 3 && (
          <div className="mt-5">
            <p className="text-sm text-neutral-200 mb-2">
              Click a text field in any app, then press the button — FlowingThoughts
              will paste a test sentence there to confirm everything works.
            </p>
            <button
              onClick={() => finish(true)}
              disabled={busy}
              className="w-full text-sm py-2 rounded-md bg-white text-black font-medium hover:bg-neutral-200 disabled:opacity-60"
            >
              {busy ? "Running..." : "Run End-to-End Test"}
            </button>
            <button
              onClick={() => finish(false)}
              disabled={busy}
              className="w-full mt-2 text-sm py-2 rounded-md border border-neutral-700 bg-neutral-900 text-neutral-300 hover:bg-neutral-800 disabled:opacity-60"
            >
              Skip Test and Finish
            </button>
          </div>
        )}

        {error && <p className="text-xs text-red-400 mt-3">{error}</p>}
      </div>
    </div>
  );
}

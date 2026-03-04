import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface PersistedStateView {
  onboarding_complete: boolean;
  license_key: string | null;
  has_openai_api_key: boolean;
}

interface OnboardingProps {
  onComplete: () => void;
}

export default function Onboarding({ onComplete }: OnboardingProps) {
  const [step, setStep] = useState(1);
  const [licenseKey, setLicenseKey] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [accessibilityGranted, setAccessibilityGranted] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    invoke<PersistedStateView>("get_persisted_state")
      .then((state) => {
        if (state.license_key) {
          setLicenseKey(state.license_key);
          setStep(2);
        }
        if (state.has_openai_api_key) {
          setStep(3);
        }
      })
      .catch(() => {
        // Keep default step when backend command is unavailable.
      });
  }, []);

  const saveLicense = () => {
    if (licenseKey.trim().length < 8) {
      setError("Please enter a valid license key.");
      return;
    }
    setError(null);
    setStep(2);
  };

  const saveApiKey = async () => {
    if (!apiKey.trim().startsWith("sk-")) {
      setError("Please enter a valid OpenAI API key.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await invoke("set_openai_api_key", { key: apiKey.trim() });
      setStep(3);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const checkAccessibility = async () => {
    setBusy(true);
    setError(null);
    try {
      const granted = await invoke<boolean>("check_accessibility_permission");
      setAccessibilityGranted(granted);
      if (granted) setStep(4);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const runTest = async () => {
    setBusy(true);
    setError(null);
    try {
      await invoke("save_onboarding_state", {
        license_key: licenseKey.trim(),
        onboarding_complete: true,
      });
      await invoke("run_injection_test");
      onComplete();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="h-screen bg-neutral-950 text-white px-6 py-8 flex items-center justify-center">
      <div className="w-full max-w-md rounded-xl border border-neutral-800 bg-neutral-900 p-5">
        <h1 className="text-lg font-semibold">Setup Open Voice Wispr</h1>
        <p className="text-xs text-neutral-400 mt-1">Step {step} of 4</p>

        {step === 1 && (
          <div className="mt-5">
            <p className="text-sm text-neutral-200 mb-2">Enter your license key</p>
            <input
              type="text"
              value={licenseKey}
              onChange={(e) => setLicenseKey(e.target.value)}
              placeholder="License key"
              className="w-full text-sm px-3 py-2 rounded-md bg-neutral-800 border border-neutral-700 text-white placeholder-neutral-500 focus:outline-none"
            />
            <button
              onClick={saveLicense}
              className="w-full mt-3 text-sm py-2 rounded-md bg-white text-black font-medium hover:bg-neutral-200"
            >
              Continue
            </button>
          </div>
        )}

        {step === 2 && (
          <div className="mt-5">
            <p className="text-sm text-neutral-200 mb-2">Enter your OpenAI API key</p>
            <input
              type="password"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder="sk-..."
              className="w-full text-sm px-3 py-2 rounded-md bg-neutral-800 border border-neutral-700 text-white placeholder-neutral-500 focus:outline-none"
            />
            <button
              onClick={saveApiKey}
              disabled={busy}
              className="w-full mt-3 text-sm py-2 rounded-md bg-white text-black font-medium hover:bg-neutral-200 disabled:opacity-60"
            >
              {busy ? "Saving..." : "Save API Key"}
            </button>
          </div>
        )}

        {step === 3 && (
          <div className="mt-5">
            <p className="text-sm text-neutral-200 mb-2">
              Grant Accessibility permission so the app can type text
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
            {accessibilityGranted && (
              <p className="text-xs text-emerald-400 mt-2">Accessibility permission detected.</p>
            )}
          </div>
        )}

        {step === 4 && (
          <div className="mt-5">
            <p className="text-sm text-neutral-200 mb-2">
              Click test, then focus any text field. The app will paste a test sentence.
            </p>
            <button
              onClick={runTest}
              disabled={busy}
              className="w-full text-sm py-2 rounded-md bg-white text-black font-medium hover:bg-neutral-200 disabled:opacity-60"
            >
              {busy ? "Running..." : "Run End-to-End Test"}
            </button>
          </div>
        )}

        {error && <p className="text-xs text-red-400 mt-3">{error}</p>}
      </div>
    </div>
  );
}

import { useEffect, useState } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

type Status = "idle" | "checking" | "available" | "installing" | "error";

export default function UpdateBanner() {
  const [status, setStatus] = useState<Status>("idle");
  const [update, setUpdate] = useState<Update | null>(null);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setStatus("checking");
    check()
      .then((result) => {
        if (cancelled) return;
        if (result) {
          setUpdate(result);
          setStatus("available");
        } else {
          setStatus("idle");
        }
      })
      .catch((err) => {
        if (cancelled) return;
        setErrorMessage(String(err));
        setStatus("error");
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const installUpdate = async () => {
    if (!update) return;
    setStatus("installing");
    try {
      await update.downloadAndInstall();
      await relaunch();
    } catch (err) {
      setErrorMessage(String(err));
      setStatus("error");
    }
  };

  if (status !== "available" && status !== "installing" && status !== "error") {
    return null;
  }

  if (status === "error") {
    return (
      <div className="shrink-0 bg-red-900/60 text-red-100 text-xs px-3 py-2 flex items-center justify-between">
        <span>Update check failed: {errorMessage}</span>
        <button
          type="button"
          onClick={() => setStatus("idle")}
          className="ml-3 underline hover:no-underline"
        >
          Dismiss
        </button>
      </div>
    );
  }

  return (
    <div className="shrink-0 bg-emerald-900/70 text-emerald-100 text-xs px-3 py-2 flex items-center justify-between">
      <span>
        Update available{update?.version ? ` — v${update.version}` : ""}
      </span>
      <button
        type="button"
        onClick={installUpdate}
        disabled={status === "installing"}
        className="ml-3 bg-emerald-700 hover:bg-emerald-600 text-white rounded px-2 py-0.5 disabled:opacity-50"
      >
        {status === "installing" ? "Installing..." : "Install & restart"}
      </button>
    </div>
  );
}

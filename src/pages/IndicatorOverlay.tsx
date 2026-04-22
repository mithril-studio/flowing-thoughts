import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

type SessionPhase = "idle" | "recording" | "transcribing" | "injecting" | "error";

function colorFor(phase: SessionPhase): string {
  switch (phase) {
    case "recording":
      return "#ef4444"; // red-500
    case "transcribing":
      return "#fbbf24"; // amber-400
    case "injecting":
      return "#34d399"; // emerald-400
    case "error":
      return "#b91c1c"; // red-700
    default:
      return "#a3a3a3"; // neutral-400 — idle
  }
}

export default function IndicatorOverlay() {
  const [phase, setPhase] = useState<SessionPhase>("idle");

  useEffect(() => {
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
    document.body.style.margin = "0";
    document.body.style.overflow = "hidden";
  }, []);

  useEffect(() => {
    const unlistenPhase = listen<{ phase: SessionPhase }>(
      "session-phase",
      (event) => setPhase(event.payload.phase),
    );
    const unlistenComplete = listen("transcription-complete", () =>
      setPhase("idle"),
    );
    const unlistenError = listen("pipeline-error", () => setPhase("error"));

    return () => {
      unlistenPhase.then((fn) => fn());
      unlistenComplete.then((fn) => fn());
      unlistenError.then((fn) => fn());
    };
  }, []);

  const color = colorFor(phase);

  return (
    <div
      data-tauri-drag-region
      style={{
        width: "100vw",
        height: "100vh",
        background: "transparent",
        borderRadius: 9999,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        cursor: "grab",
        userSelect: "none",
        WebkitUserSelect: "none",
        border: "1px solid rgba(255,255,255,0.22)",
        boxSizing: "border-box",
      }}
    >
      <div
        style={{
          width: 8,
          height: 8,
          borderRadius: "9999px",
          background: color,
          transition: "background-color 200ms ease-out",
          pointerEvents: "none",
        }}
      />
    </div>
  );
}

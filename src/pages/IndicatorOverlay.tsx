import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import Waveform, { type WavePhase } from "../components/Waveform";

/**
 * The small always-on-top pill that floats on screen. Draggable anywhere;
 * shows a live waveform while you speak, a sweep while transcribing.
 */
export default function IndicatorOverlay() {
  const [phase, setPhase] = useState<WavePhase>("idle");
  const [amplitude, setAmplitude] = useState(0);

  useEffect(() => {
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
    document.body.style.margin = "0";
    document.body.style.overflow = "hidden";
  }, []);

  useEffect(() => {
    const unlistenPhase = listen<{ phase: WavePhase }>(
      "session-phase",
      (event) => {
        setPhase(event.payload.phase);
        if (event.payload.phase !== "recording") setAmplitude(0);
      },
    );
    const unlistenAmplitude = listen<{ amplitude: number }>(
      "recording-amplitude",
      (event) => setAmplitude(event.payload.amplitude ?? 0),
    );
    const unlistenComplete = listen("transcription-complete", () =>
      setPhase("idle"),
    );
    const unlistenError = listen("pipeline-error", () => {
      setPhase("error");
      // Flash the error state briefly, then settle back to idle.
      setTimeout(() => setPhase("idle"), 1600);
    });

    return () => {
      unlistenPhase.then((fn) => fn());
      unlistenAmplitude.then((fn) => fn());
      unlistenComplete.then((fn) => fn());
      unlistenError.then((fn) => fn());
    };
  }, []);

  const active = phase !== "idle";

  return (
    <div
      data-tauri-drag-region
      style={{
        width: "100vw",
        height: "100vh",
        boxSizing: "border-box",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        cursor: "grab",
        userSelect: "none",
        WebkitUserSelect: "none",
        background: active ? "rgba(10,10,12,0.92)" : "rgba(10,10,12,0.72)",
        border: "1px solid rgba(255,255,255,0.14)",
        borderRadius: 9999,
        boxShadow: "0 4px 24px rgba(0,0,0,0.35)",
        transition: "background 200ms ease-out",
        overflow: "hidden",
      }}
    >
      <div style={{ pointerEvents: "none", width: "70%", height: "58%" }}>
        <Waveform
          phase={phase}
          amplitude={amplitude}
          bars={11}
          className="w-full h-full"
          barClassName={
            phase === "error" ? "w-[3px] bg-red-400" : "w-[3px] bg-white"
          }
        />
      </div>
    </div>
  );
}

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import Waveform, { type WavePhase } from "../components/Waveform";

/**
 * The small always-on-top emblem that floats on screen (all Spaces, above
 * fullscreen apps). Idle: a glowing orb — the FlowingThoughts logo — so you
 * always know dictation is ready. Active: a pill with a live waveform while
 * you speak and a sweep while transcribing. Draggable anywhere.
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
        background: active ? "rgba(10,10,12,0.92)" : "transparent",
        border: active
          ? "1px solid rgba(255,255,255,0.14)"
          : "1px solid transparent",
        borderRadius: 9999,
        boxShadow: active ? "0 4px 24px rgba(0,0,0,0.35)" : "none",
        transition: "background 200ms ease-out",
        overflow: "hidden",
      }}
    >
      {active ? (
        <div style={{ pointerEvents: "none", width: "72%", height: "60%" }}>
          <Waveform
            phase={phase}
            amplitude={amplitude}
            bars={7}
            gapClassName="gap-[2px]"
            className="w-full h-full"
            barClassName={
              phase === "error" ? "w-[2px] bg-red-400" : "w-[2px] bg-white"
            }
          />
        </div>
      ) : (
        // Idle emblem: the glowing orb from the app logo.
        <div
          style={{
            pointerEvents: "none",
            width: 15,
            height: 15,
            borderRadius: "50%",
            background:
              "radial-gradient(circle at 50% 45%, #2C2C32 0%, #17171B 55%, #0A0A0C 100%)",
            border: "1px solid rgba(255,255,255,0.28)",
            boxShadow:
              "0 0 8px rgba(120,120,140,0.55), 0 0 2px rgba(255,255,255,0.35), 0 1px 4px rgba(0,0,0,0.5)",
          }}
        />
      )}
    </div>
  );
}

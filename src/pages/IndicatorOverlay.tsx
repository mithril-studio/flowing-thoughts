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
      return "#525252"; // neutral-600
  }
}

export default function IndicatorOverlay() {
  const [phase, setPhase] = useState<SessionPhase>("idle");
  const [amplitude, setAmplitude] = useState(0);

  useEffect(() => {
    document.documentElement.style.background = "#0a0a0a";
    document.body.style.background = "#0a0a0a";
    document.body.style.margin = "0";
    document.body.style.overflow = "hidden";
  }, []);

  useEffect(() => {
    const unlistenPhase = listen<{ phase: SessionPhase }>(
      "session-phase",
      (event) => {
        setPhase(event.payload.phase);
        if (event.payload.phase !== "recording") {
          setAmplitude(0);
        }
      }
    );
    const unlistenAmplitude = listen<{ amplitude: number }>(
      "recording-amplitude",
      (event) => {
        setAmplitude(event.payload.amplitude ?? 0);
      }
    );
    const unlistenComplete = listen("transcription-complete", () => {
      setPhase("idle");
      setAmplitude(0);
    });
    const unlistenError = listen("pipeline-error", () => {
      setPhase("error");
      setAmplitude(0);
    });

    return () => {
      unlistenPhase.then((fn) => fn());
      unlistenAmplitude.then((fn) => fn());
      unlistenComplete.then((fn) => fn());
      unlistenError.then((fn) => fn());
    };
  }, []);

  const color = colorFor(phase);
  const amp = Math.min(1, Math.max(0, amplitude));

  const coreSize =
    phase === "recording"
      ? 18 + amp * 28
      : phase === "idle"
        ? 14
        : 22;

  const glowSize =
    phase === "recording"
      ? 36 + amp * 60
      : phase === "idle"
        ? 0
        : 42;

  const glowOpacity =
    phase === "recording"
      ? Math.min(1, 0.35 + amp * 1.5)
      : phase === "idle"
        ? 0
        : 0.55;

  const idleRing = phase === "idle";

  return (
    <div
      data-tauri-drag-region
      style={{
        width: "100vw",
        height: "100vh",
        background: "#0a0a0a",
        borderRadius: 16,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        cursor: "grab",
        userSelect: "none",
        WebkitUserSelect: "none",
      }}
    >
      <div
        data-tauri-drag-region
        style={{
          position: "relative",
          width: 72,
          height: 72,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        {/* glow halo */}
        <div
          style={{
            position: "absolute",
            width: glowSize,
            height: glowSize,
            borderRadius: "9999px",
            background: color,
            filter: "blur(14px)",
            opacity: glowOpacity,
            transition: "width 80ms ease-out, height 80ms ease-out, opacity 120ms ease-out, background-color 200ms ease-out",
            pointerEvents: "none",
          }}
        />
        {/* pulsing ping ring while recording */}
        {phase === "recording" && (
          <div
            style={{
              position: "absolute",
              width: 46,
              height: 46,
              borderRadius: "9999px",
              border: `2px solid ${color}`,
              opacity: 0.6,
              animation: "indicator-ping 1.2s cubic-bezier(0,0,0.2,1) infinite",
              pointerEvents: "none",
            }}
          />
        )}
        {/* core dot */}
        <div
          style={{
            width: coreSize,
            height: coreSize,
            borderRadius: "9999px",
            background: color,
            boxShadow:
              phase === "recording"
                ? `0 0 ${12 + amp * 28}px ${color}`
                : phase === "idle"
                  ? "0 0 0 2px rgba(255,255,255,0.35)"
                  : `0 0 10px ${color}`,
            border: idleRing ? "2px solid rgba(255,255,255,0.5)" : "none",
            opacity: 1,
            transition: "width 70ms ease-out, height 70ms ease-out, background-color 200ms ease-out, box-shadow 120ms ease-out",
          }}
        />
      </div>
      <style>{`
        @keyframes indicator-ping {
          0%   { transform: scale(1);   opacity: 0.6; }
          75%  { transform: scale(1.9); opacity: 0;   }
          100% { transform: scale(1.9); opacity: 0;   }
        }
      `}</style>
    </div>
  );
}

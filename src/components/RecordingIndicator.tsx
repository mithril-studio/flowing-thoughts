interface RecordingIndicatorProps {
  mode: "idle" | "recording" | "transcribing" | "injecting" | "error";
  idleLabel: string;
  amplitude?: number;
}

export default function RecordingIndicator({
  mode,
  idleLabel,
  amplitude = 0,
}: RecordingIndicatorProps) {
  const label =
    mode === "recording"
      ? "Listening..."
      : mode === "transcribing"
        ? "Transcribing..."
        : mode === "injecting"
          ? "Typing..."
          : mode === "error"
            ? "Pipeline error"
            : idleLabel;

  const baseColor =
    mode === "recording"
      ? "bg-red-500"
      : mode === "transcribing"
        ? "bg-amber-400"
        : mode === "injecting"
          ? "bg-emerald-400"
          : mode === "error"
            ? "bg-red-700"
            : "bg-neutral-700";

  const scale =
    mode === "recording"
      ? 1 + Math.min(0.9, Math.max(0, amplitude) * 3)
      : mode === "idle"
        ? 1
        : 1.15;

  const glowOpacity =
    mode === "recording"
      ? Math.min(1, 0.35 + amplitude * 2)
      : mode === "idle"
        ? 0
        : 0.5;

  return (
    <div className="flex flex-col items-center gap-3 select-none">
      <div className="relative w-24 h-24 flex items-center justify-center">
        {/* Outer glow ring — grows with amplitude while recording */}
        <div
          className={`absolute inset-0 rounded-full ${baseColor} blur-xl transition-all duration-100`}
          style={{
            transform: `scale(${scale * 0.95})`,
            opacity: glowOpacity,
          }}
        />
        {/* Pulsing ring while recording */}
        {mode === "recording" && (
          <div
            className={`absolute inset-2 rounded-full border ${
              baseColor.replace("bg-", "border-")
            } opacity-50 animate-ping`}
          />
        )}
        {/* Core circle */}
        <div
          className={`relative rounded-full ${baseColor} transition-transform duration-75`}
          style={{
            width: "48px",
            height: "48px",
            transform: `scale(${scale})`,
            boxShadow:
              mode === "recording"
                ? `0 0 ${16 + amplitude * 40}px rgba(239,68,68,${0.4 + amplitude})`
                : undefined,
          }}
        />
      </div>
      <span className="text-xs text-neutral-400 tracking-wide">{label}</span>
    </div>
  );
}

interface RecordingIndicatorProps {
  mode: "idle" | "recording" | "transcribing" | "error";
}

export default function RecordingIndicator({ mode }: RecordingIndicatorProps) {
  const dotClass =
    mode === "recording"
      ? "bg-red-500 animate-pulse"
      : mode === "transcribing"
        ? "bg-amber-400 animate-pulse"
        : mode === "error"
          ? "bg-red-700"
          : "bg-neutral-600";

  const label =
    mode === "recording"
      ? "Recording..."
      : mode === "transcribing"
        ? "Transcribing..."
        : mode === "error"
          ? "Pipeline error"
          : "Press fn to record";

  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded-full bg-neutral-900 border border-neutral-800">
      <div className={`w-2.5 h-2.5 rounded-full transition-colors ${dotClass}`} />
      <span className="text-xs text-neutral-400">{label}</span>
    </div>
  );
}

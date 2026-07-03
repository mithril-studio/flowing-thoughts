import Waveform, { type WavePhase } from "./Waveform";

interface RecordingIndicatorProps {
  mode: WavePhase;
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

  return (
    <div className="flex flex-col items-center gap-3 select-none">
      <div
        className={`flex h-14 w-44 items-center justify-center rounded-full border shadow-lg transition-colors duration-200 ${
          mode === "error"
            ? "border-red-500/30 bg-red-950/60"
            : mode === "idle"
              ? "border-zinc-800 bg-zinc-900/80"
              : "border-zinc-700 bg-zinc-900"
        }`}
      >
        <Waveform
          phase={mode}
          amplitude={amplitude}
          bars={13}
          className="h-7 w-28"
          barClassName={
            mode === "error"
              ? "w-[3px] bg-red-400"
              : mode === "idle"
                ? "w-[3px] bg-zinc-500"
                : "w-[3px] bg-white"
          }
        />
      </div>
      <span className="text-xs text-zinc-400 tracking-wide">{label}</span>
    </div>
  );
}

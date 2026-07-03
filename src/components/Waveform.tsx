import { useEffect, useRef } from "react";

export type WavePhase =
  | "idle"
  | "recording"
  | "transcribing"
  | "injecting"
  | "error";

interface WaveformProps {
  phase: WavePhase;
  /** Latest microphone amplitude in [0, 1]. Only used while recording. */
  amplitude: number;
  bars?: number;
  className?: string;
  barClassName?: string;
}

/**
 * Animated dictation waveform: a row of bars that idle as small dots, dance
 * with the microphone level while recording, and sweep while transcribing.
 * Animation runs on requestAnimationFrame with per-frame smoothing so it
 * never stutters when amplitude events arrive irregularly.
 */
export default function Waveform({
  phase,
  amplitude,
  bars = 11,
  className = "",
  barClassName = "w-[3px] bg-white",
}: WaveformProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const heightsRef = useRef<number[]>([]);
  const ampRef = useRef(amplitude);
  const phaseRef = useRef(phase);
  ampRef.current = amplitude;
  phaseRef.current = phase;

  useEffect(() => {
    let raf = 0;
    const start = performance.now();

    const tick = (now: number) => {
      const t = (now - start) / 1000;
      const el = containerRef.current;
      if (el) {
        const children = el.children;
        const mid = (children.length - 1) / 2;
        for (let i = 0; i < children.length; i++) {
          // Center bars run taller than the edges, like a real level meter.
          const envelope = 0.35 + 0.65 * (1 - Math.abs(i - mid) / (mid + 1));
          let target = 0.14;
          const p = phaseRef.current;
          if (p === "recording") {
            const wobble = 0.5 + 0.5 * Math.sin(t * 10 + i * 1.9);
            const level = Math.min(1, ampRef.current * 4.5);
            target = 0.14 + level * envelope * (0.35 + 0.65 * wobble);
          } else if (p === "transcribing" || p === "injecting") {
            const sweep = 0.5 + 0.5 * Math.sin(t * 5.5 - i * 0.85);
            target = 0.14 + 0.55 * sweep * envelope;
          } else if (p === "error") {
            target = 0.4;
          }
          const current = heightsRef.current[i] ?? 0.14;
          const next = current + (target - current) * 0.3;
          heightsRef.current[i] = next;
          (children[i] as HTMLElement).style.height = `${
            Math.max(0.1, Math.min(1, next)) * 100
          }%`;
        }
      }
      raf = requestAnimationFrame(tick);
    };

    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);

  return (
    <div
      ref={containerRef}
      className={`flex items-center justify-center gap-[3px] ${className}`}
      aria-hidden="true"
    >
      {Array.from({ length: bars }).map((_, i) => (
        <div
          key={i}
          className={`rounded-full transition-colors ${barClassName}`}
          style={{ height: "14%", minHeight: 3 }}
        />
      ))}
    </div>
  );
}

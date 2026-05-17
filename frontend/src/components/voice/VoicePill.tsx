import { useStore } from "../../state/store";

export function VoicePill() {
  const voiceState = useStore((s) => s.voiceState);

  if (voiceState === "idle") return null;

  const dot = voiceState === "recording" ? "rec" : "idle";
  const label =
    voiceState === "recording"
      ? "listening…"
      : voiceState === "transcribing"
        ? "transcribing…"
        : "";

  return (
    <div className="voice-pill" role="status" aria-live="polite">
      <span className={`voice-pill-dot ${dot}`} aria-hidden />
      <span className="voice-pill-text">
        <span className="voice-pill-muted">{label}</span>
      </span>
    </div>
  );
}

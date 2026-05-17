import { useState } from "react";
import { VoiceSettings } from "./VoiceSettings";
import { DictionaryEditor } from "./DictionaryEditor";
import { VoiceHistory } from "./VoiceHistory";
import { VoiceDashboard } from "./VoiceDashboard";

type VoiceTab = "settings" | "dictionary" | "history" | "stats";

const TABS: { id: VoiceTab; label: string }[] = [
  { id: "settings", label: "Settings" },
  { id: "dictionary", label: "Dictionary" },
  { id: "history", label: "History" },
  { id: "stats", label: "Stats" },
];

export function VoicePanel() {
  const [tab, setTab] = useState<VoiceTab>("settings");

  return (
    <div className="voice-root">
      <div className="right-pane-tabs voice-subtabs">
        {TABS.map((t) => (
          <button
            key={t.id}
            className={`right-pane-tab ${tab === t.id ? "active" : ""}`}
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>
      <div className="voice-body">
        {tab === "settings" ? <VoiceSettings /> : null}
        {tab === "dictionary" ? <DictionaryEditor /> : null}
        {tab === "history" ? <VoiceHistory /> : null}
        {tab === "stats" ? <VoiceDashboard /> : null}
      </div>
    </div>
  );
}

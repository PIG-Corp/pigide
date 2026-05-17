import { useEffect, useState } from "react";
import { ipc } from "../../state/ipc";
import { useStore } from "../../state/store";
import type { VoiceModel } from "../../state/types";

type RecordMode = "ptt" | "toggle";

const DEFAULT_HOTKEY = "AltRight";

export function VoiceSettings() {
  const pushToast = useStore((s) => s.pushToast);

  const [models, setModels] = useState<VoiceModel[]>([]);
  const [currentModel, setCurrentModel] = useState<string>("");
  const [recordMode, setRecordMode] = useState<RecordMode>("ptt");
  const [hotkey, setHotkey] = useState<string>(DEFAULT_HOTKEY);
  const [injectEnabled, setInjectEnabled] = useState<boolean>(true);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [list, modelId, mode, hk, inj] = await Promise.all([
          ipc.voiceListModels(),
          ipc.getSetting("whisper.model_id"),
          ipc.getSetting("voice.record_mode"),
          ipc.getSetting("voice.hotkey"),
          ipc.getSetting("voice.inject_enabled"),
        ]);
        if (cancelled) return;
        setModels(list);
        // Prefer the persisted model id; fall back to the first installed,
        // then the first registered.
        let active = modelId && list.some((m) => m.id === modelId)
          ? modelId
          : null;
        if (!active) {
          active =
            list.find((m) => m.installed)?.id ?? list[0]?.id ?? "";
        }
        setCurrentModel(active);
        if (mode === "toggle" || mode === "ptt") setRecordMode(mode);
        if (hk && hk.length > 0) setHotkey(hk);
        if (inj !== null) setInjectEnabled(inj === "true");
      } catch (err) {
        pushToast({ text: `voice settings load: ${err}`, kind: "error" });
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [pushToast]);

  const onModelChange = async (id: string) => {
    setCurrentModel(id);
    try {
      await ipc.voiceSetModel(id);
    } catch (err) {
      pushToast({ text: `voice_set_model: ${err}`, kind: "error" });
    }
  };

  const onModeChange = async (mode: RecordMode) => {
    setRecordMode(mode);
    try {
      await ipc.setSetting("voice.record_mode", mode);
    } catch (err) {
      pushToast({ text: `set_setting: ${err}`, kind: "error" });
    }
  };

  const onHotkeyChange = async (value: string) => {
    setHotkey(value);
    try {
      await ipc.setSetting("voice.hotkey", value);
    } catch (err) {
      pushToast({ text: `set_setting: ${err}`, kind: "error" });
    }
  };

  const onInjectChange = async (enabled: boolean) => {
    setInjectEnabled(enabled);
    try {
      await ipc.setSetting("voice.inject_enabled", enabled ? "true" : "false");
    } catch (err) {
      pushToast({ text: `set_setting: ${err}`, kind: "error" });
    }
  };

  if (loading) {
    return <div className="voice-panel"><div className="voice-section">Loading…</div></div>;
  }

  return (
    <div className="voice-panel">
      <section className="voice-section">
        <div className="voice-section-title">Model</div>
        <div className="voice-models-list">
          {models.map((m) => (
            <label
              key={m.id}
              className={`voice-model-item ${currentModel === m.id ? "active" : ""}`}
            >
              <input
                type="radio"
                name="voice-model"
                checked={currentModel === m.id}
                onChange={() => onModelChange(m.id)}
              />
              <span className="voice-model-name">{m.filename}</span>
              <span className="voice-model-size">{formatMB(m.approx_bytes)}</span>
              <span className="voice-model-flag">
                {m.installed ? "✓ installed" : "↓ download on use"}
              </span>
            </label>
          ))}
        </div>
      </section>

      <section className="voice-section">
        <div className="voice-section-title">Record mode</div>
        <div className="voice-row">
          <label className="voice-radio">
            <input
              type="radio"
              name="voice-mode"
              checked={recordMode === "ptt"}
              onChange={() => onModeChange("ptt")}
            />
            <span>Push-to-talk (hold)</span>
          </label>
          <label className="voice-radio">
            <input
              type="radio"
              name="voice-mode"
              checked={recordMode === "toggle"}
              onChange={() => onModeChange("toggle")}
            />
            <span>Toggle (press once)</span>
          </label>
        </div>
      </section>

      <section className="voice-section">
        <div className="voice-section-title">Hotkey</div>
        <input
          type="text"
          value={hotkey}
          onChange={(e) => onHotkeyChange(e.target.value)}
          placeholder={DEFAULT_HOTKEY}
        />
        <div className="voice-hint-text">
          Examples: AltRight, ControlLeft+Space, F8
        </div>
      </section>

      <section className="voice-section">
        <div className="voice-section-title">Injection</div>
        <label className="voice-checkbox">
          <input
            type="checkbox"
            checked={injectEnabled}
            onChange={(e) => onInjectChange(e.target.checked)}
          />
          <span>Auto-paste transcript into focused window</span>
        </label>
      </section>
    </div>
  );
}

function formatMB(bytes: number): string {
  return (bytes / 1024 / 1024).toFixed(0) + " MB";
}

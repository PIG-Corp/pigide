import { memo, useCallback, useEffect, useRef, useState } from "react";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import { Clock, Loader, Mic, Send, Trash2, X } from "./icons";
import { MentionTextarea, type MentionTextareaHandle } from "./MentionTextarea";

export function OrchestratorPanel() {
  const chat = useStore((s) => s.chat);
  const status = useStore((s) => s.orchestratorStatus);
  const draft = useStore((s) => s.draftInput);
  const setDraft = useStore((s) => s.setDraftInput);
  const setChat = useStore((s) => s.setChat);
  const voiceState = useStore((s) => s.voiceState);
  const voiceDownload = useStore((s) => s.voiceModelDownload);
  const pushToast = useStore((s) => s.pushToast);
  const queueItems = useStore((s) => s.queueItems);
  const queuePending = useStore((s) => s.queuePending);

  const listRef = useRef<HTMLDivElement | null>(null);
  const taRef = useRef<MentionTextareaHandle | null>(null);

  // Auto-scroll on new messages OR queue activity (so the user always
  // sees the freshly-queued bubble pop in at the bottom).
  useEffect(() => {
    if (!listRef.current) return;
    listRef.current.scrollTop = listRef.current.scrollHeight;
  }, [chat, queueItems]);

  // Auto-grow textarea.
  useEffect(() => {
    const ta = taRef.current?.textarea() ?? null;
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = Math.min(ta.scrollHeight, 160) + "px";
  }, [draft]);

  const send = async () => {
    const text = draft.trim();
    if (!text) return;
    setDraft("");
    try {
      await ipc.sendChat(text);
    } catch (err) {
      // Restore draft on failure (empty / duplicate / dispatch issue) so
      // the user doesn't lose what they typed.
      setDraft(text);
      pushToast({ text: `send_chat: ${err}`, kind: "error" });
    }
  };

  const clear = async () => {
    if (chat.length === 0) return;
    if (!confirm("Clear orchestrator chat history and context?")) return;
    try {
      await ipc.clearChat();
      setChat([]);
    } catch (err) {
      pushToast({ text: `clear_chat: ${err}`, kind: "error" });
    }
  };

  const cancelQueued = useCallback(async (id: string) => {
    try {
      await ipc.cancelChatQueueItem(id);
    } catch (err) {
      pushToast({ text: `cancel_chat_queue_item: ${err}`, kind: "error" });
    }
  }, [pushToast]);

  return (
    <div className="orchestrator-panel">
      <div className="orchestrator-header">
        <span className={`status-dot ${status}`} />
        <span>Orchestrator</span>
        <span className="spacer" />
        <button
          className="btn--icon"
          title="Clear chat & context"
          onClick={clear}
          disabled={chat.length === 0}
        >
          <Trash2 size={13} />
        </button>
        <span className="orchestrator-status-label">
          {status === "idle" ? "ready" : status}
        </span>
      </div>

      <div className="chat-list" ref={listRef}>
        {chat.length === 0 && queueItems.length === 0 ? (
          <div className="empty-state" style={{ padding: 20 }}>
            Start by saying or typing a command, e.g.&nbsp;
            <em>"Создай новый workspace и запусти 4 kiro cli"</em>
            <br />или
            <br />
            <em>"Распредели задачи между 4 киро — 1 плагин, 2 бот, 3 сайт, 4 review"</em>
          </div>
        ) : null}
        {chat.map((m) => (
          <ChatMessageView key={m.id} message={m} />
        ))}
        {queueItems.map((q) => (
          <QueuedMessageView
            key={q.id}
            item={q}
            onCancel={() => cancelQueued(q.id)}
          />
        ))}
      </div>

      <div className="chat-input">
        <MentionTextarea
          ref={taRef}
          placeholder="Type a message — @ to mention agents/tasks, Enter to send"
          value={draft}
          onChange={setDraft}
          onSubmit={send}
          rows={1}
          ariaLabel="Orchestrator message"
        />
        <div className="chat-input-bar">
          <span className="hint">
            {queuePending > 0 ? (
              <span className="queue-badge" title="Messages waiting in queue">
                <Clock size={10} /> +{queuePending} in queue
              </span>
            ) : status === "thinking" ? (
              "thinking…"
            ) : status === "tool" ? (
              "running tool…"
            ) : (
              "Enter ↩ to send"
            )}
          </span>
          <button onClick={send} disabled={!draft.trim()}>
            <Send size={12} /> Send
          </button>
        </div>
      </div>

      <VoiceButton voiceState={voiceState} voiceDownload={voiceDownload} />
    </div>
  );
}

function QueuedMessageView({
  item,
  onCancel,
}: {
  item: import("../state/types").QueueItem;
  onCancel: () => void;
}) {
  const isProcessing = item.status === "processing";
  const cls = `chat-msg user queue-${isProcessing ? "processing" : "queued"}`;
  return (
    <div className={cls}>
      <div className="role queue-role-row">
        {isProcessing ? <Loader size={11} className="spin" /> : <Clock size={11} />}
        <span>{isProcessing ? "user · processing" : "user · queued"}</span>
        <span className="spacer" />
        {!isProcessing ? (
          <button
            className="btn--icon btn--sm"
            title="Cancel this queued message"
            onClick={onCancel}
          >
            <X size={11} />
          </button>
        ) : null}
      </div>
      <div className="body">{item.text}</div>
    </div>
  );
}

const ChatMessageView = memo(function ChatMessageView({
  message,
}: {
  message: import("../state/types").ChatMessage;
}) {
  if (message.role === "assistant" && message.tool_calls?.length) {
    return (
      <div className={`chat-msg ${message.role}`}>
        <div className="role">{message.role}</div>
        {message.content ? <div className="body">{message.content}</div> : null}
        {message.tool_calls.map((tc) => (
          <details key={tc.id} className="chat-msg tool tool-call-details">
            <summary className="tool-call-summary">
              ▸ {tc.function.name}
            </summary>
            <div className="body">
              {tc.function.arguments}
            </div>
          </details>
        ))}
      </div>
    );
  }
  if (message.role === "tool") {
    return (
      <div className="chat-msg tool">
        <div className="role">tool result</div>
        <div className="body">{message.content}</div>
      </div>
    );
  }
  return (
    <div className={`chat-msg ${message.role}`}>
      <div className="role">{message.role}</div>
      <div className="body">{message.content}</div>
    </div>
  );
});

function VoiceButton({
  voiceState,
  voiceDownload,
}: {
  voiceState: import("../state/types").VoiceState;
  voiceDownload: { bytes: number; total: number } | null;
}) {
  const [holding, setHolding] = useState(false);
  // Latch press-and-hold (push-to-talk) so the click handler — which fires
  // *after* mouseup — knows not to also try a toggle transition. Without
  // this, a quick tap would start, stop, and immediately re-cancel.
  const heldRef = useRef(false);

  const start = async () => {
    setHolding(true);
    heldRef.current = true;
    try {
      await ipc.startVoice();
    } catch (err) {
      console.error(err);
      setHolding(false);
      heldRef.current = false;
    }
  };

  const stop = async () => {
    setHolding(false);
    try {
      await ipc.stopVoice();
    } catch (err) {
      console.error(err);
    }
  };

  // Click toggle: idle → record, recording → stop+transcribe, transcribing
  // → cancel (drop the in-flight result).  Push-to-talk users won't reach
  // this path because mousedown/mouseup already handled the cycle and
  // `heldRef` is set.
  const onClick = async () => {
    if (heldRef.current) {
      heldRef.current = false;
      return;
    }
    try {
      if (voiceState === "transcribing") {
        await ipc.cancelVoice();
      } else if (voiceState === "recording") {
        await ipc.stopVoice();
      } else {
        await ipc.startVoice();
      }
    } catch (err) {
      console.error(err);
    }
  };

  const cls = `voice-button ${voiceState === "recording" || holding ? "recording" : ""} ${
    voiceState === "transcribing" ? "transcribing" : ""
  }`;

  const title =
    voiceState === "transcribing"
      ? "Click to cancel transcription"
      : voiceState === "recording"
        ? "Click to stop & transcribe (or release if push-to-talk)"
        : "Click to record · hold for push-to-talk";

  return (
    <div className="voice-button-wrap">
      <button
        className={cls}
        onMouseDown={start}
        onMouseUp={stop}
        onMouseLeave={() => holding && stop()}
        onTouchStart={start}
        onTouchEnd={stop}
        onClick={onClick}
        title={title}
      >
        <Mic size={36} />
      </button>
      <div className="voice-hint">
        {voiceState === "recording"
          ? "RECORDING — release / click to transcribe"
          : voiceState === "transcribing"
            ? "Transcribing… (click to cancel)"
            : voiceDownload && voiceDownload.bytes < voiceDownload.total
              ? `Downloading model: ${formatMB(voiceDownload.bytes)} / ${formatMB(voiceDownload.total)}`
              : "Click or hold to talk"}
      </div>
    </div>
  );
}

function formatMB(bytes: number): string {
  return (bytes / 1024 / 1024).toFixed(1) + " MB";
}

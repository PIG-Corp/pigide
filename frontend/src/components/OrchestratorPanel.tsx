import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import {
  Send,
  Square,
  X,
  RotateCcw,
  Paperclip,
  ListTodo,
  Maximize2,
  Mic,
  Volume2,
  Wrench,
} from "./icons";
import { MentionTextarea, type MentionTextareaHandle } from "./MentionTextarea";
import { Markdown } from "./Markdown";
import type {
  ChatMessage,
  QueueItem,
  Task,
  VoiceState,
  ToolCall,
} from "../state/types";


const SCROLL_BOTTOM_THRESHOLD_PX = 50;
const MAX_VISIBLE_TASKS = 3;
const ICON_SIZE_HEADER = 14;
const ICON_SIZE_ACTION = 14;
const ICON_SIZE_SEND = 12;
const ICON_SIZE_STOP = 10;
const ICON_SIZE_CANCEL = 11;
const ICON_SIZE_TASKS = 14;
const ICON_SIZE_TASKS_EXPAND = 13;

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
  const tasks = useStore((s) => s.tasks);
  const currentId = useStore((s) => s.currentId);
  const chatScope = useStore((s) => s.chatScope);
  const showTaskBoard = useStore((s) => s.showTaskBoard);
  const setShowTaskBoard = useStore((s) => s.setShowTaskBoard);
  const devTrace = useStore((s) => s.devTrace);
  const setDevTrace = useStore((s) => s.setDevTrace);

  const handleScopeChange = useCallback(async (scope: "global" | "workspace") => {
    try {
      await ipc.setChatScope(scope);
    } catch (err) {
      pushToast({ text: `setChatScope: ${err}`, kind: "error" });
    }
  }, [pushToast]);

  const listRef = useRef<HTMLDivElement | null>(null);
  const taRef = useRef<MentionTextareaHandle | null>(null);
  const userScrolledUp = useRef(false);

  // Track whether the user has manually scrolled up, so auto-scroll doesn't
  // yank them back to the bottom while they're reading earlier messages.
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const onScroll = () => {
      const atBottom =
        el.scrollHeight - el.scrollTop - el.clientHeight < SCROLL_BOTTOM_THRESHOLD_PX;
      userScrolledUp.current = !atBottom;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  // Auto-scroll on new messages OR queue activity, but only when not scrolled up.
  useEffect(() => {
    if (!listRef.current || userScrolledUp.current) return;
    listRef.current.scrollTop = listRef.current.scrollHeight;
  }, [chat, queueItems]);

  // Auto-scroll on container resize if the user was not scrolled up.
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => {
      if (!userScrolledUp.current) {
        el.scrollTop = el.scrollHeight;
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);



  const send = useCallback(async () => {
    const text = draft.trim();
    if (!text) return;
    setDraft("");
    userScrolledUp.current = false;
    if (listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
    try {
      await ipc.sendChat(text);
    } catch (err) {
      // Restore draft on failure (empty / duplicate / dispatch issue) so the
      // user doesn't lose what they typed.
      setDraft(text);
      pushToast({ text: `send_chat: ${err}`, kind: "error" });
    } finally {
      taRef.current?.focus();
    }
  }, [draft, setDraft, pushToast]);

  const stop = useCallback(async () => {
    try {
      await ipc.stopChat();
    } catch (err) {
      pushToast({ text: `stop_chat: ${err}`, kind: "error" });
    }
  }, [pushToast]);

  const clear = useCallback(async () => {
    if (chat.length === 0) return;
    if (!confirm("Clear orchestrator chat history and context?")) return;
    try {
      await ipc.clearChat();
      setChat([]);
    } catch (err) {
      pushToast({ text: `clear_chat: ${err}`, kind: "error" });
    }
  }, [chat.length, setChat, pushToast]);

  const cancelQueued = useCallback(
    async (id: string) => {
      try {
        await ipc.cancelChatQueueItem(id);
      } catch (err) {
        pushToast({ text: `cancel_chat_queue_item: ${err}`, kind: "error" });
      }
    },
    [pushToast],
  );

  const toggleTaskBoard = useCallback(() => {
    setShowTaskBoard(!showTaskBoard);
  }, [showTaskBoard, setShowTaskBoard]);

  const workspaceTasks = useMemo(
    () =>
      Object.values(tasks).filter(
        (t) => t.workspace_id === currentId && t.status !== "cancelled",
      ),
    [tasks, currentId],
  );

  const isOrbActive = status !== "idle" || voiceState === "recording";
  const inputPlaceholder =
    status !== "idle"
      ? "PigIDE is busy — your message will queue"
      : "Type a message — @ to mention agents/tasks, Enter to send";

  return (
    <div className="right-pane-bridge">
      <div className="right-pane-bridge__top">
        <div className="orchestrator-header">
          <span className="accent-dot" aria-hidden="true" />
          <span className="orchestrator-title">PigIDE</span>
          <span className="spacer" />
          <button
            type="button"
            className={`btn--icon ${devTrace ? "active" : ""}`}
            title={devTrace ? "Hide raw orchestrator trace" : "Show raw orchestrator trace"}
            onClick={() => setDevTrace(!devTrace)}
            aria-label="Toggle raw orchestrator trace"
            aria-pressed={devTrace}
          >
            <Wrench size={ICON_SIZE_HEADER} />
          </button>
          <button
            type="button"
            className="btn--icon"
            title="Toggle audio"
            aria-label="Toggle audio"
          >
            <Volume2 size={ICON_SIZE_HEADER} />
          </button>
          <button
            type="button"
            className="btn--icon"
            title="Clear chat & context"
            onClick={clear}
            disabled={chat.length === 0}
            aria-label="Clear chat & context"
          >
            <RotateCcw size={ICON_SIZE_HEADER} />
          </button>
        </div>

        <div className="chat-scope-selector">
          <button
            type="button"
            className={`chat-scope-btn ${chatScope === "global" ? "active" : ""}`}
            onClick={() => handleScopeChange("global")}
          >
            Global Chat
          </button>
          <button
            type="button"
            className={`chat-scope-btn ${chatScope === "workspace" ? "active" : ""}`}
            onClick={() => handleScopeChange("workspace")}
            disabled={!currentId}
            title={!currentId ? "Select a workspace to scope chat" : undefined}
          >
            Workspace Chat
          </button>
        </div>

        <BridgeOrb active={isOrbActive} voiceState={voiceState} status={status} />

        <TasksCard
          tasks={workspaceTasks}
          expanded={showTaskBoard}
          onToggleExpand={toggleTaskBoard}
        />

        <div className="transmission-log-divider">
          <span className="transmission-log-text">Transmission log</span>
        </div>
      </div>

      <div className="right-pane-bridge__bottom chat-list" ref={listRef} role="log" aria-label="Transmission log">
        {chat.length === 0 && queueItems.length === 0 ? (
          <div className="chat-empty">
            <div className="chat-empty-title">Transmission log is empty</div>
            <div className="chat-empty-desc">
              Send a message below to start a conversation with PigIDE.
            </div>
          </div>
        ) : null}
        {(() => {
          const visible = devTrace ? chat : chat.filter((m) => m.role !== "tool");
          return (
            <>
              {visible.map((m, idx) => (
                <ChatMessageView
                  key={m.id}
                  message={m}
                  index={idx}
                  chat={chat}
                  devTrace={devTrace}
                />
              ))}
              {queueItems.map((q, idx) => (
                <QueuedMessageView
                  key={q.id}
                  item={q}
                  index={visible.length + idx}
                  onCancel={() => cancelQueued(q.id)}
                />
              ))}
            </>
          );
        })()}
      </div>

      {voiceDownload && voiceDownload.bytes < voiceDownload.total && (
        <VoiceDownloadBar bytes={voiceDownload.bytes} total={voiceDownload.total} />
      )}

      <div className="chat-input">
        <div className="chat-input-wrapper">
          <MentionTextarea
            ref={taRef}
            placeholder={inputPlaceholder}
            value={draft}
            onChange={setDraft}
            onSubmit={send}
            rows={1}
            ariaLabel="Orchestrator message"
            className="chat-input-textarea"
          />
          <div className="chat-input-actions">
            <VoiceIconButton voiceState={voiceState} />
            <button
              type="button"
              className="chat-input-action-btn"
              title="Attach file"
              aria-label="Attach file"
            >
              <Paperclip size={ICON_SIZE_ACTION} />
            </button>
            {status !== "idle" ? (
              <button
                type="button"
                onClick={stop}
                className="chat-input-stop-btn"
                title="Stop orchestrator turn"
                aria-label="Stop orchestrator turn"
              >
                <Square size={ICON_SIZE_STOP} fill="currentColor" stroke="currentColor" />
              </button>
            ) : (
              <button
                type="button"
                onClick={send}
                disabled={!draft.trim()}
                className="chat-input-send-btn"
                title="Send message"
                aria-label="Send message"
              >
                <Send size={ICON_SIZE_SEND} />
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

const BridgeOrb = memo(function BridgeOrb({
  active,
  voiceState,
  status,
}: {
  active: boolean;
  voiceState: VoiceState;
  status: string;
}) {
  const onOrbClick = async () => {
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

  const isRecording = voiceState === "recording";
  const isTranscribing = voiceState === "transcribing";

  const orbTitle = isTranscribing
    ? "Click to cancel transcription"
    : isRecording
      ? "Click to stop & transcribe"
      : "Click to record voice";

  return (
    <div className="bridge-orb-wrap">
      <button
        type="button"
        onClick={onOrbClick}
        className={`bridge-orb ${active ? "bridge-orb--active" : ""} ${
          isRecording ? "bridge-orb--recording" : ""
        } ${isTranscribing ? "bridge-orb--transcribing" : ""}`}
        title={orbTitle}
        aria-label={orbTitle}
        aria-pressed={isRecording}
      >
        <div className="bridge-orb__aura bridge-orb__aura--outer" aria-hidden="true" />
        <div className="bridge-orb__aura bridge-orb__aura--mid" aria-hidden="true" />
        <div className="bridge-orb__aura bridge-orb__aura--inner" aria-hidden="true" />
        <div className="bridge-orb__core">
          <img
            src="/icon.png"
            alt=""
            aria-hidden="true"
            className={`bridge-orb__mark ${isRecording ? "pulse-voice" : ""}`}
          />
        </div>
      </button>
      <div className="bridge-orb__caption">
        {voiceState === "recording" ? (
          <>
            <span className="orb-status-active recording">Recording</span>
            <span className="orb-speaking-sep"> | </span>
            <span className="orb-speaking-right">Listening to user</span>
          </>
        ) : voiceState === "transcribing" ? (
          <>
            <span className="orb-status-active transcribing">Transcribing</span>
            <span className="orb-speaking-sep"> | </span>
            <span className="orb-speaking-right">Processing voice...</span>
          </>
        ) : status === "thinking" ? (
          <>
            <span className="orb-status-active thinking">Thinking</span>
            <span className="orb-speaking-sep"> | </span>
            <span className="orb-speaking-right">Orchestrating swarm</span>
          </>
        ) : status === "tool" ? (
          <>
            <span className="orb-status-active tool">Running Tool</span>
            <span className="orb-speaking-sep"> | </span>
            <span className="orb-speaking-right">Executing actions</span>
          </>
        ) : (
          <>
            <span className="orb-status-idle">PigIDE Idle</span>
            <span className="orb-speaking-sep"> | </span>
            <span className="orb-speaking-right">Awaiting prompt</span>
          </>
        )}
      </div>
    </div>
  );
});

const TasksCard = memo(function TasksCard({
  tasks,
  expanded,
  onToggleExpand,
}: {
  tasks: Task[];
  expanded: boolean;
  onToggleExpand: () => void;
}) {
  const visible = tasks.slice(0, MAX_VISIBLE_TASKS);
  const overflow = tasks.length - visible.length;
  return (
    <div className="tasks-card">
      <div className="tasks-card-header">
        <span className="tasks-card-title-wrap">
          <ListTodo size={ICON_SIZE_TASKS} className="tasks-card-icon" />
          <span className="tasks-card-title">Tasks</span>
        </span>
        <button
          className={`btn--icon btn--sm tasks-card-expand ${expanded ? "active" : ""}`}
          onClick={onToggleExpand}
          title="Toggle Kanban Board"
          aria-label="Toggle Kanban Board"
          aria-pressed={expanded}
        >
          <Maximize2 size={ICON_SIZE_TASKS_EXPAND} />
        </button>
      </div>
      <div className="tasks-card-body">
        {tasks.length === 0 ? (
          <>
            <div className="tasks-empty-title">No tasks yet</div>
            <div className="tasks-empty-desc">
              Ask PigIDE to plan something — "help me wire up the auth refractor",
              "make a list of the bugs in the swarm runner", "let's ship the
              sidebar" — and the steps will show up here as PigIDE works.
            </div>
          </>
        ) : (
          <div className="tasks-mini-list">
            {visible.map((t) => (
              <div key={t.id} className="tasks-mini-item">
                <span
                  className={`tasks-mini-status-dot ${t.status}`}
                  aria-hidden="true"
                />
                <span className="tasks-mini-text">{t.title}</span>
              </div>
            ))}
            {overflow > 0 && (
              <div className="tasks-mini-more">
                +{overflow} more tasks (click expand to view board)
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
});

function VoiceDownloadBar({ bytes, total }: { bytes: number; total: number }) {
  const pct = (bytes / total) * 100;
  return (
    <div
      className="voice-download-bar"
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(pct)}
      aria-label="Voice model download progress"
    >
      <div className="voice-download-progress" style={{ width: `${pct}%` }} />
      <span className="voice-download-text">
        Downloading model: {pct.toFixed(0)}%
      </span>
    </div>
  );
}

function QueuedMessageView({
  item,
  index,
  onCancel,
}: {
  item: QueueItem;
  index: number;
  onCancel: () => void;
}) {
  const isProcessing = item.status === "processing";
  const displayNum = String(index + 1).padStart(2, "0");
  const variant = isProcessing ? "processing" : "queued";
  return (
    <div className={`chat-log-row user queue-${variant}`}>
      <div className="chat-log-num">{displayNum}</div>
      <div className={`chat-log-line user ${variant}`} />
      <div className="chat-log-content">
        <div className="chat-log-sender user">
          <span>{isProcessing ? "You · processing" : "You · queued"}</span>
          {!isProcessing ? (
            <button
              className="btn--icon btn--sm chat-log-cancel-btn"
              title="Cancel this queued message"
              onClick={onCancel}
              aria-label="Cancel queued message"
            >
              <X size={ICON_SIZE_CANCEL} />
            </button>
          ) : null}
        </div>
        <div className="chat-log-body">{item.text}</div>
      </div>
    </div>
  );
}

const ToolCallView = memo(function ToolCallView({
  tc,
  chat,
}: {
  tc: ToolCall;
  chat: ChatMessage[];
}) {
  const isRunning = useMemo(
    () => !chat.some((m) => m.role === "tool" && m.tool_call_id === tc.id),
    [chat, tc.id]
  );
  const [isOpen, setIsOpen] = useState(isRunning);

  const [prevIsRunning, setPrevIsRunning] = useState(isRunning);
  if (isRunning !== prevIsRunning) {
    setPrevIsRunning(isRunning);
    if (isRunning) {
      setIsOpen(true);
    }
  }

  const formattedArgs = useMemo(() => {
    try {
      return JSON.stringify(JSON.parse(tc.function.arguments), null, 2);
    } catch {
      return tc.function.arguments;
    }
  }, [tc.function.arguments]);

  return (
    <details
      open={isOpen}
      onToggle={(e) => setIsOpen(e.currentTarget.open)}
      className="chat-log-tool-details"
    >
      <summary className="chat-log-tool-summary" style={{ display: "flex", alignItems: "center" }}>
        <span className="chat-log-tool-arrow">{isOpen ? "▾" : "▸"}</span>
        <span className="chat-log-tool-name">{tc.function.name}</span>
        {isRunning && (
          <span className="chat-log-tool-status running">
            <span className="spinner-dot" />
            running
          </span>
        )}
      </summary>
      <pre className="chat-log-tool-body">{formattedArgs}</pre>
    </details>
  );
});

const ToolResultView = memo(function ToolResultView({
  content,
}: {
  content: string;
}) {
  const [isExpanded, setIsExpanded] = useState(false);
  const isLong = content.length > 1200;

  const displayContent = useMemo(() => {
    if (!isLong || isExpanded) {
      try {
        return JSON.stringify(JSON.parse(content), null, 2);
      } catch {
        return content;
      }
    }
    const truncated = content.slice(0, 1000);
    return `${truncated}\n\n... [Output truncated. Total size: ${(content.length / 1024).toFixed(2)} KB]`;
  }, [content, isLong, isExpanded]);

  return (
    <div className="chat-log-tool-result-wrapper">
      <pre className="chat-log-body tool">{displayContent}</pre>
      {isLong && (
        <button
          type="button"
          className="tool-expand-btn"
          onClick={() => setIsExpanded(!isExpanded)}
        >
          {isExpanded ? "Show less" : "Show full output"}
        </button>
      )}
    </div>
  );
});

/// Strip the noisy `Calling tools:` / `вызываю тулзы:` preamble that some
/// model turns include before (or instead of) emitting real tool_use
/// blocks. Mirrors `phantom::strip_calling_tools_preamble` on the Rust side
/// so user-visible text stays clean even when an older message persisted
/// the preamble in its `content` column.
function stripCallingToolsPreamble(text: string): string {
  if (!text) return text;
  const markers = [
    /\n?Calling tools:[\s\S]*$/i,
    /\n?Вызываю тулзы:[\s\S]*$/i,
    /\n?tool_use:[\s\S]*$/i,
  ];
  let out = text;
  for (const re of markers) {
    out = out.replace(re, "");
  }
  return out.trimEnd();
}

const ChatMessageView = memo(function ChatMessageView({
  message,
  index,
  chat,
  devTrace,
}: {
  message: ChatMessage;
  index: number;
  chat: ChatMessage[];
  devTrace: boolean;
}) {
  const displayNum = String(index + 1).padStart(2, "0");

  if (message.role === "assistant" && message.tool_calls?.length) {
    const cleaned = stripCallingToolsPreamble(message.content);
    const names = message.tool_calls.map((tc) => tc.function.name);
    const uniqueNames = Array.from(new Set(names));
    const isRunning = message.tool_calls.some(
      (tc) => !chat.some((m) => m.role === "tool" && m.tool_call_id === tc.id),
    );
    return (
      <div className="chat-log-row assistant">
        <div className="chat-log-num">{displayNum}</div>
        <div className="chat-log-line assistant" />
        <div className="chat-log-content">
          <div className="chat-log-sender assistant">PigIDE</div>
          {cleaned ? (
            <div className="chat-log-body">
              <Markdown content={cleaned} />
            </div>
          ) : null}
          {devTrace ? (
            message.tool_calls.map((tc) => (
              <ToolCallView key={tc.id} tc={tc} chat={chat} />
            ))
          ) : (
            <div className="chat-log-tool-summary-line" aria-live="polite">
              {isRunning ? <span className="spinner-dot" /> : null}
              <span className="chat-log-tool-summary-text">
                {isRunning ? "Working" : "Ran"}{" "}
                {names.length === 1
                  ? "1 action"
                  : `${names.length} actions`}
                {uniqueNames.length <= 3
                  ? ` (${uniqueNames.join(", ")})`
                  : ""}
              </span>
            </div>
          )}
        </div>
      </div>
    );
  }

  if (message.role === "tool") {
    // Hidden entirely when devTrace=false (we filter upstream); this branch
    // only renders in dev mode.
    return (
      <div className="chat-log-row tool">
        <div className="chat-log-num">{displayNum}</div>
        <div className="chat-log-line tool" />
        <div className="chat-log-content">
          <div className="chat-log-sender tool">tool result</div>
          <ToolResultView content={message.content} />
        </div>
      </div>
    );
  }

  return (
    <div className={`chat-log-row ${message.role}`}>
      <div className="chat-log-num">{displayNum}</div>
      <div className={`chat-log-line ${message.role}`} />
      <div className="chat-log-content">
        <div className={`chat-log-sender ${message.role}`}>
          {message.role === "user" ? "You" : message.role === "system" ? "System" : "PigIDE"}
        </div>
        <div className={`chat-log-body ${message.role}`}>
          {message.role === "system" ? (
            message.content
          ) : (
            <Markdown
              content={
                message.role === "assistant"
                  ? stripCallingToolsPreamble(message.content)
                  : message.content
              }
            />
          )}
        </div>
      </div>
    </div>
  );
});

function VoiceIconButton({ voiceState }: { voiceState: VoiceState }) {
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
    heldRef.current = false;
    try {
      await ipc.stopVoice();
    } catch (err) {
      console.error(err);
    }
  };

  // Click toggle: idle → record, recording → stop+transcribe, transcribing
  // → cancel (drop the in-flight result). Push-to-talk users won't reach
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

  const isRecording = voiceState === "recording" || holding;
  const isTranscribing = voiceState === "transcribing";

  const cls = `chat-input-action-btn voice ${isRecording ? "recording" : ""} ${
    isTranscribing ? "transcribing" : ""
  }`;

  const title =
    voiceState === "transcribing"
      ? "Click to cancel transcription"
      : voiceState === "recording"
        ? "Click to stop & transcribe (or release if push-to-talk)"
        : "Click to record · hold for push-to-talk";

  return (
    <button
      type="button"
      className={cls}
      onMouseDown={start}
      onMouseUp={stop}
      onMouseLeave={() => {
        if (holding) void stop();
      }}
      onTouchStart={start}
      onTouchEnd={stop}
      onTouchCancel={() => {
        if (holding) void stop();
      }}
      onClick={onClick}
      title={title}
      aria-label="Record voice"
      aria-pressed={isRecording}
    >
      <Mic
        size={ICON_SIZE_ACTION}
        className={isRecording ? "pulse-voice" : ""}
      />
    </button>
  );
}

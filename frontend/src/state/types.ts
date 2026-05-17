// Types mirroring the Rust side. Kept in sync manually.

export type SplitDir = "h" | "v";

export type LayoutNode =
  | { type: "empty" }
  | { type: "leaf"; agentId: string }
  | { type: "split"; direction: SplitDir; ratio: number; a: LayoutNode; b: LayoutNode };

export interface Workspace {
  id: string;
  name: string;
  created_at: string;
  layout: LayoutNode;
  paths: string[];
  agent_count: number;
}

export interface Agent {
  id: string;
  workspace_id: string;
  agent_type: string;
  cwd: string | null;
  status: string;
  created_at: string;
}

export interface ToolCall {
  id: string;
  type: string;
  function: { name: string; arguments: string };
}

export interface ChatMessage {
  id: string;
  role: "user" | "assistant" | "tool" | "system";
  content: string;
  tool_calls?: ToolCall[];
  tool_call_id?: string;
  created_at: string;
}

export type OrchestratorStatus = "idle" | "thinking" | "tool";
export type VoiceState = "idle" | "recording" | "transcribing";

// ---------- Chat queue ----------

export type QueueStatus = "queued" | "processing" | "failed" | "cancelled";

export interface QueueItem {
  id: string;
  session_id: string;
  text: string;
  status: QueueStatus | string;
  position: number;
  created_at: string;
}

export interface QueueSnapshot {
  session_id: string;
  items: QueueItem[];
  pending: number;
}

export type TaskStatus =
  | "todo"
  | "in_progress"
  | "in_review"
  | "complete"
  | "cancelled";

export interface Task {
  id: string;
  workspace_id: string;
  agent_id: string | null;
  parent_id: string | null;
  title: string;
  instructions: string;
  knowledge: string;
  status: TaskStatus;
  created_at: string;
  updated_at: string;
}

export interface Note {
  id: string;
  slug: string;
  title: string;
  tags: string[];
  aliases: string[];
  body: string;
  created_at: string;
  updated_at: string;
}

export interface NoteSummary {
  id: string;
  slug: string;
  title: string;
  tags: string[];
  updated_at: string;
}

export interface SearchHit {
  id: string;
  slug: string;
  title: string;
  snippet: string;
  score: number;
}

export interface Backlink {
  src_id: string;
  src_slug: string;
  src_title: string;
  context: string;
}

export interface GraphNode {
  id: string;
  slug: string;
  title: string;
  tags: string[];
}

export interface GraphEdge {
  source: string;
  target: string | null;
  target_text: string;
  ambiguous: boolean;
}

export interface GraphData {
  nodes: GraphNode[];
  links: GraphEdge[];
}

export interface VoiceModel {
  id: string;
  filename: string;
  approx_bytes: number;
  installed: boolean;
  url: string;
}

export interface DictEntry {
  id: string;
  pattern: string;
  replacement: string;
  case_sense: boolean;
  enabled: boolean;
  created_at: string;
}

export interface Transcript {
  id: string;
  text: string;
  text_raw: string;
  language: string | null;
  model_id: string;
  source: string;
  duration_ms: number;
  word_count: number;
  created_at: string;
  injected: boolean;
}

export interface VoiceStats {
  sessions: number;
  total_words: number;
  talk_seconds: number;
  avg_wpm: number;
}

export type VoiceStatsRange = "day" | "week" | "month" | "all";

export interface RoomTemplate {
  id: string;
  name: string;
  description: string;
  agents: { agent_type: string; role: string; count: number }[];
  tasks: { title: string; instructions: string }[];
}

export interface RoomApplyResult {
  spawned_agents: string[];
  created_tasks: string[];
}

export interface DirEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
}

// ---------- Prompts library (#18) ----------

export interface Prompt {
  id: string;
  workspace_id: string | null;
  name: string;
  body: string;
  tags: string[];
  created_at: string;
  updated_at: string;
}

// ---------- Role-prompt overrides (#19) ----------

export type SwarmRole = "coordinator" | "builder" | "reviewer" | "scout";

export interface RolePromptOverride {
  workspace_id: string;
  /** Empty string = all agent types in this workspace. */
  agent_type: string;
  role: SwarmRole;
  prompt: string;
  updated_at: string;
}

// ---------- SSH presets (#14) ----------

export interface SshPreset {
  id: string;
  name: string;
  host: string;
  user: string | null;
  port: number | null;
  identity: string | null;
  args: string[];
  cwd: string | null;
  created_at: string;
  updated_at: string;
}

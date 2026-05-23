import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  Agent,
  AliasSummary,
  Backlink,
  ChatMessage,
  DictEntry,
  DirEntry,
  GraphData,
  LayoutNode,
  MemoryStatus,
  Note,
  NoteSummary,
  PathAttachment,
  PathSuggestion,
  Prompt,
  QueueItem,
  QueueSnapshot,
  RolePromptOverride,
  RoomApplyResult,
  RoomTemplate,
  SearchHit,
  SshPreset,
  SwarmRole,
  TagSummary,
  Task,
  TaskStatus,
  Transcript,
  VoiceModel,
  VoiceStats,
  VoiceStatsRange,
  Workspace,
} from "./types";

// ---------- Commands ----------

export const ipc = {
  ping: () => invoke<string>("ping"),

  listWorkspaces: () => invoke<Workspace[]>("list_workspaces"),
  getWorkspace: (id: string) => invoke<Workspace>("get_workspace", { id }),
  createWorkspace: (name: string, paths: string[] = []) =>
    invoke<Workspace>("create_workspace", { args: { name, paths } }),
  renameWorkspace: (id: string, name: string) =>
    invoke<void>("rename_workspace", { id, name }),
  deleteWorkspace: (id: string) => invoke<void>("delete_workspace", { id }),
  updateLayout: (workspaceId: string, layout: LayoutNode) =>
    invoke<void>("update_layout", { workspaceId, layout }),
  setCurrentWorkspace: (id: string) =>
    invoke<void>("set_current_workspace", { id }),
  getCurrentWorkspace: () => invoke<string | null>("get_current_workspace"),

  spawnAgent: (
    workspaceId: string,
    agentType: "kiro-cli" | "claude" | "opencode" | "devin" | "agy" | "codex",
    opts: { cwd?: string | null; count?: number; autoLayout?: boolean } = {},
  ) =>
    invoke<Agent[]>("spawn_agent", {
      args: {
        workspace_id: workspaceId,
        agent_type: agentType,
        cwd: opts.cwd ?? null,
        count: opts.count ?? 1,
        auto_layout: opts.autoLayout ?? true,
      },
    }),
  killAgent: (agentId: string) => invoke<void>("kill_agent", { agentId }),
  writeToAgent: (agentId: string, dataB64: string) =>
    invoke<void>("write_to_agent", {
      args: { agent_id: agentId, data_b64: dataB64 },
    }),
  resizeAgent: (agentId: string, cols: number, rows: number) =>
    invoke<void>("resize_agent", {
      args: { agent_id: agentId, cols, rows },
    }),
  listAgents: (workspaceId: string) =>
    invoke<Agent[]>("list_agents", { workspaceId }),
  agentLogTail: (agentId: string, maxBytes?: number) =>
    invoke<string>("agent_log_tail", {
      agentId,
      maxBytes: maxBytes ?? null,
    }),
  restoreSession: () =>
    invoke<[number, number]>("restore_session"),

  listChat: () => invoke<ChatMessage[]>("list_chat"),
  sendChat: (text: string, attachments: PathAttachment[] = []) =>
    invoke<QueueItem>("send_chat", { args: { text, attachments } }),
  suggestPaths: (query: string, workspaceId?: string | null) =>
    invoke<PathSuggestion[]>("suggest_paths", {
      args: { query, workspace_id: workspaceId ?? null },
    }),
  clearChat: () => invoke<void>("clear_chat"),
  stopChat: () => invoke<boolean>("stop_chat"),
  listChatQueue: () => invoke<QueueItem[]>("list_chat_queue"),
  cancelChatQueueItem: (id: string) =>
    invoke<boolean>("cancel_chat_queue_item", { id }),
  setChatScope: (scope: "global" | "workspace") =>
    invoke<any>("set_chat_scope", { args: { scope } }),
  getCurrentSession: () =>
    invoke<any>("get_current_session"),
  setChatSession: (id: string) =>
    invoke<any>("set_current_session", { id }),
  createChatSession: (name: string, scope: "global" | "workspace", workspaceId?: string | null) =>
    invoke<any>("create_chat_session", { args: { name, scope, workspace_id: workspaceId } }),
  listChatSessions: (args?: { scope?: "global" | "workspace" | "all"; workspace_id?: string | null }) =>
    invoke<any[]>("list_chat_sessions", { args }),
  chatQueueSetContinueOnError: (continueOnError: boolean) =>
    invoke<void>("chat_queue_set_continue_on_error", {
      args: { continue_on_error: continueOnError },
    }),
  chatQueueGetContinueOnError: () =>
    invoke<boolean>("chat_queue_get_continue_on_error"),

  startVoice: () => invoke<void>("start_voice"),
  stopVoice: () => invoke<void>("stop_voice"),
  cancelVoice: () => invoke<void>("cancel_voice"),

  setSetting: (key: string, value: string) =>
    invoke<void>("set_setting", { args: { key, value } }),
  getSetting: (key: string) => invoke<string | null>("get_setting", { key }),

  // ---------- Architect provider ----------
  providerInfo: () =>
    invoke<{
      provider: string;
      primary_model: string;
      fallback_model: string | null;
      has_api_key: boolean;
    }>("provider_info"),
  providerTestConnection: () =>
    invoke<{
      provider: string;
      model: string;
      ok: boolean;
      note: string | null;
    }>("provider_test_connection"),

  createTask: (args: {
    workspace_id: string;
    title: string;
    instructions?: string;
    knowledge?: string;
    parent_id?: string | null;
  }) =>
    invoke<Task>("create_task", {
      args: {
        workspace_id: args.workspace_id,
        title: args.title,
        instructions: args.instructions ?? "",
        knowledge: args.knowledge ?? "",
        parent_id: args.parent_id ?? null,
      },
    }),
  getTask: (id: string) => invoke<Task>("get_task", { id }),
  listTasks: (filter?: {
    workspace_id?: string;
    status?: TaskStatus;
    agent_id?: string;
  }) => invoke<Task[]>("list_tasks", { args: filter ?? null }),
  updateTask: (args: {
    id: string;
    title?: string;
    instructions?: string;
    knowledge?: string;
    status?: TaskStatus;
  }) => invoke<Task>("update_task", { args }),
  deleteTask: (id: string) => invoke<void>("delete_task", { id }),
  assignTask: (taskId: string, agentId: string | null) =>
    invoke<Task>("assign_task", {
      args: { task_id: taskId, agent_id: agentId },
    }),

  createMemory: (args: {
    workspace_id: string;
    title: string;
    body?: string;
    tags?: string[];
    aliases?: string[];
    slug?: string;
  }) =>
    invoke<Note>("create_memory", {
      args: {
        workspace_id: args.workspace_id,
        title: args.title,
        body: args.body ?? "",
        tags: args.tags ?? [],
        aliases: args.aliases ?? [],
        slug: args.slug,
      },
    }),
  readMemory: (id: string) => invoke<Note>("read_memory", { id }),
  updateMemory: (args: {
    id: string;
    title?: string;
    body?: string;
    tags?: string[];
    aliases?: string[];
  }) => invoke<Note>("update_memory", { args }),
  deleteMemory: (id: string) => invoke<void>("delete_memory", { id }),
  listMemories: (args: {
    workspace_id: string;
    tag?: string;
    limit?: number;
  }) => invoke<NoteSummary[]>("list_memories", { args }),
  searchMemories: (args: {
    workspace_id: string;
    query: string;
    limit?: number;
  }) => invoke<SearchHit[]>("search_memories", { args }),
  findBacklinks: (id: string) =>
    invoke<Backlink[]>("find_backlinks", { id }),
  suggestConnections: (id: string, limit?: number) =>
    invoke<SearchHit[]>("suggest_connections", { id, limit: limit ?? 5 }),
  memoryGraph: (workspaceId: string) =>
    invoke<GraphData>("memory_graph", { workspaceId }),
  memoryTags: (workspaceId: string) =>
    invoke<TagSummary[]>("memory_tags", { workspaceId }),
  memoryAliases: (workspaceId: string) =>
    invoke<AliasSummary[]>("memory_aliases", { workspaceId }),
  memoryStatus: (workspaceId: string) =>
    invoke<MemoryStatus>("memory_status", { workspaceId }),
  memoryReindex: (workspaceId: string) =>
    invoke<MemoryStatus>("memory_reindex", { workspaceId }),

  voiceListModels: () => invoke<VoiceModel[]>("voice_list_models"),
  voiceSetModel: (modelId: string) =>
    invoke<void>("voice_set_model", { args: { model_id: modelId } }),
  voiceDictList: () => invoke<DictEntry[]>("voice_dict_list"),
  voiceDictAdd: (args: {
    pattern: string;
    replacement: string;
    case_sense?: boolean;
  }) =>
    invoke<DictEntry>("voice_dict_add", {
      args: {
        pattern: args.pattern,
        replacement: args.replacement,
        case_sense: args.case_sense ?? false,
      },
    }),
  voiceDictUpdate: (args: {
    id: string;
    pattern?: string;
    replacement?: string;
    case_sense?: boolean;
    enabled?: boolean;
  }) => invoke<void>("voice_dict_update", { args }),
  voiceDictDelete: (id: string) => invoke<void>("voice_dict_delete", { id }),
  voiceHistoryList: (limit?: number) =>
    invoke<Transcript[]>("voice_history_list", { limit: limit ?? null }),
  voiceHistorySearch: (query: string, limit?: number) =>
    invoke<Transcript[]>("voice_history_search", {
      args: { query, limit: limit ?? null },
    }),
  voiceHistoryDelete: (id: string) =>
    invoke<void>("voice_history_delete", { id }),
  voiceStats: (range?: VoiceStatsRange) =>
    invoke<VoiceStats>("voice_stats", { range: range ?? null }),

  listRoomTemplates: () => invoke<RoomTemplate[]>("list_room_templates"),
  applyRoomTemplate: (workspaceId: string, templateId: string) =>
    invoke<RoomApplyResult>("apply_room_template", {
      args: { workspace_id: workspaceId, template_id: templateId },
    }),

  listDir: (path: string) => invoke<DirEntry[]>("list_dir", { path }),
  browseDir: (path: string) => invoke<DirEntry[]>("browse_dir", { path }),
  homeDir: () => invoke<string>("home_dir"),
  readFile: (path: string) => invoke<string>("read_file", { path }),
  writeFile: (path: string, content: string) =>
    invoke<void>("write_file", { args: { path, content } }),
  walkFiles: (root: string, maxFiles?: number) =>
    invoke<DirEntry[]>("walk_files", {
      args: { root, max_files: maxFiles ?? 2000 },
    }),

  // ---------- Prompts library (#18) ----------
  listPrompts: (filter?: { workspace_id?: string; tag?: string }) =>
    invoke<Prompt[]>("list_prompts", { args: filter ?? null }),
  getPrompt: (id: string) => invoke<Prompt>("get_prompt", { id }),
  createPrompt: (args: {
    workspace_id?: string | null;
    name: string;
    body: string;
    tags?: string[];
  }) =>
    invoke<Prompt>("create_prompt", {
      args: {
        workspace_id: args.workspace_id ?? null,
        name: args.name,
        body: args.body,
        tags: args.tags ?? [],
      },
    }),
  updatePrompt: (args: {
    id: string;
    name?: string;
    body?: string;
    tags?: string[];
  }) => invoke<Prompt>("update_prompt", { args }),
  deletePrompt: (id: string) => invoke<void>("delete_prompt", { id }),

  // ---------- Role-prompt overrides (#19) ----------
  listRolePrompts: (workspaceId: string) =>
    invoke<RolePromptOverride[]>("list_role_prompts", {
      workspaceId,
    }),
  upsertRolePrompt: (args: {
    workspace_id: string;
    agent_type?: string;
    role: SwarmRole;
    prompt: string;
  }) =>
    invoke<RolePromptOverride>("upsert_role_prompt", {
      args: {
        workspace_id: args.workspace_id,
        agent_type: args.agent_type ?? "",
        role: args.role,
        prompt: args.prompt,
      },
    }),
  deleteRolePrompt: (args: {
    workspace_id: string;
    agent_type?: string;
    role: SwarmRole;
  }) =>
    invoke<boolean>("delete_role_prompt", {
      args: {
        workspace_id: args.workspace_id,
        agent_type: args.agent_type ?? "",
        role: args.role,
      },
    }),
  resolveRolePrompt: (args: {
    workspace_id: string;
    agent_type?: string;
    role: SwarmRole;
  }) =>
    invoke<string>("resolve_role_prompt", {
      args: {
        workspace_id: args.workspace_id,
        agent_type: args.agent_type ?? "",
        role: args.role,
      },
    }),

  // ---------- SSH presets (#14) ----------
  listSshPresets: () => invoke<SshPreset[]>("list_ssh_presets"),
  createSshPreset: (args: {
    name: string;
    host: string;
    user?: string | null;
    port?: number | null;
    identity?: string | null;
    args?: string[];
    cwd?: string | null;
  }) =>
    invoke<SshPreset>("create_ssh_preset", {
      args: {
        name: args.name,
        host: args.host,
        user: args.user ?? null,
        port: args.port ?? null,
        identity: args.identity ?? null,
        args: args.args ?? [],
        cwd: args.cwd ?? null,
      },
    }),
  deleteSshPreset: (id: string) => invoke<boolean>("delete_ssh_preset", { id }),
  spawnSsh: (workspaceId: string, presetId: string) =>
    invoke<Agent>("spawn_ssh", {
      args: { workspace_id: workspaceId, preset_id: presetId },
    }),

  // ---------- Skills ----------
  listSkills: () => invoke<SkillView[]>("list_skills"),
  getSkill: (id: string) => invoke<SkillFull | null>("get_skill", { id }),
  setSkillEnabled: (id: string, enabled: boolean) =>
    invoke<void>("set_skill_enabled", { args: { id, enabled } }),
  reloadSkills: () => invoke<number>("reload_skills"),
  lastSkillsTrace: (sessionId?: string) =>
    invoke<SkillsTraceRow | null>("last_skills_trace", {
      sessionId: sessionId ?? null,
    }),
  createUserSkill: (id: string, name: string) =>
    invoke<string>("create_user_skill", { args: { id, name } }),
  importClaudeSkills: (extraPaths?: string[]) =>
    invoke<ClaudeImportReport>("import_claude_skills", {
      args: { extra_paths: extraPaths ?? [] },
    }),
  listClaudeSkillSources: () =>
    invoke<ClaudeSourceRoot[]>("list_claude_skill_sources"),
};

export interface ClaudeSourceRoot {
  label: string;
  path: string;
  exists: boolean;
  skill_count: number;
}

export interface ClaudeImportedSkill {
  id: string;
  name: string;
  source_path: string;
  written_to: string;
  status: "created" | "updated" | "unchanged" | "skipped" | "failed";
  warnings: string[];
}

export interface ClaudeImportReport {
  roots: ClaudeSourceRoot[];
  imported: ClaudeImportedSkill[];
  destination: string;
  created: number;
  updated: number;
  unchanged: number;
  skipped: number;
  failed: number;
}

export interface SkillView {
  id: string;
  name: string;
  description: string;
  source: string;
  path: string;
  priority: number;
  tags: string[];
  triggers: string[];
  enabled: boolean;
  override_disabled: boolean;
  shadowed_by: string | null;
  digest: string;
}

export interface SkillFull extends SkillView {
  body: string;
}

export interface SkillsTraceSelection {
  id: string;
  score: number;
  reasons: string[];
}

export interface SkillsTraceRow {
  id: string;
  session_id: string;
  turn_at: string;
  selected: SkillsTraceSelection[];
  rejected: SkillsTraceSelection[];
  composed_chars: number;
  fallback_used: boolean;
}

export function onSkillsReloaded(
  cb: (e: { path: string }) => void,
): Promise<UnlistenFn> {
  return listen<{ path: string }>("skills://reloaded", (e) => cb(e.payload));
}
export function onSkillsError(
  cb: (e: { path: string; error: string }) => void,
): Promise<UnlistenFn> {
  return listen<{ path: string; error: string }>("skills://error", (e) =>
    cb(e.payload),
  );
};

// ---------- Event listeners ----------

export type AgentSpawnedEvent = Agent;
export type LayoutChangedEvent = { workspace_id: string; layout: import("./types").LayoutNode };
export type WorkspaceChangedEvent = { current_workspace_id?: string; deleted?: string };
export type AgentStdoutEvent = { agent_id: string; data_b64: string };
export type AgentExitEvent = { agent_id: string; code?: number };
export type ChatChunkEvent = { id: string; delta: string };
export type ChatStatusEvent = { state: "idle" | "thinking" | "tool" };
export type VoiceStateEvent = { state: "idle" | "recording" | "transcribing" };
export type VoiceTranscriptEvent = { text: string };
export type VoiceDownloadEvent = { bytes: number; total: number };

export function onAgentSpawned(cb: (a: AgentSpawnedEvent) => void): Promise<UnlistenFn> {
  return listen<AgentSpawnedEvent>("agent://spawned", (e) => cb(e.payload));
}
export function onLayoutChanged(
  cb: (e: LayoutChangedEvent) => void,
): Promise<UnlistenFn> {
  return listen<LayoutChangedEvent>("workspace://layout", (e) => cb(e.payload));
}
export function onWorkspaceChanged(
  cb: (e: WorkspaceChangedEvent) => void,
): Promise<UnlistenFn> {
  return listen<WorkspaceChangedEvent>("workspace://changed", (e) => cb(e.payload));
}
export function onAgentStdout(
  cb: (e: AgentStdoutEvent) => void,
): Promise<UnlistenFn> {
  return listen<AgentStdoutEvent>("agent://stdout", (e) => cb(e.payload));
}
export function onAgentExit(cb: (e: AgentExitEvent) => void): Promise<UnlistenFn> {
  return listen<AgentExitEvent>("agent://exit", (e) => cb(e.payload));
}
export function onChatMessage(cb: (m: ChatMessage) => void): Promise<UnlistenFn> {
  return listen<ChatMessage>("chat://message", (e) => cb(e.payload));
}
export function onChatChunk(cb: (e: ChatChunkEvent) => void): Promise<UnlistenFn> {
  return listen<ChatChunkEvent>("chat://chunk", (e) => cb(e.payload));
}
export function onChatStatus(cb: (e: ChatStatusEvent) => void): Promise<UnlistenFn> {
  return listen<ChatStatusEvent>("chat://status", (e) => cb(e.payload));
}
export function onChatQueue(cb: (e: QueueSnapshot) => void): Promise<UnlistenFn> {
  return listen<QueueSnapshot>("chat://queue", (e) => cb(e.payload));
}
export function onVoiceState(cb: (e: VoiceStateEvent) => void): Promise<UnlistenFn> {
  return listen<VoiceStateEvent>("voice://state", (e) => cb(e.payload));
}
export function onVoiceTranscript(
  cb: (e: VoiceTranscriptEvent) => void,
): Promise<UnlistenFn> {
  return listen<VoiceTranscriptEvent>("voice://transcript", (e) => cb(e.payload));
}
export function onVoiceDownload(
  cb: (e: { bytes: number; total: number }) => void,
): Promise<UnlistenFn> {
  return listen<{ bytes: number; total: number }>("voice://download", (e) => cb(e.payload));
}
export type ChatScopeEvent = { scope: "global" | "workspace"; session_id: string; workspace_id: string | null };
export function onChatScopeChanged(cb: (e: ChatScopeEvent) => void): Promise<UnlistenFn> {
  return listen<ChatScopeEvent>("chat://scope", (e) => cb(e.payload));
}

// ---------- Deep links (#17) ----------
export type DeepLinkRoute =
  | { kind: "workspace"; id: string }
  | { kind: "agent_spawn"; agent_type: string; workspace_id: string | null; cwd: string | null }
  | { kind: "task"; id: string }
  | { kind: "memory"; slug: string }
  | { kind: "chat"; text: string }
  | { kind: "unknown"; url: string };

export type DeepLinkEvent = { url: string; route: DeepLinkRoute };

export function onDeepLink(cb: (e: DeepLinkEvent) => void): Promise<UnlistenFn> {
  return listen<DeepLinkEvent>("deep-link://nav", (e) => cb(e.payload));
}


# ARCHITECT_MODEL_PLAN — Kiro-as-Architect on Claude Opus

**Date:** 2026-05-15
**Owner:** Architect-Model agent (Opus 4.7 backend in CC; targets Opus 4.5 / Opus 4 in product)
**Scope:** Wire PIG IDE's Kiro orchestrator to Anthropic Messages API as the default Architect backend; keep the existing OmniRouter provider as an alternative.

---

## 1. Goal

Make the Kiro orchestrator pluggable across multiple LLM providers, with **Claude Opus 4.5** as the default Architect model and **Claude Opus 4** as automatic fallback. Streaming, tool calling, prompt caching, settings UX, secure key storage.

Model IDs (per King Prompt):

| Slot     | Model ID            | Notes                          |
|----------|---------------------|--------------------------------|
| default  | `claude-opus-4-5`   | Architect primary              |
| fallback | `claude-opus-4`     | On 5xx / 529 / timeout         |

> Note: Anthropic's actual production IDs evolve; the user-supplied IDs are stored verbatim in settings so they can be updated without code changes.

---

## 2. Existing surface

- `src-tauri/src/orchestrator/mod.rs` — Orchestrator owns chat loop, system prompt, memory injection, MAX_ITERATIONS.
- `src-tauri/src/orchestrator/client.rs` — `OmniClient` (OpenAI-style `/v1/chat/completions`, SSE streaming, tool_call accumulator).
- `src-tauri/src/orchestrator/tools.rs` — OpenAI-style `tools[]` definitions (`{type:"function", function:{name,parameters}}`).
- `src-tauri/src/chat.rs` — `ChatMessage`, `ToolCall { id, type:"function", function:{name, arguments:string} }` (OpenAI shape).
- DB: `settings` table (key/value text). Existing keys: `omnirouter.base_url`, `omnirouter.model`, `omnirouter.api_key`.
- Frontend: `frontend/src/components/voice/VoicePanel.tsx` is the pattern for a tabbed settings panel; `OrchestratorPanel.tsx` is the orchestrator UI (no settings yet).

---

## 3. Architecture

### 3.1 Provider trait

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn label(&self) -> &str;
    async fn chat_stream(
        &self,
        req: ChatRequest,
        on_text_delta: &mut dyn FnMut(&str),
    ) -> Result<ChatRespMessage>;
    async fn ping(&self) -> Result<PingInfo>; // for "Test connection"
}
```

`ChatRequest` carries provider-neutral inputs:

```rust
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Value>,    // OpenAI-shape — Anthropic adapter translates internally
    pub tools: Option<Vec<Value>>, // OpenAI-shape; adapter converts to Anthropic schema
    pub system: Option<String>,
    pub temperature: f32,
    pub max_tokens: u32,
}
```

Concrete impls:

| File                                       | Provider     | Notes                                  |
|--------------------------------------------|--------------|----------------------------------------|
| `orchestrator/providers/omni.rs`           | OmniRouter   | Wraps existing OmniClient logic        |
| `orchestrator/providers/anthropic.rs`      | Anthropic    | Messages API, native tools, caching    |
| `orchestrator/providers/mod.rs`            | factory      | `build_provider(db)` selects via settings |

### 3.2 Anthropic adapter — translation layer

| Concept     | OpenAI shape (internal)                                    | Anthropic Messages API                                |
|-------------|------------------------------------------------------------|-------------------------------------------------------|
| auth        | Bearer                                                     | `x-api-key` + `anthropic-version: 2023-06-01`         |
| endpoint    | `POST /v1/chat/completions`                                | `POST https://api.anthropic.com/v1/messages`          |
| system      | `{role:"system", content}` in messages array               | Top-level `system: string \| ContentBlock[]`          |
| user/asst   | `{role, content:string}`                                   | `{role, content: ContentBlock[]}` (text/tool_use/...) |
| tools       | `tools[].function.{name,parameters}`                       | `tools[].{name,description,input_schema}`             |
| tool call   | assistant `tool_calls[]` with stringified JSON `arguments` | assistant `content: [{type:"tool_use", id, name, input}]` |
| tool result | `{role:"tool", tool_call_id, content}`                     | user `content: [{type:"tool_result", tool_use_id, content}]` |
| streaming   | SSE `data:` chunks with `choices[0].delta.content`         | SSE events `message_start`, `content_block_start`, `content_block_delta` (text_delta, input_json_delta), `message_delta`, `message_stop` |

The adapter:
1. Strips a leading `system` message (if present) and lifts it to the top-level `system` field with a `cache_control: { type: "ephemeral" }` annotation on the stable header — this hits Anthropic's **prompt caching** for the SYSTEM_PROMPT_BASE + tool definitions.
2. Converts `assistant.tool_calls[]` → `[{type:"tool_use", ...}]` and `role:"tool"` → user `[{type:"tool_result", tool_use_id, content}]`.
3. Converts `tools[].function.{name, description, parameters}` → `[{name, description, input_schema: parameters}]`.
4. Streams Anthropic SSE → emits text deltas via `on_text_delta`. Tool input deltas (`input_json_delta`) are accumulated per content block; on `content_block_stop` we have the full JSON → packaged into our internal `ToolCall { id, type:"function", function:{name, arguments} }`.

### 3.3 Prompt caching

Anthropic supports `cache_control: { type: "ephemeral" }` on system / tools / message content. PIG IDE marks two stable regions:

- The leading static portion of the system prompt (`SYSTEM_PROMPT_BASE`, before the dynamic `[WORLD STATE]` block).
- The full `tools` array (Kiro's tool catalog rarely changes within a session).

Caching is enabled when `architect.cache_enabled` ≠ `"false"`.

### 3.4 Fallback policy

```text
on chat_stream() error:
    if provider == anthropic
       and error in {HTTP 5xx, 529 overloaded, timeout}
       and architect.fallback_enabled != "false":
           swap model: primary -> architect.fallback_model
           emit chat://status { state: "fallback" }
           retry once
    else:
           surface error (existing path)
```

Retries inside one HTTP attempt use jittered exponential backoff (250ms, 500ms, 1000ms with ±50% jitter) up to 3 attempts before declaring "real" failure that triggers the fallback model swap.

### 3.5 Long context

`max_tokens` defaults to 8192; context window is 200k for Opus. The orchestrator already truncates tool results to 4000 chars (`truncate_for_model`) so fixing that is out of scope.

### 3.6 Settings keys

| Key                          | Default               | Description                              |
|------------------------------|-----------------------|------------------------------------------|
| `provider`                   | `anthropic`           | `anthropic` \| `omnirouter`              |
| `architect.model`            | `claude-opus-4-5`     | Primary model                            |
| `architect.fallback_model`   | `claude-opus-4`       | Used on 5xx/529/timeout                  |
| `architect.fallback_enabled` | `true`                |                                          |
| `architect.cache_enabled`    | `true`                | Anthropic prompt caching                 |
| `architect.max_tokens`       | `8192`                |                                          |
| `anthropic.api_key`          | (read from env first) | Stored encrypted in DB if user typed it  |
| `anthropic.base_url`         | `https://api.anthropic.com` | For self-hosted / proxies         |
| `omnirouter.base_url`        | `http://localhost:20128` | unchanged                             |
| `omnirouter.model`           | `kr/claude-opus-4.7`  | unchanged                                |
| `omnirouter.api_key`         | unchanged             | unchanged                                |

API-key resolution order:
1. `ANTHROPIC_API_KEY` env var.
2. OS keychain (via `keyring` crate, if available — feature-gated).
3. `anthropic.api_key` setting (encrypted-at-rest scaffold; v1 stores plaintext but never logs it; secure storage upgrade is in `docs/architect-model.md` follow-ups).

`.env*` is already gitignored (verified).

### 3.7 Settings UX (frontend)

A new `SettingsPanel` mounted as a tab option in the right pane (alongside Memory/Voice/Browser/Files), with a `provider` section:

```
┌─ Architect model ────────────────────────────────────┐
│ Provider:    [Anthropic ▼]                          │
│ Model:       [claude-opus-4-5 ▼]                    │
│ Fallback:    [claude-opus-4   ▼] [✓] enabled        │
│ Caching:     [✓] enabled                            │
│ Max tokens:  [ 8192 ]                               │
│                                                      │
│ API key:     [••••••••••••••••] [Save]              │
│              (or set ANTHROPIC_API_KEY env var)     │
│                                                      │
│              [Test connection]    ✓ ok / ✗ error    │
└──────────────────────────────────────────────────────┘
```

Backed by a single new IPC command: `provider_test_connection() -> Result<PingInfo, String>` that the panel calls on the Test button.

### 3.8 Migration

- Existing workspaces only depend on the `settings` table. New keys are read with defaults — no schema migration required.
- First run: if `ANTHROPIC_API_KEY` is set in env and `provider` is unset, default to `anthropic`. If the key is missing AND no `omnirouter.base_url` is reachable, the panel shows a clear "configure provider" empty-state.
- Existing OmniRouter users keep working: if `provider = omnirouter`, behaviour is unchanged.

---

## 4. Tests

| Test                                                       | Location                                  |
|------------------------------------------------------------|-------------------------------------------|
| Anthropic SSE parser → text deltas + tool_use round trip   | `orchestrator/providers/anthropic.rs`     |
| OpenAI→Anthropic message translation                       | `orchestrator/providers/anthropic.rs`     |
| Tool definition conversion                                 | `orchestrator/providers/anthropic.rs`     |
| Fallback on simulated 529                                  | `orchestrator/providers/mod.rs`           |
| Settings persistence (provider/model)                      | `orchestrator/providers/mod.rs`           |

All tests use static fixtures — **zero live API calls**.

---

## 5. Boundaries

- Skills system, BridgeSpace 3 port, PigVoice — untouched.
- `OmniClient` is preserved as an alternate provider; not deleted.
- Frontend voice panel and orchestrator chat panel — not modified beyond adding settings tab entry.

---

## 6. Out of scope

- Full OS-keychain integration (left as a follow-up; v1 supports env var + setting).
- Cost telemetry dashboard (server logs token counts via `tracing`, but no UI yet).
- Streaming tool-call deltas to the UI (current behaviour: text deltas stream, tool calls are accumulated then emitted as a single message — same as the existing OmniClient).

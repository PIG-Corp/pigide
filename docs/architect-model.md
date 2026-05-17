# Architect model

PIG IDE's Kiro orchestrator runs against a swappable LLM backend. The default is the **Anthropic Messages API** with **Claude Opus 4.5** as primary and **Claude Opus 4** as automatic fallback. The original OmniRouter (OpenAI-compatible) provider remains available for self-hosted or proxy setups.

## Default behaviour

| Slot     | Model              |
|----------|--------------------|
| Primary  | `claude-opus-4-5`  |
| Fallback | `claude-opus-4`    |

The fallback fires automatically on `5xx`, `529 overloaded`, request timeouts, and transient network errors after three retries with jittered exponential backoff (250 ms → 500 ms → 1000 ms ± 50%). Non-retryable errors (e.g. 401, 400 invalid model) propagate immediately.

## API key

Resolved in priority order:

1. `ANTHROPIC_API_KEY` environment variable.
2. `anthropic.api_key` in the SQLite settings table (entered via the Settings UI).

`.env*` is in `.gitignore`. Keys saved through the UI are stored in `~/.config/pigide/db.sqlite` and never logged.

## Settings UI

`Right pane → Settings`:

- **Provider** — `Anthropic (Claude)` or `OmniRouter (OpenAI-compatible)`.
- **Model / Fallback model** — pick from the registered Anthropic IDs.
- **Fallback enabled** — toggle automatic 5xx/529 fallback.
- **Prompt caching** — when on, the static head of the system prompt and the entire tool catalog get `cache_control: ephemeral`.
- **Max tokens** — Anthropic `max_tokens` cap (default 8192).
- **API key** — secure password input; shows a placeholder when the env var is providing the key.
- **Test connection** — round-trips a tiny `ping` request and reports `✓` / `✗` with the upstream error.

## Configuration keys

All persisted in the `settings` table:

| Key                          | Default                       |
|------------------------------|-------------------------------|
| `provider`                   | `anthropic`                   |
| `architect.model`            | `claude-opus-4-5`             |
| `architect.fallback_model`   | `claude-opus-4`               |
| `architect.fallback_enabled` | `true`                        |
| `architect.cache_enabled`    | `true`                        |
| `architect.max_tokens`       | `8192`                        |
| `anthropic.base_url`         | `https://api.anthropic.com`   |
| `anthropic.api_key`          | (read from env first)         |
| `omnirouter.base_url`        | `http://localhost:20128`      |
| `omnirouter.model`           | `kr/claude-opus-4.7`          |
| `omnirouter.api_key`         | (optional)                    |

## Tool calling

Anthropic's native `tool_use` / `tool_result` content blocks are used. The orchestrator's existing OpenAI-shape `tool_calls` round-trip cleanly through the adapter:

- Assistant `tool_calls[]` → `[{type:"tool_use", id, name, input}]`.
- `role:"tool"` messages with `tool_call_id` → user `[{type:"tool_result", tool_use_id, content}]`.
- OpenAI `tools[].function.{name, description, parameters}` → `[{name, description, input_schema}]`.

Streaming SSE events handled: `message_start`, `content_block_start`, `content_block_delta` (`text_delta`, `input_json_delta`), `content_block_stop`, `message_delta`, `message_stop`, `error`. Text deltas stream live to the chat UI; `input_json_delta` fragments are concatenated into a complete JSON `arguments` string before tool dispatch.

## Prompt caching

When `architect.cache_enabled = true`, two stable regions are marked with `cache_control: { type: "ephemeral" }`:

1. The static head of the system prompt — everything before the `[WORLD STATE]` marker.
2. The trailing tool definition (Anthropic caches up to and including that block, so all earlier tool entries are also covered).

The dynamic `[WORLD STATE]` block — workspace list, agents, tasks — is sent uncached so the model always sees the latest state.

## Provider trait

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn label(&self) -> &str;
    fn primary_model(&self) -> &str;
    fn fallback_model(&self) -> Option<&str>;
    async fn chat_stream(&self, req: ChatRequest, delta_tx: DeltaTx)
        -> Result<ChatRespMessage>;
    async fn ping(&self) -> Result<PingInfo>;
}
```

Adding a provider: drop a module under `src-tauri/src/orchestrator/providers/` and add a branch to `build_provider`.

## Tests

Unit tests in `src-tauri/src/orchestrator/providers/` cover:

- OpenAI ↔ Anthropic message translation (system lift, tool_calls, tool_result).
- Cache-control placement on system + tools.
- SSE parser: text-delta accumulation and tool_use round-trip.
- Retry classifier: `5xx`, `529 overloaded`, network errors retryable; `4xx` not.
- Settings persistence and provider selection.

Run with `cargo test -p pigide --lib`. No live API calls.

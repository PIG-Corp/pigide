# FINAL_REPORT — Architect Model wiring

**Date:** 2026-05-15
**Branch:** main (no PR opened — repo is single-branch)
**Owner:** Architect-Model agent (Claude Code, Opus 4.7 backend)

---

## What shipped

A pluggable LLM provider layer with **Anthropic Claude Opus 4.5 as default** for the Kiro orchestrator. Existing OmniRouter behaviour preserved as an alternative.

### Backend (Rust)

- `src-tauri/src/orchestrator/providers/mod.rs` — `LlmProvider` trait + `ChatRequest` / `ChatRespMessage` / `PingInfo` / `DeltaTx` channel-based streaming sink. Settings keys + `build_provider(db)` factory + `selected_provider(db)` resolver. Env-first API-key resolver. 4 unit tests.
- `src-tauri/src/orchestrator/providers/anthropic.rs` — Anthropic Messages API adapter. SSE parser handling `message_start`, `content_block_{start,delta,stop}`, `message_{delta,stop}`, `error` events; `text_delta` streams to the UI mpsc channel, `input_json_delta` accumulates into the assistant's tool_call `arguments` string. Tool round-trip: assistant `tool_calls[]` → `tool_use` blocks; `role:"tool"` → user `tool_result` blocks. OpenAI `tools[].function.parameters` → Anthropic `input_schema`. Prompt caching with `cache_control: ephemeral` on the static system head (split at `[WORLD STATE]`) and the trailing tool. Retries: 3 attempts on retryable errors with jittered backoff (250/500/1000 ms ±50%); auto-swap to fallback model on final failure. 9 unit tests.
- `src-tauri/src/orchestrator/providers/omni.rs` — Existing OmniClient lifted into the trait.
- `src-tauri/src/orchestrator/mod.rs` — `build_provider`/`build_request` replace `build_client`. Tool loop drives the trait via mpsc channel: provider future and a tokio-spawned forwarder run concurrently so `chat://chunk` events still stream live to the UI. The 6-arg signature with skills (added by parallel agent) is preserved.
- `src-tauri/src/commands.rs` — New IPC commands `provider_info` and `provider_test_connection` returning `ProviderInfo` / `PingInfo`. Registered in `lib.rs::invoke_handler`.

### Frontend (TypeScript / React)

- `frontend/src/components/ArchitectSettingsPanel.tsx` — Settings panel: provider dropdown, model dropdowns (Anthropic IDs registered; free-form for OmniRouter), fallback toggle, caching toggle, max-tokens, API base URL, secure password input for the API key, "Test connection" button with `✓ ok` / `✗ error` badge.
- `frontend/src/state/ipc.ts` — `providerInfo()` and `providerTestConnection()` bindings.
- `frontend/src/App.tsx` — New `Settings` tab in the right pane.
- `docs/architect-model.md` — Full feature & config reference.
- `README.md` — Architect-model section pointing at the new doc.

### Plan & artifacts

- `ARCHITECT_MODEL_PLAN.md` — Architecture plan, settings keys, fallback policy, migration notes.
- `FINAL_REPORT_ARCHITECT_MODEL.md` — This file.

## Verification

- `cargo build -p pigide` — clean (2 pre-existing warnings unrelated to this change).
- `cargo test -p pigide --lib` — **117 passed, 0 failed**, including 13 new tests covering Anthropic translation, SSE parser, tool round-trip, retry classifier, and provider selection.
- `pnpm tsc --noEmit` (via `pnpm build`) — clean.
- `pnpm build` — production bundle builds successfully (`dist/assets/index-*.js` 889 kB, gzip 256 kB).
- `pnpm lint` — pre-existing errors in `voice/*` and `MemoryGraph` (none from added files).

## Boundaries respected

- Skills system — untouched (only adapted Orchestrator constructor signature to the 6-arg variant added by the parallel agent).
- BridgeSpace 3 port files — untouched.
- PigVoice — untouched.
- OmniRouter provider preserved as opt-in.

## Security

- API key sources: `ANTHROPIC_API_KEY` env var first, then `anthropic.api_key` setting. Never logged. UI uses `type="password"` with `autoComplete="new-password"`.
- `.env*` already in `.gitignore`.
- No external network calls in tests; SSE fixtures hand-rolled.

## Known follow-ups (out of scope)

- OS keychain integration (currently env var or DB setting only).
- Cost / token telemetry dashboard (currently `tracing::debug!` only).
- Streaming individual tool-call argument deltas to the UI (currently accumulated then emitted).

STATUS: architect_model_complete

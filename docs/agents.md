# Supported CLI agents

PigIDE spawns each agent as a PTY subprocess. Defaults can be overridden via settings keys `bin.<type>` and `args.<type>`.

## Orchestrator settings

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `orchestrator.max_iterations` | integer | 20 | Maximum turn-loop iterations per user message. Increase for complex multi-agent dispatches. Clamped to 1–100. |
| `orchestrator.max_phantom_retries` | integer | 2 | Max phantom-tool-call re-prompts per turn. Clamped to 0–5. |

## kiro-cli
Kiro's own coding agent. Default args: `chat --trust-all-tools`. Reads `~/.aws/credentials` for Bedrock or relies on Kiro auth state.

## claude
Anthropic Claude Code CLI. Default args: none (interactive mode). Needs `ANTHROPIC_API_KEY` or an existing Claude Code session login.

## opencode
Terminal-native open-source agent (alias `oc`). Default args: none. Reads `OPENAI_API_KEY` / provider env vars and `~/.opencode/config.json` for model selection.

## codex
OpenAI Codex CLI (aliases: `openai-codex`). Official OpenAI coding agent — runs in interactive TUI mode inside the PigIDE PTY tile.

Install:

```bash
npm i -g @openai/codex
# or
brew install codex
```

Configure:

- `OPENAI_API_KEY` — required; inherited from PigIDE's environment, so export it before launching the app (`export OPENAI_API_KEY=sk-...`).
- `bin.codex` setting **or** `PIGIDE_CODEX_BIN` env var — override the binary path. Otherwise PigIDE searches `~/.local/bin/codex`, `~/.npm-global/bin/codex`, `~/.bun/bin/codex`, `/usr/local/bin/codex`, `/opt/homebrew/bin/codex`, `/usr/bin/codex`, then `$PATH`.
- `args.codex` setting — override default argv (default is empty, which launches the interactive TUI).

Spawn from the orchestrator: `spawn_agent { agent_type: "codex" }`.

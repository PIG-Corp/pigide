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

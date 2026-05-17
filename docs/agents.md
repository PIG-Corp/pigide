# Supported CLI agents

PigIDE spawns each agent as a PTY subprocess. Defaults can be overridden via settings keys `bin.<type>` and `args.<type>`.

## kiro-cli
Kiro's own coding agent. Default args: `chat --trust-all-tools`. Reads `~/.aws/credentials` for Bedrock or relies on Kiro auth state.

## claude
Anthropic Claude Code CLI. Default args: none (interactive mode). Needs `ANTHROPIC_API_KEY` or an existing Claude Code session login.

## aider
AI pair-programmer that edits a git repo. Default args: `--no-auto-commits` so PigIDE controls commits. Needs `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, or another supported provider key. Pass `--model <name>` via `args.aider` to switch models.

## goose
Block's open-source coding agent. Default args: `session` (starts an interactive session). Configured through `~/.config/goose/config.yaml`; provider keys come from env (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, etc.).

## opencode
Terminal-native open-source agent (alias `oc`). Default args: none. Reads `OPENAI_API_KEY` / provider env vars and `~/.opencode/config.json` for model selection.

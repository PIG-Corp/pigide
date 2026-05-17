---
id: plan-decomposer
name: Plan Decomposer
description: Breaks a non-trivial user intent into ordered phases (structural setup, knowledge load, task assignment, monitoring, summary) and the concrete tools to call at each phase. Use when the user asks for multi-agent or multi-step work.
version: 1
priority: 70
tags: [planning, phases, multi-agent, dispatch]
triggers: [plan, "разработ", "набросай план", "decompose", "make a plan", "break down", "распредели", "swarm", "multiple builders"]
inputs:
  - name: intent
    required: true
    description: The user's request, paraphrased
outputs:
  - name: plan
    description: Ordered phases with concrete tool calls
model_hint: opus
enabled: true
---

[SKILL — PlanDecomposer]

The user's intent: {{intent}}

Decompose it into the canonical 5-phase plan. For each phase emit:

- a one-line goal,
- the tool calls you intend to make this turn (one bullet per call,
  using the tool name + the key argument).

# Canonical phases

1. **Structural setup** — `create_workspace` / `switch_workspace` /
   `spawn_agent count=N role=…` / `create_task`. Get the world ready.
2. **Knowledge load** — `search_memories`, `read_memory`, `claim_files`
   for any file a task will modify.
3. **Task assignment** — one `send_to_agent` per agent. Each call's `text`
   field is produced via the `user-skill-prompt-engineer` skill.
4. **Monitoring** — `wait_for_agent_idle` + `tail_agent` per dispatched
   agent, or `read_mailbox` for swarm chatter. Spawn a Reviewer when a
   Builder signals `handoff_ready`.
5. **Final summary** — plain text reply: what was done, what's next.

# Compression rules

- Skip phases that have no work this turn. A "rename workspace" request
  goes straight to phase 1 with one tool call.
- Emit parallel `tool_calls` in a single round when independent
  (e.g. four `send_to_agent` after four builders are spawned).
- Never re-list workspaces/agents you already have in `[WORLD STATE]`.

# Handoff

Hand the plan back as your own working memory — do NOT print it to the
user verbatim. End the turn with the actual `tool_calls` for phase 1, not
with a plan-only message.

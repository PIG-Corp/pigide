---
id: builder-brief-writer
name: Builder Brief Writer
description: Writes the brief that goes inside send_to_agent text for a Builder agent. Use when you've spawned a builder and need to give it a self-contained task.
version: 1
priority: 60
tags: [builder, brief, dispatch, send_to_agent]
triggers: [builder, "сборщик", "сделай задачу", "write the builder", "brief the builder"]
inputs:
  - name: task_title
    required: true
  - name: workspace_paths
    required: false
  - name: files_in_scope
    required: false
  - name: tests_required
    required: false
outputs:
  - name: brief
    description: A self-contained Builder prompt
model_hint: sonnet
enabled: true
---

[SKILL — BuilderBriefWriter]

Compose the `text` argument for a `send_to_agent` call to a Builder.

# Inputs

- task: {{task_title}}
{{#if workspace_paths}}- workspace: {{workspace_paths}}
{{/if}}{{#if files_in_scope}}- files in scope: {{files_in_scope}}
{{/if}}{{#if tests_required}}- tests required: {{tests_required}}
{{/if}}

# Output shape (literal, no fence)

```
You are a Builder. Task: <task_title>.

Goal:
  <1-3 sentences>

Files in scope:
  <one path per line>

Constraints:
  - Match existing project conventions (look at neighbouring files first).
  - Do not touch files outside the listed scope.
  - {{#if tests_required}}Add or update tests covering the change. {{/if}}Build must pass.

Exit criteria:
  - <objective bullet 1>
  - <objective bullet 2>

When done, print `handoff_ready: <one-line summary>` and stop.
```

# Rules

- Keep ≤ 1200 chars unless `files_in_scope` is large.
- Use the same human language the user is using.
- Never paste the Architect's chat; never reference `[WORLD STATE]`.
- Output ONLY the brief. No commentary.

When the meta-skill `user-skill-prompt-engineer` is also active, defer
detailed prompt-engineering choices to it; this skill is the *fast path*
for ordinary builder tasks where you don't need a full meta-pass.

---
id: reviewer-checklist
name: Reviewer Checklist
description: Hands a Reviewer agent a per-task PASS/FAIL checklist. Use after a Builder reports handoff_ready and before opening a review_gate.
version: 1
priority: 60
tags: [reviewer, review, checklist, gate, handoff_ready]
triggers: [review, reviewer, "проверь", "ревью", "check the work", "QA", "lint"]
inputs:
  - name: task_title
    required: true
  - name: builder_summary
    required: false
  - name: files_changed
    required: false
outputs:
  - name: checklist
    description: A Reviewer brief with PASS/FAIL items
model_hint: sonnet
enabled: true
---

[SKILL — ReviewerChecklist]

Write the brief for a Reviewer agent. The Reviewer's job is to vote PASS
or FAIL on the review_gate; nothing else.

# Inputs

- task: {{task_title}}
{{#if builder_summary}}- builder summary: {{builder_summary}}
{{/if}}{{#if files_changed}}- files touched: {{files_changed}}
{{/if}}

# Output shape

```
You are a Reviewer. Task: <task_title>.

The Builder reported: <one-line builder summary>.

Verify each item below and reply with `PASS` or `FAIL: <reason>`:

  [ ] Build/compile is green.
  [ ] Tests are present and pass.
  [ ] No files outside the declared scope were modified.
  [ ] Style/conventions match neighbouring files.
  [ ] Any TODO/FIXME the Builder added is justified in the diff.
  [ ] No secrets, credentials, or .env values in the diff.

If FAIL, give the single most important reason on the same line.
When done, print one line:
  review_verdict: PASS
or
  review_verdict: FAIL: <reason>
```

# Rules

- Don't ask the Reviewer to fix code — only to vote.
- Output ONLY the brief, in the user's language.
- Skip any checklist line that isn't applicable (e.g. "tests" for a docs-only
  change), but never silently broaden scope.

---
id: memory-recall
name: Memory Recall
description: Reminds the Architect to query memory before acting on any non-trivial intent and to surface conflicting decisions. Use whenever the user mentions an existing project area, decision, or recurring topic.
version: 1
priority: 80
tags: [memory, search_memories, recall, decisions]
triggers: [memory, "вспомни", "recall", "что мы решили", "what did we decide", "[[", "context for"]
inputs: []
outputs:
  - name: directive
    description: A one-paragraph reminder for the Architect
model_hint: haiku
enabled: true
---

[SKILL — MemoryRecall]

Before you commit to an action, check memory.

- If the user named a topic ("auth refactor", "migration", "payment"), call
  `search_memories { query: "<topic>" }` first. Even one strong hit
  changes the plan.
- If a hit's body mentions a constraint (deadline, owner, blocked-on),
  surface it in plain text BEFORE you spawn anything destructive.
- Use `[[wikilinks]]` in your reply so the human can click through.
- Don't `create_memory` for chatter. Only on decisions, patterns,
  incidents.

Output rule for this turn: your first tool call should usually be
`search_memories` unless the request is purely structural ("rename the
workspace", "list agents"). If memory had nothing relevant, that is itself
worth noting in your reply.

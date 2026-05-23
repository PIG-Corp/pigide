# PigIDE Chat Panel (Bridge) Visual & UX Audit
Date: 2026-05-23
Auditor: Senior Frontend/UX Engineer

This document outlines the findings of the visual and UX audit of the right-hand Chat Panel (Bridge) in PigIDE.

---

## 1. Summary of Identified Issues

| Bug ID | Component / Area | Description | Severity | Target File & Line |
| :--- | :--- | :--- | :--- | :--- |
| **UX-01** | Scroll | User scrolled up to read history is yanked back to bottom on streaming, or scroll state is not reset when typing a new message. | **High** | `frontend/src/components/OrchestratorPanel.tsx:90` |
| **UX-02** | Markdown | Chat messages render as raw text instead of parsed Markdown (lists, headers, inline code, tables, links are not rendered as HTML). | **Critical** | `frontend/src/components/OrchestratorPanel.tsx:434, 469` |
| **UX-03** | Tool Calls | JSON arguments are minified and unformatted, making tool arguments difficult to read. | **Medium** | `frontend/src/components/OrchestratorPanel.tsx:440` |
| **UX-04** | Tool Calls | Running tool calls do not show a status indicator and are collapsed by default, hiding active tasks. | **High** | `frontend/src/components/OrchestratorPanel.tsx:435` |
| **UX-05** | Tool Outputs | Extremely long tool results (4KB+) render fully, cluttering the chat view and causing layout bloat. | **High** | `frontend/src/components/OrchestratorPanel.tsx:455` |
| **UX-06** | Bridge Orb | The orb caption is hardcoded to static `"Speaking | Speaking.."` regardless of active voice, transcription, thinking, or idle state. | **Medium** | `frontend/src/components/OrchestratorPanel.tsx:293` |
| **UX-07** | Styles | Missing custom styling for system/error message logs, causing inconsistent appearance. | **Medium** | `frontend/src/styles.css` |
| **UX-08** | Animations | Messages pop up abruptly during streaming/sending without transitions. | **Medium** | `frontend/src/styles.css` |
| **UX-09** | Scroll / Resize | Resizing the panel vertically or horizontally changes text wrapping but does not auto-scroll to the bottom. | **Medium** | `frontend/src/components/OrchestratorPanel.tsx` |

---

## 2. Detailed Findings & Proposed Fixes

### UX-01: Auto-scroll Latching & User Send Reset
* **Severity:** High
* **Reproduction Steps:**
  1. Scroll up slightly in the chat history to read older messages.
  2. Type a new message in the chat input and hit Enter/Send.
  3. The message is sent but the chat area does not scroll to the bottom. The user must manually scroll down to see their message.
* **Proposed Fix:** Reset `userScrolledUp.current = false` inside the `send` callback in `OrchestratorPanel.tsx`.

### UX-02: Missing Markdown Rendering
* **Severity:** Critical
* **Reproduction Steps:**
  1. Send a message to the orchestrator that generates lists, headers, or code blocks.
  2. The messages render with raw Markdown syntax (e.g. `**bold**`, `- item`), impairing layout readability.
* **Proposed Fix:** Create a custom React-based light Markdown rendering component (`Markdown`) to parse and format headers, lists, code, tables, bold, italics, and links securely without external dependencies.

### UX-03: Unformatted Tool Call Arguments
* **Severity:** Medium
* **Reproduction Steps:**
  1. Open a tool call details drop-down.
  2. The JSON arguments are rendered as a single minified line.
* **Proposed Fix:** Parse the JSON arguments string and pretty-print it using `JSON.stringify(..., null, 2)` inside a dedicated `ToolCallView` component.

### UX-04: Running Tool Indicator & Auto-Expand
* **Severity:** High
* **Reproduction Steps:**
  1. Trigger an agent action that calls a tool.
  2. The tool details are rendered collapsed by default and have no visible "running" indicator.
* **Proposed Fix:** Identify running tools by checking if a corresponding `tool` result message exists in the chat. If the tool is running, default `<details>` to open and display an animated "running" status spinner.

### UX-05: Unbounded Tool Output Length (Layout Bloat)
* **Severity:** High
* **Reproduction Steps:**
  1. Trigger a tool call that outputs large data (e.g., directory list or file content of 4KB+).
  2. The raw output spans thousands of lines, cluttering the view.
* **Proposed Fix:** Truncate tool output inside `.chat-log-body.tool` if it exceeds 1200 characters, showing a "Show full output" toggle.

### UX-06: Static Bridge Orb Caption
* **Severity:** Medium
* **Reproduction Steps:**
  1. Look at the caption below the Bridge Orb when idle.
  2. It shows `"Speaking | Speaking.."` instead of reflecting whether it is listening, transcribing, thinking, or running a tool.
* **Proposed Fix:** Connect the caption to `voiceState` and orchestrator `status` states to render dynamic, informative status captions.

### UX-07: Incomplete System Message Styles
* **Severity:** Medium
* **Reproduction Steps:**
  1. Trigger a system error or offline agent message.
  2. Notice that the vertical log line and sender label are unstyled or inherit incorrect colors.
* **Proposed Fix:** Add CSS rules for `.chat-log-line.system`, `.chat-log-sender.system`, and `.chat-log-body.system` to render error and system text in warning/danger styling.

### UX-08: Lack of smooth transitions
* **Severity:** Medium
* **Reproduction Steps:**
  1. Observe new messages/tool logs popping in.
  2. The logs appear instantly and cause layout shifts.
* **Proposed Fix:** Apply a slide-up-and-fade animation (`chat-row-enter`) on `.chat-log-row` mount.

### UX-09: Resize scroll adjustments
* **Severity:** Medium
* **Reproduction Steps:**
  1. Scroll to the bottom of the chat.
  2. Drag the splitter to resize the right pane.
  3. The scroll position jumps or shifts away from the bottom as wrapping layout updates.
* **Proposed Fix:** Setup a `ResizeObserver` on the chat list container to adjust `scrollTop` when element dimensions change, provided the user was already scrolled to the bottom.

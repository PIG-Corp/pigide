#!/usr/bin/env python3
"""PreToolUse hook: block edits/writes to .env, lockfiles, and other protected paths.

Reads the Claude Code hook payload from stdin (JSON) and exits 2 with a stderr
message to block the tool call when the target path matches a protected pattern.
"""
import json
import os
import sys

PROTECTED_BASENAMES = {".env", "Cargo.lock", "pnpm-lock.yaml"}
PROTECTED_PREFIXES = (".env.",)  # .env.local, .env.production, etc.


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, ValueError):
        return 0

    tool_input = payload.get("tool_input") or {}
    path = tool_input.get("file_path") or tool_input.get("path") or ""
    if not path:
        return 0

    base = os.path.basename(path)
    if base in PROTECTED_BASENAMES or any(base.startswith(p) for p in PROTECTED_PREFIXES):
        sys.stderr.write(
            f"BLOCKED: {path} is a protected file (secrets / lockfile). "
            "Ask the user to confirm explicitly before editing, or have them edit it manually.\n"
        )
        return 2

    return 0


if __name__ == "__main__":
    sys.exit(main())

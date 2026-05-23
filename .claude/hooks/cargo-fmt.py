#!/usr/bin/env python3
"""PostToolUse hook: auto-cargo-fmt edited Rust files inside src-tauri/.

Silent on success. Errors written to stderr but exit 0 — never block the agent
on a formatter problem.
"""
import json
import os
import subprocess
import sys

PROJECT_ROOT = "/home/camer/pigide"
TAURI_DIR = os.path.join(PROJECT_ROOT, "src-tauri")


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, ValueError):
        return 0

    tool_input = payload.get("tool_input") or {}
    path = tool_input.get("file_path") or tool_input.get("path") or ""
    if not path or not path.endswith(".rs"):
        return 0
    if not path.startswith(TAURI_DIR + os.sep):
        return 0
    if not os.path.isfile(path):
        return 0

    try:
        subprocess.run(
            ["cargo", "fmt", "--", path],
            cwd=TAURI_DIR,
            check=False,
            timeout=20,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired) as exc:
        sys.stderr.write(f"cargo-fmt hook: {exc}\n")

    return 0


if __name__ == "__main__":
    sys.exit(main())

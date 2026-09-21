#!/usr/bin/env python3
"""
AI Assistant — persistent workspace + idle auto-pause.

Python port of `1-ai-assistant.ts`. `idle_timeout_secs` makes the
reaper auto-snapshot + wipe `source_dir` once the workspace goes
idle. The next exec transparently restores before running, so the
loop body never notices the pause.

Run with:
    uv run --with agentjail scripts/demos/1-ai-assistant.py
or, against the in-repo SDK:
    uv run --with ./packages/sdk-python scripts/demos/1-ai-assistant.py
"""

from __future__ import annotations

import os
import time

from agentjail import Agentjail

BASE_URL = os.environ.get("CTL_URL", "http://localhost:7070")
API_KEY = os.environ.get("AGENTJAIL_API_KEY")


def ai(aj: Agentjail, workspace_id: str, prompt: str) -> str:
    """Placeholder `ai(workspace, prompt)` — same shape as the TS demos."""
    safe = prompt.replace('"', '\\"')
    r = aj.workspaces.exec(
        workspace_id,
        cmd="/bin/sh",
        args=["-c", f'echo "ai: {safe}"'],
    )
    return r["stdout"].strip()


def step(msg: str) -> None:
    print(f"\u25b8 {msg}")


def ok(msg: str) -> None:
    print(f"\u2713 {msg}")


def main() -> None:
    scripted = ["what's the time?", "summarize this in 3 bullets", "goodbye"]

    with Agentjail(base_url=BASE_URL, api_key=API_KEY) as aj:
        step("Creating persistent workspace with 60s idle pause")
        ws = aj.workspaces.create(
            idle_timeout_secs=60,
            memory_mb=512,
            label="assistant-py",
        )
        ok(f"workspace {ws['id']} (paused? {ws['paused_at'] is not None})")

        try:
            for msg in scripted:
                step(f"user \u2192 {msg}")
                reply = ai(aj, ws["id"], msg)
                ok(f"assistant \u2190 {reply}")

                refreshed = aj.workspaces.get(ws["id"])
                if refreshed["paused_at"] is None and refreshed["auto_snapshot"] is None:
                    print(f"  workspace {ws['id']} is active")

                time.sleep(0.3)
        finally:
            try:
                aj.workspaces.delete(ws["id"])
            except Exception:
                pass


if __name__ == "__main__":
    main()

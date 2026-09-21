#!/usr/bin/env python3
"""Devin UserPromptSubmit hook: advise `/compact` when over Gobstopper policy.

Reads the hook payload on stdin (`session_id`), asks Gobstopper for the
session's policy decision, and emits `additionalContext` when the session is
over its compaction trigger. Read-only: never mutates the session store and
never invokes `/compact` itself. Fails silently so a broken hook never blocks
a prompt.
"""

import json
import os
import subprocess
import sys

GOBSTOPPER = os.environ.get("GOBSTOPPER_BIN") or os.path.expanduser(
    "~/.cargo/bin/gobstopper"
)


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return 0
    session_id = payload.get("session_id")
    if not session_id:
        return 0
    try:
        proc = subprocess.run(
            [
                GOBSTOPPER,
                "policy-check",
                "--provider",
                "devin",
                "--session",
                str(session_id),
                "--json",
            ],
            capture_output=True,
            text=True,
            timeout=8,
        )
        decision = json.loads(proc.stdout)
    except Exception:
        return 0
    if decision.get("action") != "provider_compact":
        return 0
    ctx = decision.get("context_tokens") or 0
    trig = decision.get("effective_trigger_tokens") or 0
    context = (
        f"[gobstopper] Context is {ctx:,} tokens, above the {trig:,}-token "
        "compaction trigger. Run /compact before continuing this turn."
    )
    print(
        json.dumps(
            {
                "hookSpecificOutput": {
                    "hookEventName": "UserPromptSubmit",
                    "additionalContext": context,
                }
            }
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

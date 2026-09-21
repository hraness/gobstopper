# Devin integration

Gobstopper integrates with Devin through its local session store and documented
control-plane surfaces. It reads Devin's session database for inspection and
planning, and it can rewrite payload bytes in place for **idle** sessions
through a locked, transactional write path. Live sessions remain
provider-owned: Gobstopper delegates to Devin's `/compact` and never injects
input into another process.

## Session discovery

Devin stores sessions in a SQLite database:

- `$DEVIN_DATA_DIR/sessions.db`
- `$XDG_DATA_HOME/devin/cli/sessions.db`
- `~/.local/share/devin/cli/sessions.db`

`gobstopper detect` lists Devin sessions alongside Codex and Claude Code.
Sessions are resolved by ID, ID prefix, or title — pass the session ID, never
the `sessions.db` path (the database is a shared store, not a transcript file).

A session is **active** while its `session_locks/<id>.lock` file is flock-held
by a live Devin process. Active sessions are provider-owned: Gobstopper plans
native delegation for them and never proposes local surgery.

## Inspection and planning

```sh
gobstopper detect --json
gobstopper plan <session-id> --json
gobstopper verify <session-id> --json
gobstopper export <session-id> > session.jsonl
```

`export` writes a deterministic canonical JSONL: a `session_meta` record
followed by `message_node` records for the session's main conversation chain.
The export is what `verify` checks (chain linkage, tool-call pairing) and what
`eval` consumes — hashing or snapshotting the shared database itself would mix
unrelated sessions, so the canonical per-session bytes are the unit of record.

For an **active** session, `plan` returns `provider_compact` with control
`devin: /compact`; `apply` refuses file surgery while the provider holds the
session lock. For an **idle** session, custom strategies (`auto`, `middle`, …)
produce elision plans, and `apply` performs the guarded store write below.
`compact`/`fork` still refuse store-backed sessions: a detached copy is not a
resumable Devin artifact. Detached export files can be processed and evaluated
directly.

## Guarded store mutation

`gobstopper apply <session-id>` on an idle Devin session rewrites the session
in place:

1. The canonical per-session export is snapshot into the vault (the shared
   database file is never the snapshot unit — unrelated sessions live in it).
2. The session lock is acquired; a locked session aborts before any write.
3. One SQLite transaction updates elided `chat_message` payloads with
   conditional identity checks, inserts a Gobstopper digest node on the main
   chain, and moves `sessions.main_chain_id` to it.
4. The post-write canonical export is re-derived and verified (chain linkage,
   tool-call pairing) before commit is considered final.

`gobstopper undo` restores the snapshot: original message payloads and chain
head are written back and Gobstopper-injected digest nodes are deleted. Undo
refuses if foreign provider nodes were appended after the snapshot — restore
never orphans provider state it did not create.

`watch` can perform this write automatically for idle Devin sessions that
cross the trigger, but only when the operator opts in:

```toml
[provider.devin]
auto_apply_store = true
```

and only under a provider-scoped watch (`gobstopper watch --provider devin`)
so Codex/Claude fork preparation is unaffected. Live sessions still defer to
`/compact` regardless of the flag.

Long-running sessions can exceed the default 512 MiB transcript bound —
raise it with `GOBSTOPPER_MAX_TRANSCRIPT_BYTES` (bytes) in the watch
environment when needed; oversized sessions are skipped, never truncated.

## Native compaction policy

Devin CLI exposes `/context` and `/compact`. Evaluate the layered Gobstopper
policy either with an explicit token count:

```sh
gobstopper policy-check \
  --provider devin \
  --context-tokens 300000 \
  --session-active \
  --json
```

or by session ID, which reads context usage and lock state directly from the
store (fast, no full discovery scan — suitable for hooks):

```sh
gobstopper policy-check --provider devin --session <session-id> --json
```

The session argument accepts an exact id, an id prefix, or a title
substring — the same resolution `detect`/`plan` use, so
`--session scarlet-gemini` works as well as a raw store id.

`--session current` resolves the active session bound to the caller's
working directory: it intersects the store's `working_directory` with the
flock-held `session_locks/*.lock` set and prefers the longest matching
directory, then the most recently active. It refuses to guess when locked
sessions are ambiguous — this is what the in-agent rule (below) and TUI
hook rely on:

```sh
gobstopper policy-check --provider devin --session current --json
```

An over-threshold response uses:

```json
{
  "provider": "devin",
  "action": "provider_compact",
  "control": "/compact"
}
```

The caller executes `/compact` inside the owned Devin session. Gobstopper never
becomes a second session writer.

Configure Devin independently from Codex and Claude Code:

```toml
[provider.devin]
trigger_tokens = 200_000
floor_tokens = 40_000
min_savings_tokens = 4_096
```

`floor_tokens` and `min_savings_tokens` remain useful shared policy metadata,
but Devin controls the actual post-compaction result.

## Prompt hook

Devin's `UserPromptSubmit` hook can advise compaction before each turn.
`gobstopper install-hooks` registers a native callback —

```json
{
  "UserPromptSubmit": [
    {
      "matcher": "",
      "hooks": [
        {
          "type": "command",
          "command": "gobstopper hook prompt-policy:devin",
          "timeout": 10
        }
      ]
    }
  ]
}
```

— into `~/.config/devin/config.json` under the `"hooks"` key (Devin's
documented user-level location; the flat `hooks.v1.json` shape is project
level, `.devin/hooks.v1.json`). The handler resolves the session by ID
directly against the store, runs the shared policy, and emits
`hookSpecificOutput.additionalContext` recommending `/compact` when over
trigger. It is advisory only and fails silently, so a broken hook never
blocks a prompt. `gobstopper uninstall-hooks` removes only Gobstopper-owned
commands.

**ACP caveat:** hooks fire in the interactive `devin` TUI; Devin's ACP
server mode (`devin acp`, e.g. under Windsurf) does not run lifecycle hooks
— verified empirically. For ACP sessions the equivalent levers are the MCP
`policy_check` tool and a global-rules entry in
`~/.config/devin/AGENTS.md` instructing the agent to run
`gobstopper policy-check --provider devin --session current --json` before
substantive turns and obey `provider_compact` by running `/compact`.

The same `install-hooks` run installs the Claude Code `UserPromptSubmit`
advisor (`gobstopper hook prompt-policy:claude`) into
`~/.claude/settings.json` alongside the existing `PreCompact` and
`SessionStart` hooks — one policy engine, one installer, per-provider
session resolution.

## Rollout gating

`[rollout]` in `~/.config/gobstopper/config.toml` gates the advisory per
provider with deterministic session bucketing:

```toml
[rollout]
devin = 50        # half of Devin sessions get the advisory (treatment)
claude_code = 50  # the other half stays silent (control)
```

The bucket is `int(sha256(session_id)[:16], 16) % 100` — stable across
prompts and machines. Every resolved decision is appended to the telemetry log as a
`prompt-policy:treatment|control` event — numerator and denominator for an
A/B readout both land in `events.jsonl`, and `gobstopper report` /
`scripts/monitor.py --provider <name>` supply the per-session context
trajectories to compare cohorts.

## Read-only MCP

```sh
devin mcp add -s user gobstopper -- gobstopper mcp
devin mcp get gobstopper
```

Use `-s project` for checked-in `.devin/mcp_config.json`, or omit `-s` for the
gitignored local `.devin/mcp_config.local.json`. The server exposes only
read-only tools. `policy_check` supports Devin; `plan`/`verify`/`detect` cover
Devin sessions discovered from the store.

## Export inspection

`devin --export out.json` writes ATIF, a whole JSON document rather than JSONL.
A provider plugin can inspect it without granting mutation authority:

```sh
gobstopper plugin check /absolute/path/gobstopper-plugin.json
gobstopper plugin inspect /absolute/path/gobstopper-plugin.json \
  --trusted-sha256 <manifest-sha256> \
  --provider devin-atif \
  --source /absolute/path/out.json
```

Provider-read plugins receive bounded source content and return normalized
logical items plus usage. Provider inspection cannot return edits.

## Resume integrity

Use Devin's own `--resume <session-id>` or `--continue` controls. Gobstopper
does not synthesize Devin session IDs, restore exports into the session store,
or claim that an exported trajectory can replace provider-owned resume state.

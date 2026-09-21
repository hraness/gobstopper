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

— into `~/.config/devin/hooks.v1.json` (Devin's flat event-map shape, no
`"hooks"` wrapper). The handler resolves the session by ID directly against
the store, runs the shared policy, and emits `hookSpecificOutput.
additionalContext` recommending `/compact` when over trigger. It is advisory
only and fails silently, so a broken hook never blocks a prompt.
`gobstopper uninstall-hooks` removes only Gobstopper-owned commands.

The same `install-hooks` run installs the Claude Code `UserPromptSubmit`
advisor (`gobstopper hook prompt-policy:claude`) into
`~/.claude/settings.json` alongside the existing `PreCompact` and
`SessionStart` hooks — one policy engine, one installer, per-provider session
resolution.

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

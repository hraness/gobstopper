# Devin integration

Gobstopper integrates with Devin through its local session store and documented
control-plane surfaces. It reads Devin's private session database for
inspection and planning; it never writes to it, and it never injects input into
another process.

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
`devin: /compact`. For an **idle** session, custom strategies (`auto`,
`middle`, …) produce elision plans against the exported form. Applying those
edits to `sessions.db` is not yet implemented — `apply`/`compact`/`fork` refuse
store-backed mutation until a locked, transactional, WAL-aware write path with
snapshot and verification exists. Detached export files can be processed and
evaluated directly.

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
`scripts/devin-prompt-policy.py` reads the hook payload, calls
`policy-check --provider devin --session <id>`, and emits `additionalContext`
recommending `/compact` when over trigger. It is advisory only and fails
silently, so a broken hook never blocks a prompt.

Install:

```sh
install -m 0755 scripts/devin-prompt-policy.py ~/.config/gobstopper/
```

`~/.config/devin/hooks.v1.json`:

```json
{
  "UserPromptSubmit": [
    {
      "matcher": "",
      "hooks": [
        {
          "type": "command",
          "command": "python3 ~/.config/gobstopper/devin-prompt-policy.py",
          "timeout": 10
        }
      ]
    }
  ]
}
```

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

# Devin integration

Gobstopper integrates with Devin through its local session store and documented
control-plane surfaces. It reads Devin's session database for inspection and
planning, and it can rewrite payload bytes in place for **idle** sessions
through a locked, transactional write path. Gobstopper leaves a live session
to Devin: it recommends Devin's `/compact` and never sends input to another
process.

## Session discovery

Devin stores sessions in a SQLite database:

- `$DEVIN_DATA_DIR/sessions.db`
- `$XDG_DATA_HOME/devin/cli/sessions.db`
- `~/.local/share/devin/cli/sessions.db`

`gobstopper detect` lists Devin sessions alongside Codex and Claude Code.
Sessions are resolved by ID, ID prefix, or title. Pass the session ID, never
the `sessions.db` path: the database is a shared store, not a transcript file.

A session is **active** while its `session_locks/<id>.lock` file is flock-held
by a live Devin process. For an active session, Gobstopper plans a handoff to
Devin's own compaction and never proposes a local rewrite.

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
`eval` consumes. Hashing or snapshotting the shared database would mix
unrelated sessions, so the canonical per-session bytes are the unit of record.

For an **active** session, `plan` returns `provider_compact` with control
`devin: /compact`; `apply` refuses file surgery while the provider holds the
session lock. For an **idle** session, custom strategies (`auto`, `middle`, …)
produce elision plans, and `apply` performs the guarded store write below.
`fork` refuses store-backed sessions, because a detached copy is not
something Devin can resume. Detached export files can be processed and
evaluated directly.

## Guarded store mutation

`gobstopper apply <session-id>` on an idle Devin session rewrites the session
in place:

1. The canonical per-session export is snapshotted into the vault. The shared
   database file is never the snapshot unit, because unrelated sessions live
   in it.
2. The session lock is acquired; a locked session aborts before any write.
3. One SQLite transaction updates elided `chat_message` payloads with
   conditional identity checks, inserts a Gobstopper digest node on the main
   chain, and moves `sessions.main_chain_id` to it.
4. The post-write canonical export is re-derived and verified (chain linkage,
   tool-call pairing) before commit is considered final.

`gobstopper undo` restores the snapshot: original message payloads and chain
head are written back and Gobstopper-injected digest nodes are deleted. Undo
refuses if Devin appended nodes after the snapshot, so a restore never orphans
provider state that Gobstopper did not create. Undo also refuses while Devin
holds the session lock.

`watch` can perform this write automatically for idle Devin sessions that
cross the trigger, but only when you opt in:

```toml
[provider.devin]
auto_apply_store = true
```

and only under a provider-scoped watch (`gobstopper watch --provider devin`)
so Codex/Claude fork preparation is unaffected. Live sessions still defer to
`/compact` regardless of the flag. When a `[rollout]` entry exists for the
provider, only the treatment cohort is auto-applied; control sessions log a
`watch-apply:control` skipped event per content version so the experiment
keeps its denominator.

Watch caches a per-session fingerprint (chain head and node count for Devin,
file length and mtime for JSONL, plus the live/idle bit) after every terminal
decision (apply, terminal plan failure, provider delegation, or no plan) and
skips the transcript load while it is unchanged. This matters because
store-reported context stays stale after a Gobstopper write; without the
fingerprint every pass would re-plan an unchanged session. Claude in-place
rewrites additionally require the fingerprint to hold across two
consecutive passes before writing, which closes the
`ChangedDuringWrite` race window that mtime-only idle detection leaked.
Sessions that are still live skip the transcript load entirely: `auto`
delegates to the provider for active sessions unconditionally, so the
loop emits the delegation decision straight from the cheap usage read.
Two further bounds keep the loop cheap and fair: a successful in-place
mutation holds the session out of evaluation for `apply_hold_secs`
(default 1800; a session that re-appends and re-triggers inside that
window is churning, not accumulating), and each pass services sessions
in ascending size order so one multi-minute apply cannot delay every
session behind it.

The suppression fingerprints, settle arms, and rate-limit clocks persist
to `~/.local/share/gobstopper/watch-state-<provider>.json` after every
pass (atomic write, clocks as epoch seconds, entries older than a day
dropped on load), so restarting the daemon does not re-plan every
session once. Per-provider state files mean concurrent `--provider`
watches never share a file. Delegation decisions log only when the
session's context moved; otherwise a live session appended to every
pass would emit an identical `provider_compact/skipped` record each
interval.

Long-running sessions can exceed the default 512 MiB transcript limit.
Raise it with `GOBSTOPPER_MAX_TRANSCRIPT_BYTES` (bytes) in the watch
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
store. That skips the full discovery scan, so it is fast enough for hooks:

```sh
gobstopper policy-check --provider devin --session <session-id> --json
```

The session argument accepts an exact ID, an ID prefix, or a title
substring, the same resolution `detect` and `plan` use, so
`--session scarlet-gemini` works as well as a raw store ID.

`--session current` resolves the active session bound to the caller's
working directory: it intersects the store's `working_directory` with the
flock-held `session_locks/*.lock` set and prefers the longest matching
directory, then the most recently active. It refuses to guess when locked
sessions are ambiguous. The in-agent rule and the TUI hook below rely on it:

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

The caller runs `/compact` inside the Devin session. Gobstopper's own store
writes refuse a session whose lock Devin holds at the time of the check.

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
`gobstopper install-hooks` registers this callback in
`~/.config/devin/config.json` under the `"hooks"` key:

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

That file is Devin's documented user-level location; the flat
`hooks.v1.json` shape is for a project, in `.devin/hooks.v1.json`. The
handler resolves the session by ID directly against the store, runs the
shared policy, and, when the session is over its trigger, emits
`hookSpecificOutput.additionalContext` telling the model that `/compact` is a
host-level command and that it should recommend compaction to the person
running the session. It is advisory only and fails silently, so a broken hook never
blocks a prompt. A repeat throttle keeps the advisory from re-entering
every prompt of a session that stays over trigger: after one is shown,
the next advisory waits until the session's context grew by ≥25k tokens
or ≥20 minutes passed (checked against the telemetry log tail, so it
costs a bounded read per prompt, not a state file).
`gobstopper uninstall-hooks` removes only Gobstopper-owned commands.

The same install registers `PostCompaction` (`gobstopper hook
postcompact`, timeout 60). Devin's hook enum has no `PreCompact` event,
so the handler runs after the provider's own compaction: it appends a
`native/provider_compact/applied` telemetry record and snapshots the
session's canonical export into the vault for provenance (a read-only
export on the provider-held store; WAL readers don't contend with the
session lock). Telemetry lands before the snapshot attempt so a timeout
kill on a giant session still records the event.

Hooks fire in the interactive `devin` TUI. Devin's ACP server mode
(`devin acp`, used by Windsurf, for example) does not run lifecycle hooks; we
confirmed this by testing. For ACP sessions the equivalent advisory levers are
the MCP `policy_check` tool and a global-rules entry in
`~/.config/devin/AGENTS.md` instructing the agent to run
`gobstopper policy-check --provider devin --session current --json` before
substantive turns and obey `provider_compact` by running `/compact`.

The ACP bridge does interpret `session/prompt` text that matches an
advertised command, so `watch` can drive provider-native compaction on
idle sessions: `devin acp` → `session/load` → `session/prompt "/compact"`
runs the provider's own `file_compactor`. In our tests a summary node lands
on the main chain, while print mode (`-p "/compact"`) does nothing.
Enable it per provider with `auto_compact_closed = true`. When the flock
check says the session is idle and the rollout cohort is treatment, watch
runs the ACP compact. If it fails or does not confirm completion, watch
leaves the session unchanged and does not fall back to the store write.
Live (locked) sessions are skipped.

Protocol notes, observed on the wire: requests must be serialized, because
a `session/prompt` sent before `session/load` resolves reaches an unloaded
session (`-32002 "Session not found"`). The prompt reply is only an ack;
the compaction runs asynchronously and reports
`_cognition.ai/compaction` notifications (`started` → `completed` with
the summary text, or a failure status) plus a `Context compacted`
display message. Dropping the client at the ack aborts the in-flight
compaction, so `acp_compact` holds the session until a terminal status
arrives. The `_meta` in the `session/load` result carries `isLocked` and
`lockHolderPid`, which say whether another process holds the session (ACP
clients hold sessions without flock), and a held session aborts before
`/compact`.
The lock is process-exclusive: `session/load` on a session another
client holds fails outright with `-32015 "already open in another
process (PID …)"`, so a second client cannot reach a live session even
before `/compact` is considered. A compaction that started but never
confirmed is left untouched rather than falling back to store mutation.
`completed` is also possible when the provider's compactor finds nothing
to do, so watch checks `context_tokens` afterward and records a `skipped` /
`provider_noop` event rather than a false `applied`.

The same `install-hooks` run installs the Claude Code `UserPromptSubmit`
advisor (`gobstopper hook prompt-policy:claude`) into
`~/.claude/settings.json` alongside the `PreCompact` and `SessionStart`
hooks. Both providers use the same policy engine and installer, with
per-provider session resolution.

## Rollout gating

`[rollout]` in `~/.config/gobstopper/config.toml` gates the advisory per
provider with deterministic session bucketing:

```toml
[rollout]
devin = 50        # half of Devin sessions get the advisory (treatment)
claude_code = 50  # the other half stays silent (control)
```

The bucket is `int(sha256(session_id)[:16], 16) % 100`, which is stable
across prompts and machines. Every resolved decision is appended to the
telemetry log as a `prompt-policy:treatment|control` event, so the numerator
and denominator for an A/B readout both land in `events.jsonl`.
`gobstopper events --cohort` aggregates telemetry per provider per rollout
arm (sessions, advisories shown and suppressed, watch cohort skips, applies,
and reclaimed tokens), recomputing each session's deterministic bucket rather
than trusting event tags. `--since 24h` windows the readout (so pre-gate data does not
pollute post-gate reads) and `--json` emits the same table for tooling. `gobstopper report` /
`scripts/monitor.py --provider <name>` supply the per-session context
trajectories to compare cohorts.

## Read-only MCP

```sh
devin mcp add -s user gobstopper -- gobstopper mcp
devin mcp get gobstopper
```

Use `-s project` for checked-in `.devin/mcp_config.json`, or omit `-s` for the
gitignored local `.devin/mcp_config.local.json`. The server exposes only
read-only tools. `policy_check` supports Devin, and `list_sessions`, `plan`,
and `verify` cover Devin sessions discovered from the store.

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
does not synthesize Devin session IDs or replace the provider's complete resume
state with an exported trajectory. Its guarded `undo` path does restore selected
payloads from a canonical per-session vault export into the existing store and
rejects a foreign session identity. The idle-lock probe is not retained through
that transaction, and restore does not bind an expected current export; these
remaining ownership and drift risks are tracked in the
[correctness audit](correctness-audit.md) and [plan](correctness-plan.md).

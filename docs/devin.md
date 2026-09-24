# Devin integration

Gobstopper integrates with Devin through its local session store and documented
control-plane surfaces. It reads Devin's session database for inspection and
planning. Policy can recommend `/compact` to the existing session owner, but
released CLI native dispatch is blocked pending qualification, even with
`auto_compact_closed = true`. Direct-store apply and restore are disabled:
observed idleness and a SQLite transaction do not establish lifetime provider
custody. See the [activation matrix](assurance/qualification.json) and
[recovery runbook](assurance/operations.md).

## Session discovery

Devin stores sessions in a SQLite database:

- `$DEVIN_DATA_DIR/sessions.db`
- `$XDG_DATA_HOME/devin/cli/sessions.db`
- `~/.local/share/devin/cli/sessions.db`

`gobstopper detect` lists Devin sessions alongside Codex and Claude Code.
Sessions are resolved by ID, ID prefix, or title. Pass the session ID, never
the `sessions.db` path: the database is a shared store, not a transcript file.

A held `session_locks/<id>.lock` is one signal of an **active** session.
ACP ownership can also be reported by the provider. A missing or momentarily
unheld file does not prove custody for a later operation. Sessions observed as
active receive native-delegation plans.

## Inspection and planning

```sh
gobstopper detect --json
gobstopper plan <session-id> --json
gobstopper verify <session-id> --json
gobstopper eval <session-id> --json
gobstopper export <session-id> > session.jsonl
```

`export` writes a deterministic canonical JSONL: a `session_meta` record
followed by `message_node` records for the session's main conversation chain.
The export is what `verify` checks (chain linkage, tool-call pairing) and what
`eval` consumes. Hashing or snapshotting the shared database would mix
unrelated sessions, so the canonical per-session bytes are the unit of record.
`eval` and `bench` capture that export once and run every strategy against the
same bytes, with an exact source hash. Neither command changes the database.
Benchmark rows preserve failed discovered sessions with an explicit failure
category and unavailable measurements; they do not silently remove those
sessions from coverage.

For an active session, `plan` returns `provider_compact` with control
`devin: /compact`. Idle
sessions may still produce detached elision plans for evaluation, but direct
`apply` and `undo` refuse before snapshots, confirmation or database writes.
`compact`/`fork` also refuse store-backed sessions: a detached canonical export
is not a resumable Devin artifact. Export bytes remain available to pure
transforms, structural verification and isolated evaluation.

## Provider custody and compatibility

The old direct-store path released its flock probe before starting its SQL
transaction. A provider could load the session between those steps; same-ID
foreign stores and changed graphs also weakened restoration identity. No
qualified provider-compatible lifetime lock was established, so direct writes
are disabled rather than relying on those checks.

Existing `auto_apply_store` and `auto_apply_inplace` configuration fields remain
accepted for compatibility. They cannot enable direct mutation. A matching
watch attempt is blocked with `custody_unavailable`; existing configuration is
preserved. Native compaction has its own opt-in and qualification obligations,
and a failed or uncertain native attempt never falls back to file surgery.

Watch suppression and native outcome reconciliation are documented in the
[recovery runbook](assurance/operations.md). A liveness marker, unchanged fingerprint,
or elapsed cooldown alone does not establish ownership or prove that an earlier
remote operation did nothing.

Large exports default to a 512 MiB bound, configurable with
`GOBSTOPPER_MAX_TRANSCRIPT_BYTES`. Oversized input is refused without truncation.

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
sessions are ambiguous:

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

The caller executes `/compact` inside its owned Devin session. This advisory
path does not itself write the session store.

Configure Devin independently from Codex and Claude Code:

```toml
[provider.devin]
trigger_tokens = 200_000
floor_tokens = 40_000
min_savings_tokens = 4_096
```

`floor_tokens` and `min_savings_tokens` remain useful shared policy metadata,
but Devin controls the actual post-compaction result.

## Hook settings candidates

Devin's `UserPromptSubmit` hook can advise compaction before each turn.
`install-hooks` and `uninstall-hooks` export settings candidates without
changing Devin or Claude settings. The destination must be a new file in an
existing directory:

```sh
gobstopper install-hooks --output ./hook-candidates.json
gobstopper uninstall-hooks --output ./hook-removal-candidates.json
```

Each bundle retains the original settings bytes and SHA-256 plus the candidate
and its SHA-256. Keep it private. Candidate generation preserves unrelated
settings and handlers; removal recognizes exact Gobstopper-owned entries.
Review any change through provider-owned settings controls, retain trust
prompts, and check the source precondition there. Blindly copying a candidate
over settings is not a supported transaction.

The Devin candidate targets `~/.config/devin/config.json` and proposes
`UserPromptSubmit` (`gobstopper hook prompt-policy:devin`, timeout 10) and
`PostCompaction` (`gobstopper hook postcompact`, timeout 60) handlers. The prompt
handler resolves the exact session, runs shared policy and can advise the
operator to use `/compact`. It does not execute that command. A shown advisory
is throttled until context grows by at least 25k tokens or 20 minutes pass.
Malformed or unresolvable input produces no advisory.

`PostCompaction` archives a source-bound canonical export as an observation.
It has no operation ID and cannot establish a corresponding before-state,
causality or token savings. Its event uses `action: "none"`,
`outcome: "skipped"`, and `error_code: "unattributed_provider_hook"`.
Duplicate and out-of-order callbacks do not become applied operations.

Earlier Devin TUI/ACP trials observed different hook delivery. Those historical
observations do not qualify hook execution in an installed provider build. MCP
`policy_check` remains an advisory alternative. An owner acting on that advice
must establish its own provider control and outcome contract.

## Guarded native adapter

The lower-level adapter implements a bounded serialized ACP flow:
`devin acp` → initialize → `session/load` → `session/prompt "/compact"`.
It rejects a reported lock holder before submitting the prompt. A prompt reply
is an acknowledgement, not a terminal result. The client processes framed
notifications during reply waits and accepts a matching session's
`_cognition.ai/compaction` terminal status; arbitrary `Context compacted`
display text is not success evidence. Deadline, record and aggregate output
limits cover owned process cleanup.

These protocol fixtures do not establish that a concurrent provider cannot
open the same session or that the proprietary provider retains ownership.
No released Gobstopper CLI path activates the adapter. Existing opt-ins cannot
override `native_unqualified`. A failed or uncertain attempt never falls back
to database mutation, and a durable unknown operation is not retried after a
cooldown, source change or restart. Devin's session-only terminal contract
cannot satisfy the exact Codex terminal evidence accepted by
`native-reconcile`; weaker evidence remains blocked.

Even a matching completion does not establish reduction. Watch must compare
source-bound provider observations; zero, reset or missing usage remains
unresolved. Structural checks, lexical retention and a successful provider exit
cannot substitute for a reported same-source reduction or task continuation.

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
`scripts/monitor.py --session <exact-id> --provider devin` supply bounded context
observations; the provider flag broadens collection to active Devin sessions.
Cohort event counts are not a randomized task-quality result, and unattributed
hooks receive no applied credit. See the [monitor contract](../scripts/monitor.md).

## MCP inspection

```sh
devin mcp add -s user gobstopper -- gobstopper mcp
devin mcp get gobstopper
```

Use `-s project` for checked-in `.devin/mcp_config.json`, or omit `-s` for the
gitignored local `.devin/mcp_config.local.json`. The server exposes inspection
tools with deterministic built-ins; it rejects executable strategy selection
and does not call plugins or model scorers. `policy_check` supports Devin;
`plan`, `verify` and `list_sessions`
cover Devin sessions discovered from the store.

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

Use Devin's own resume controls on provider-owned data. A canonical per-session
export is an inspection and archive format, not a complete resumable Devin
store. Gobstopper does not synthesize a replacement session identity or restore
exported payloads into the database. `apply`, `undo`, `compact` and `fork`
refuse store-backed mutation before snapshots or writes. Existing SQL helper
entry points also refuse direct writes.

The [dialect checks](../verify/transcript/README.md),
[vault models](../verify/vault/README.md) and
[native control model](../verify/watch/README.md) cover declared structural,
recovery and no-replay properties. They do not prove current provider
continuation behavior or universal preservation of task facts.

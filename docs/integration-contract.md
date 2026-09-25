# Historical integration contract

This records the proposed numeric interface for the session runtime that
preceded xcb, retired on 2026-09-19. The owner obligations below still apply to
any runtime that integrates Gobstopper: Gobstopper evaluates policy, while the
runtime retains provider-process custody and must qualify any operation it
executes. A policy response transfers neither ownership nor mutation authority.
This document does not establish that xcb or another runtime meets those
obligations.

## Policy input → `gobstopper policy-check`

Supply the provider's current context observation and session-active state:

```sh
gobstopper policy-check \
  --provider codex \
  --context-tokens 300000 \
  --session-active \
  --quota-pressure normal \
  --json
```

With the default policy, an over-trigger result includes:

```json
{"action":"provider_compact","strategy":"auto","trigger_tokens":250000,"control":"thread/compact/start"}
```

This is an advisory result. A runtime should use a reported context observation,
not substitute a lifetime counter or missing value. `--quota-pressure` accepts
`low|normal|high`, defaults to `normal`, and adjusts the effective trigger by
1.15/1.0/0.7 respectively. Layered policy can change the threshold or decision.
This numeric invocation does not inspect a transcript or call a provider.

## Proposed effect path → `session.compact` in the owner

An owner-side integration needs these obligations before activation:

| Boundary | Required owner behavior |
|---|---|
| Identity | Bind provider, exact session, provider generation, home and binary/protocol version. |
| Admission | Establish retained provider-compatible ownership; an idle observation is insufficient. |
| Receipt | Persist intent and recovery evidence before dispatch; persist dispatch before calling the provider. |
| Codex | Qualify `thread/compact/start` on the owner's app-server connection and correlate session/turn/compaction item terminal IDs. |
| Claude | Qualify the selected owned-session `/compact` protocol; historical stream-json trials are not current qualification. |
| Uncertainty | A lost acknowledgement, timeout or uncorrelated result stays unknown; do not replay automatically. |
| Measurement | Capture source-bound before/after usage separately from protocol completion and task continuation. |

Gobstopper's released CLI refuses all native dispatch pending these live
qualification obligations, including previous `auto_compact_closed` opt-ins
and standalone native-fork requests. Its lower-level adapter methods are
explicit caller-owned primitives, not qualified unattended entry points.
The [activation matrix](assurance/qualification.json) has no qualified live
native cells. A separate runtime cannot treat Gobstopper fixture success as its
own qualification.

The CLI's native journal enforces no automatic unknown replay for cooperating
callers sharing its data root. `native-operations` is read-only inspection;
`native-reconcile` accepts only already persisted matching Codex terminal IDs.
It makes no provider call and accepts no operator-supplied success flag. See the
[recovery runbook](assurance/operations.md) and
[bounded control model](../verify/watch/README.md).

## Telemetry → `gobstopper/compaction-events-v1`

Telemetry appends best-effort JSONL to
`$XDG_DATA_HOME/gobstopper/events.jsonl` (default
`~/.local/share/gobstopper/events.jsonl`). It is an observation log, not the
durable operation journal. Failed telemetry does not roll back a completed
effect. Numeric counts, closed codes and identifiers are allowed; transcript
content, paths and raw provider messages are not.

The additive source fields distinguish comparable evidence:

- `source_identity_sha256` binds canonical provider/store/session identity.
- `snapshot_before_sha256` and `snapshot_after_sha256` identify retained vault
  objects. `before_observation` and `after_observation` bind accounting to those
  exact source bytes and snapshots.
- Context state is `reported`, `absent`, `unknown` or `reset`; lifetime scope is
  `full`, `partial` or `absent`. Legacy numeric estimates do not qualify as
  observed savings. A missing or reset context is not a zero-token success.
- `retention_total`, `retention_retained` and `retention_lexical` describe bounded
  heuristic checks against an exact before-state. Missing fields mean
  unmeasured. Literal presence and lexical coverage do not establish semantic
  correctness, permission to act, task success or billing savings.

Only a comparable retained same-source pair can qualify a recorded context
reduction. Report joins include source identity, so equal session IDs in foreign
stores cannot merge. Retention rollups count each exact before/after snapshot
pair once; conflicting source hashes or counts disqualify that pair. Hook
callbacks have session identity but lack operation
correlation; they record `action: "none"`, `outcome: "skipped"` and
`error_code: "unattributed_provider_hook"`. They supply no applied credit,
paired retention or causal savings. Consumers must preserve those distinctions.

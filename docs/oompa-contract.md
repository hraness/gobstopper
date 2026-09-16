# oompa integration contract

What oompa implements to drive gobstopper policy on live managed
sessions, and what gobstopper provides in return. The seam is numeric:
oompa never parses provider transcripts and gobstopper never owns
provider processes.

## Policy input → `gobstopper policy-check`

oompa already persists `token_usage` session events
(`totalTokens`, `modelContextWindow`) through one choke point
(`#persistSessionEventWrites`). The integration is a pure evaluation at
that boundary — or, for a zero-daemon-change start, an external watcher
on `oompa session events --jsonl`.

```
gobstopper policy-check \
  --provider codex \
  --context-tokens 231400 \
  --session-active \
  --quota-pressure normal \
  --json
→ {"action":"provider_compact","strategy":"auto",
   "trigger_tokens":250000,"control":"thread/compact/start"}
```

`--quota-pressure` is additive: `low|normal|high`, default `normal`,
multiplies the effective trigger ×1.15/×1.0/×0.7. Sources: the provider's
`account/rateLimits/updated` signal or an operator setting.

## Effect path → `session.compact` (oompa-side)

| step | oompa-side work |
|---|---|
| command | new `LocalCommand` variant `session.compact { session_id, trigger_tokens, strategy? }` |
| receipt | record mutation before dispatch; idempotency key; exact provider-generation binding |
| codex | add `thread/compact/start` to `CodexMethod` + `OPERATIONS` (effect class, deadline, lost-response policy per `src/codex/AGENTS.md`); call on the daemon's own `CodexAppServerClient` |
| claude | `--autocompact <tokens>` in `buildPinnedClaudeRuntimeArgv` (argv, not env); optional `/compact` steering write if stream-json admits it |
| evidence | route `thread/compacted` (currently `ignored`) into the timeline; record a `compaction` session event `{trigger, strategy, pre_tokens}` |
| reconcile | uncertain compact results reconcile like every other mutation — no speculative replay |

gobstopper stays outside oompa's trust boundary: the CLI is invoked as a
subprocess, JSON in/out, or vendored as a pure-policy Rust dependency
later — never as a file-level component inside the daemon.

## Telemetry back → `gobstopper/compaction-events-v1`

Every compaction decision or outcome appends one JSONL record
(`~/.local/share/gobstopper/events.jsonl`):

```json
{"schema":"gobstopper/compaction-events-v1","ts":175...,"provider":"codex",
 "session_id":"…","strategy":"auto","action":"provider_compact",
 "outcome":"applied","trigger_tokens":250000,"context_tokens_before":231400,
 "context_tokens_after":41200,"est_reclaimed_tokens":190200,
 "items_covered":38,"duration_ms":1220,"error_code":null}
```

Consumers: aicharts (occupancy-over-time + savings dashboards; resolves
the cumulative-counter-regression ambiguity), oompa (timeline evidence).
Numeric only — no transcript content, no freeform error text.

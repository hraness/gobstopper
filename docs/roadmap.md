# gobstopper roadmap

gobstopper is the context-compaction layer for the Hraness agent stack.
This document is the engineering map: what exists, what comes next, and
how the work shares foundations with **oompa** (session control plane),
**aicharts** (usage measurement + evidence), **textbutler** (local agent
runtime + skill distribution), and **agentrouter** (brokered agent task
execution, inside the textbutler repo).

---

## 0. The stack picture

```
                    ┌─────────────────────────────────────────┐
 measurement  ────► │ aicharts                                 │
 (prove savings)    │  usage ledger · AICU wire · dashboards   │
                    └───────────────▲─────────────────────────┘
                                    │ compaction events,
                                    │ context-occupancy series
 ┌──────────────┐   policy-check    │        ┌────────────────────────┐
 │   oompa      │ ◄──────────────►  │        │      gobstopper        │
 │ control plane│   (numbers→action)│        │  transcript surgery ·  │
 │ owns live    │                   │        │  strategy engine ·     │
 │ sessions     │ ──executes native─►        │  watch daemon          │
 └──────────────┘   compact on its  │        └───────────▲────────────┘
                    own connections │                    │ EditorDriver
 ┌──────────────┐                   │        ┌───────────┴───────────┐
 │ agentrouter  │ ── editor model ──►────────►│  (capability profile: │
 │ (textbutler) │    task runtime   │        │   keep/elide/summarize│
 └──────────────┘                   │        │   /defer)             │
                                    │        └───────────────────────┘
 shared: transcript-foundation crate (codex/claude JSONL dialects,
         normalized items, usage extraction) — read side shared with
         aicharts, write side owned by gobstopper
```

## 1. What exists (v0.1, shipped)

- Session detection across `~/.codex/sessions` and `~/.claude/projects`
  (content sniffing, not filename guessing; custom roots for managed
  homes).
- Usage extraction from provider records (`token_usage_record`,
  `message.usage`) — context occupancy + lifetime burn, no transcript
  content needed.
- `Edit` IR (`Elide` / `InjectDigest` / `ProviderCompact`), strategies
  `auto | sawtooth | elide | structured | agentic(scaffold)`, layered
  config (`policy → provider → preset → session`), userspace `command`
  presets, `policy-check` JSON seam, `watch` loop.
- Verified against real session files: in-place elision preserves Claude
  `parentUuid` chains and Codex `ordinal` order; live-branch-only
  accounting for Claude's tree-shaped transcripts; `compacted` records'
  `replacement_history` covered.

## 2. Landscape position (why this is a real niche)

Cross-provider auto-compaction daemons exist only as **proxies**
(Headroom, Compresr, kompact) — they shrink the wire but leave the
on-disk transcript bloated, so resume/fork still pays full context.
File-layer tools (cc-session, coldxx, claude-journal, compactdiff,
claude-streaming-compactor) are manual or single-provider/single-
strategy. Nobody composes: watch → policy → transcript surgery →
pluggable strategies → measurement. gobstopper's moat is the surgery
layer plus the eval harness (§6).

Adjacent risks noted for honesty: undocumented JSONL drift (mitigate via
`verify` + provider-native delegation), prompt-cache invalidation on
rewrite (mitigate via polling-cadence + boundary-timed compaction), and
provider-native compaction improving (Anthropic `compact_20260112`,
Codex `ResponsesCompactionV2`) — gobstopper treats those as delegates,
not competitors.

## 3. Phase A — surgery correctness & reversibility (v0.2, shipped)

The durable core is safe JSONL surgery. Before scaling strategies, make
the write path bulletproof and undoable.

- ✅ `gobstopper verify <file>`: resume-validity checker — orphaned
  `tool_use`/`tool_result` pairs, broken `parentUuid` chains, malformed
  `compacted` records, torn tail lines. Exit 1 on errors, `--json` for
  integrators.
- ✅ **Content-addressed vault** (`~/.local/share/gobstopper/vault/`):
  every `apply`/`watch` snapshots the pre-edit transcript by digest;
  `gobstopper undo <session>` restores (and snapshots the compacted
  state first — undo is itself undoable). Nobody else offers "undo a
  compaction." The vault doubles as the eval corpus (§6).
- ✅ **Fork-on-write**: `gobstopper fork <session>` clones the transcript
  under a fresh session id (`sessionId`/`session_meta` rewritten, chains
  preserved) and prints the provider resume command. Default posture for
  anything risky.
- ✅ **Telemetry**: every mutating path appends a
  `gobstopper/compaction-events-v1` record to `events.jsonl` (§4, §6).
- ✅ **Quota pressure**: `policy-check --quota-pressure low|normal|high`
  scales the effective trigger ×1.15/×1.0/×0.7 — the input agentrouter's
  `rateLimits/updated` signal feeds.
- ✅ **Hook installers**: `gobstopper install-hooks` merges
  `PreCompact`/`SessionStart(source=compact)` entries into Claude
  `settings.json` and Codex `hooks.json` (verified supported on the
  pinned 0.153.2; requires one-time `/hooks` trust approval). Hook
  callbacks snapshot to the vault, append events, and return an
  `additionalContext` restore pointer after native compaction.
- ⏳ Codex `compacted`-record writer: emit a real `compacted` record with
  custom `replacement_history` (window chain fields understood:
  `window_id`, `first_window_id`, `previous_window_id`,
  `guardian_history`, `latest_token_usage_record`). Gated `--experimental`
  until validated against resume.

## 4. Phase B — live control plane (v0.3)

- **Codex app-server attach**: `thread/compact/start` client via
  `codex app-server proxy` (exists, minimal) + `codex agents` discovery
  for daemon-registered sessions.
- **oompa managed sessions** (the primary integration):
  - oompa already records `token_usage` (`totalTokens`,
    `modelContextWindow`) into its neutral timeline; the insertion point
    is `#persistSessionEventWrites` — one `policy-check` call per event,
    or an external watcher on `oompa session events --jsonl`.
  - Effect side: new `session.compact` command →
    `thread/compact/start` on the daemon's own Codex connection;
    Claude gets `--autocompact` in `buildPinnedClaudeRuntimeArgv` or a
    `/compact` steering write. Follows oompa's receipt-before-dispatch +
    idempotency discipline; `thread/compacted` should become a routed
    timeline event.
  - Known gap: Claude's `modelContextWindow` is `null` in oompa events —
    gobstopper's trigger needs an absolute token threshold anyway, so
    this is acceptable, but worth fixing upstream.
- **agentrouter telemetry**: it already parses `tokenUsage/updated`
  incl. `modelContextWindow` (currently dropped — one-line retain) and
  drops `account/rateLimits/updated` (the quota-pressure signal).
  Additive `policy-check` flag `--quota-pressure low|normal|high` lets
  rate-limit state shift the effective trigger.
- **Double-buffer compaction** (Aider/Compresr pattern): at ~60% of
  trigger, build the compacted transcript on a clone in background; swap
  atomically at trigger — zero-stall compaction at the file layer.

## 5. Phase C — the agentic strategy, wired (v0.4)

`agentic`'s `EditorDriver` gets its first real backend:

- **agentrouter `runTask`** with a `CapabilityProfile` exposing exactly
  `keep`, `elide`, `summarize`, `defer` — serialized, bounded, revocable,
  digest-bound tool calls. `calls_to_plan()` already disposes. Ships as a
  `preset.command` Bun shim today (zero Rust integration needed); a
  native driver later.
- **Model selection** via `selectClassifierModel()` semantics — cheapest
  eligible model under a fixed workload; the editor should cost ~1% of
  the tokens it saves.
- **Provider delegates**: Anthropic `compact_20260112` (server-side
  compaction with custom instructions), Codex `compact_prompt` — the
  strategy engine should be able to *steer* native compaction, not only
  trigger it.
- Rubric upgrade: SelfCompact's when-to-compact signals (closed unit,
  summarizable span, progress markers, not-stuck) become first-class
  trigger inputs beyond the flat token threshold — compact at task
  boundaries, not mid-derivation.

## 6. Phase D — measurement as the moat (v0.5, with aicharts)

Nobody ships a compaction eval. gobstopper should.

- `gobstopper eval` — **v1 shipped**: replays a transcript through every
  strategy on temp copies, reports per-strategy savings and post-edit
  `verify` findings. Next: probe-based quality scoring —
  recall/artifact/continuation/decision (Factory-style) + **interaction
  cost** (count re-fetch calls; hidden-cost research shows task metrics
  alone are misleading), over the vault corpus. Publish a leaderboard
  vs. provider-native compaction.
- **aicharts emission**: gobstopper emits `session-observations-v1`
  reports (existing aicharts JSON schema — browser dashboard works
  unchanged) plus compaction events: `{trigger, strategy, pre_tokens,
  post_tokens, items_covered, digest}` — numeric-only, keyed-ID'd per
  aicharts privacy doctrine. This resolves aicharts' known ambiguity
  (`codex_cumulative_regression` can't tell compaction from reset) and
  gives the "did gobstopper actually save tokens" answer a dashboard.
- `Usage.context_tier` in AICU is a reserved field designed for a price
  registry — gobstopper's per-compaction reclaimed-token events are the
  first real producer of occupancy-over-time data.

## 7. Shared foundations — the extraction plan

Three repos now parse the same JSONL dialects: oompa (TS, read-neutral),
aicharts (`aicharts-core`, metadata-only), gobstopper (`-adapters`,
read+write). The hraness rule — extract a shared package once two
consumers need the same interface — is met.

- **`hraness/transcript-foundation`** (new git-pinned crate, same
  distribution pattern as `support-foundation`): normalized
  `SessionMeta`/`Item`/`UsageSample`, dialect sniffing, codex+claude
  readers, live-branch resolution, bounded tail-scan. aicharts adopts
  the read side; gobstopper adopts read+write (surgery stays
  gobstopper-owned).
- **Shared event schema** `hraness/compaction-events-v1`: the numeric
  compaction record all three tools understand — emitted by gobstopper,
  recorded by oompa's timeline, measured by aicharts.
- oompa stays transcript-free by design: it consumes `policy-check`
  numbers and compaction events, never files.

## 8. Distribution & operations

- v0.2+: `cargo install gobstopper-cli` / homebrew tap; signed release
  binaries once `verify` lands.
- Agent-skill packaging (textbutler/oompa marketplace pattern):
  `gobstopper` presets + SKILL.md so any agent session can reason about
  its own context policy.
- Menubar presence later via `hraness/desktop-foundation` (aicharts
  precedent): live context occupancy + last-compaction savings.

## 9. Open questions

- Whether `claude --input-format stream-json` admits `/compact` as a
  steering write — determines if live Claude compaction needs hooks or
  oompa argv only.
- Codex `compacted`-record acceptance rules on resume (window-id chain
  validation) — the single highest-leverage unknown for custom
  strategies on Codex.
- Whether Anthropic server-side compaction (`compact_20260112`)
  becomes a better default than transcript surgery for Claude — decided
  by the eval harness, not by guess.

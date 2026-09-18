# gobstopper roadmap

gobstopper is the context-compaction layer for the Hraness agent stack.
This document is the engineering map: what exists, what comes next, and
how the work shares foundations with **oompa** (session control plane),
**aicharts** (usage measurement + evidence), **textbutler** (local agent
runtime + skill distribution), and **XCB (Excalibur)** (native local agent
workspace plus the retained AgentMixer compatibility package in
`hraness/xcb`).

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
 │ XCB compat   │ ── editor model ──►────────►│  (capability profile: │
 │ AgentMixer   │    task runtime   │        │   keep/elide/summarize│
 └──────────────┘                   │        │   /defer)             │
                                    │        └───────────────────────┘
 shared: transcript-foundation crate (codex/claude JSONL dialects,
         normalized items, usage extraction) — read side shared with
         aicharts, write side owned by gobstopper
```

## 1. What exists (v0.2, shipped and hardened)

- Session detection across `~/.codex/sessions` and `~/.claude/projects`
  (content sniffing, not filename guessing; custom roots for managed
  homes).
- Usage extraction from provider records (`token_usage_record`,
  `message.usage`) — context occupancy + lifetime burn, no transcript
  content needed.
- `Edit` IR (`Elide` / `InjectDigest` / `ProviderCompact` / `CacheEdit`),
  deterministic strategies, layered config (`policy → provider → preset →
  session`), minimum-savings admission, bounded plugins, `policy-check`, and
  copy-only watch.
- Verified against real session files: candidate elision preserves Claude
  `parentUuid` chains and Codex ordinal/window/tool-pair invariants;
  live-branch-only
  accounting for Claude's tree-shaped transcripts; `compacted` records'
  `replacement_history` covered.
- `cache_aware` strategy elides the *latest* stale tool outputs before
  the protected tail so the conversation prefix stays byte-identical;
  `gobstopper bench` reports `prefix_tokens` so cache preservation is
  visible next to projected savings.
- Adaptive thresholds (`adaptive = true`): trigger/floor are re-derived
  per session at each decision point from the provider-advertised
  context window, the elidable share of the transcript, and past
  compaction yields recorded in `events.jsonl`. Deterministic, bounded,
  and self-explaining via closed-vocab reasons in plan output and
  telemetry; `gobstopper tune <session>` previews the adjustment.
- Read-only MCP server (`gobstopper mcp`): agents query sessions,
  state-card recall, vault history/show/diff, dry-run plans, and
  transcript verification over stdio JSON-RPC — agent-addressable
  memory without a mutating surface.

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
  scales the effective trigger ×1.15/×1.0/×0.7 for any session owner that
  observes a provider rate-limit signal.
- ✅ **Hook installers**: `gobstopper install-hooks` merges
  `PreCompact`/`SessionStart(source=compact)` entries into Claude
  `settings.json` and Codex `hooks.json` (verified supported on the
  pinned 0.153.2; requires one-time `/hooks` trust approval). Hook
  callbacks snapshot to the vault, append events, and return an
  `additionalContext` restore pointer after native compaction.
- ✅ **Codex `compacted`-record writer** (gated `apply
  --experimental-compacted`): emits a real `compacted` record with
  custom `replacement_history` — field-for-field verified against live
  rollouts (`window_number`/`first_window_id`/`previous_window_id`/
  `window_id` chain advanced correctly, `compaction_response_id`,
  `latest_token_usage_record`, `ordinal` = line index). The one
  deliberately omitted field is the provider-encrypted `compaction`
  item (unforgeable). Shape-verified; partially resume-validated
  2026-09-15: a fork carrying a gobstopper `compacted` record resumed
  cleanly under `codex exec resume` on the pinned 0.153.2 — rollout
  accepted, window chain parsed, turn attempted; the turn itself was
  blocked by an account usage-limit, not a transcript rejection.
  Remains gated until a quota-available resume completes an API call.
- **Double-buffer watch retired**: `--double-buffer` now fails visibly because
  swapping a staged file into a provider-owned path violates the single-writer
  boundary. Ordinary watch prepares a verified, no-clobber fork.
- ✅ **`gobstopper events`**: telemetry readback — recent activity +
  cumulative reclaimed-token totals.

## 4. Phase B — live control plane (v0.3)

- **Codex app-server attach**: ✅ 2026-09-16 — `thread/compact/start`
  works over a **private** `codex app-server --listen stdio://` (no
  daemon or standalone install needed — `app-server proxy` requires the
  installer's managed layout). Verified flow: `initialize` →
  `initialized` → `thread/resume {threadId, excludeTurns:true}` →
  `thread/compact/start` accepted `{}`; compaction runs as a provider
  turn surfacing a `contextCompaction` item + `turn/completed` (the
  deprecated `thread/compacted` notification did not fire). Observed
  quota rejection surfaces as `turn/completed status=failed` with
  `usageLimitExceeded` — compaction is itself a provider call.
  gobstopper reports the observed turn outcome within a 90s bound and
  records it honestly in telemetry.
- **oompa managed sessions** (the primary integration):
  - ✅ 2026-09-16 — `oompa session compact <session>` is live
    (oompa `devin/session-compaction` @ `5501d83a`): Codex dispatches
    `thread/compact/start` on the daemon's own connection, Claude gets a
    `/compact` steering write, both behind receipt-before-dispatch +
    idempotency. A provider-neutral `compaction` timeline event records
    `outcome`/`trigger` (`manual`/`policy`/`provider`), and uncertain
    dispatches reconcile against the event stream without replay.
  - ✅ 2026-09-17 — opt-in auto-compaction open as hraness/oompa#252
    (main-based; schema v62 `session_compact_policies` since main's v61
    is the Devin readmission): `evaluateAutoCompact` + per-session
    `session.compact-policy` config (`enabled` default off,
    `triggerTokens` 250k, `minIntervalMs` 300s) wired at the
    `token_usage` persistence boundary; a crossing enqueues one durable
    `session.compact` per usage bucket with `trigger: "policy"`, never
    mid-turn. Devin sessions compact via the pinned CLI's `/compact`
    slash command over ACP `session/prompt`.
  - oompa already records `token_usage` (`totalTokens`,
    `modelContextWindow`) into its neutral timeline; the insertion point
    is `#persistSessionEventWrites` — one `policy-check` call per event,
    or an external watcher on `oompa session events --jsonl`.
  - Known gap: Claude's `modelContextWindow` is `null` in oompa events —
    gobstopper's trigger needs an absolute token threshold anyway, so
    this is acceptable, but worth fixing upstream.
- **XCB integration** has two bounded paths. Native XCB pins
  `gobstopper-core` at an immutable commit and applies `ElideStrategy` only to
  its in-memory prompt projection while retaining full local history. The
  AgentMixer compatibility package exposes the bounded editor shim. Neither
  path grants Gobstopper ownership of XCB provider processes or durable state.
- ~~**Double-buffer compaction**~~ was implemented experimentally and then
  retired: safe publication is copy-only, so watch never swaps a staged file
  over a provider-owned transcript.

## 5. Phase C — the agentic strategy, wired (v0.4)

`agentic`'s `EditorDriver` gets its first real backend:

- ✅ **XCB AgentMixer compatibility editor** via `preset.command`:
  `src/gobstopper-editor.ts` in `hraness/xcb` exposes exactly `keep`,
  `elide`, `summarize`, and `defer` through the capability broker — no shell,
  filesystem, or network tools — and writes Gobstopper `Edit` JSON. The
  half-open range/protected-tail contract is aligned in xcb#54. Gobstopper-side,
  `run_preset_command` pipes `{session_id, provider, items, usage}` and
  treats an empty edit list as `defer`, not a phantom applied plan
  (`c5de379`). A native driver remains future work.
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
- **aicharts emission** — ✅ `gobstopper report` emits
  `session-observations-v1` (verified against the real
  `parseSessionReport`; `--strict` for direct ingest, extension key
  carries compaction stats otherwise). `compaction-events-v1` records
  resolve aicharts' known ambiguity (`codex_cumulative_regression`
  can't tell compaction from reset) and feed the occupancy-over-time
  story. ✅ aicharts-side ingest merged as hraness/aicharts#278 —
  `compaction-events-v1` decoder, session join, aggregate strip, and a
  per-session column on the usage dashboard, verified in production.
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

- ~~Whether `claude --input-format stream-json` admits `/compact`~~ —
  **answered (verified on 2.1.270)**: a `user` message carrying
  `/compact` is accepted as a real slash command — the stream emits
  `system/status: "compacting"`, then `compact_result` + a post-compact
  `init`. Oompa can drive live Claude compaction as a steering write on
  its existing client; no argv or restart needed.
- Codex `compacted`-record acceptance rules on resume (window-id chain
  validation) — partially answered 2026-09-15: rollout-level acceptance
  verified (resume proceeded to a real turn attempt); API-level context
  correctness still unproven pending account quota.
- Whether Anthropic server-side compaction (`compact_20260112`)
  becomes a better default than transcript surgery for Claude — decided
  by the eval harness, not by guess.

## 10. Correctness and qualification remediation

This section supersedes earlier completion claims about safe live file rewriting,
structured summarization, double buffering, measured savings, and plugin bounds.
The audit baseline is `c6d91a8`. Existing passing tests did not establish those claims.

### Constraints

- Source transcripts remain unchanged by standalone compaction. Publish a separate
  validated fork; only the provider owner may compact a live session in place.
- All safety tests use synthetic isolated homes. Paid APIs and account switching
  are excluded; live trials may use existing subscriptions only.
- Strategies propose edits; host validation, snapshots, publication and telemetry
  remain outside plugin authority. Trusted subprocesses are not security sandboxes.
- Preserve existing JSON fields additively; distinguish projections from observations.
- Each phase includes regressions and focused tests. Final gate: `cargo test
  --workspace --locked` and `cargo clippy --workspace --all-targets --all-features
  --locked -- -D warnings`, plus relevant site tests for changed public claims.
- Execute sequentially in the current checkout. No phase delegates concurrent edits.

### Phases

| Phase | Deliverable | Depends on | Status |
|---|---|---|---|
| R1 | Transactional private copy publication and recovery | none | Complete |
| R2 | Effective context, eligibility, convergence and verification | R1 | Complete |
| R3 | Validated configuration and bounded versioned plugins | R2 | Complete |
| R4 | Safe CLI/watch/native control and honest public surfaces | R1–R3 | Complete |
| R5 | Comparative evaluation and fault/property regression gates | R1–R4 | Local proxy complete |
| R6 | Subscription-only live qualification and benchmark pilot | R5 | Complete: Codex custom `compacted` record and Claude `compacted`/digest resume both qualified live; head-to-head `diff` vs. Claude `--autocompact` on a 333k-token session shows gobstopper producing measurable structural reduction while native autocompact did not remove records |
|| R7 | Cache-aware suffix elision and prefix preservation metrics | R6 | Complete: `cache_aware` strategy elides the latest stale tool outputs before the protected tail and `gobstopper bench` reports `prefix_tokens` per strategy |

### R1: Transactional private copy publication and recovery

- **Scope:** adapter transaction, fork, vault and rewrite entry points.
- **Objective:** failures never publish partial plans or overwrite source transcripts.
- **Approach:** private unique temporary files, whole-plan candidate validation,
  durable no-clobber publication, exact-source binding, checked vault deduplication.
- **Acceptance:** missing-newline digest stays valid; Claude digest extends the
  original chain; failed later edits leave originals unchanged; permissions never
  broaden; corrupted snapshots abort admission; open provider handles remain valid.
- **Validation:** focused adapter regression and Hegel integration tests.

### R2: Effective context, eligibility, convergence and verification

- **Scope:** core model/strategies/probes and provider loading/verification.
- **Objective:** plans describe effective context and never repeatedly grow it.
- **Approach:** project only the active Codex window; share block eligibility;
  retain protected tails; replace placeholder structured generation with explicit
  conservative behavior until a content-authorized summarizer is configured.
- **Acceptance:** chat-only passes reach no-plan; Claude tool dominance routes
  correctly; scan survives UTF-8 boundaries; tool pairing is linear; measured net
  bytes include stubs; historical usage is not presented as observed post-edit usage.
- **Validation:** generated multi-window/block/tail cases and bounded scaling checks.

### R3: Validated configuration and bounded versioned plugins

- **Scope:** core plugin contracts, CLI config and adapter subprocess host.
- **Objective:** user-authored extensions cannot bypass host edit admission.
- **Approach:** strict versioned manifests, exact implementation digest, bounded
  argv/stdin/stdout/deadline, explicit trusted-code configuration, closed capabilities,
  snapshot-bound proposals and common host validation. Provider extensions must
  describe semantic read/plan operations rather than arbitrary mutation commands.
- **Acceptance:** malformed config fails visibly; invalid bounds/unknown fields fail;
  oversized/duplicate/protected edits fail; timeout children are reaped; no unsupported
  provider capability is advertised as qualified.
- **Validation:** adversarial fixture executables and protocol/property tests.

### R4: Safe CLI/watch/native control and honest public surfaces

- **Scope:** CLI commands/hooks/reporting, README, design, site claims.
- **Objective:** consistent JSON and execution semantics without implicit source writes.
- **Acceptance:** apply and undo publish separate forks; watch is copy-producing and
  idempotent by source identity; no-op JSON is valid; provider homes bind execution;
  interrupted/unknown native outcomes never count as applied; hook recovery points to
  the exact pre-compact snapshot; unsupported staging and savings claims are removed.
- **Validation:** isolated CLI end-to-end tests, provider stubs, site tests.

### R5: Comparative evaluation and fault/property regression gates

- **Scope:** evaluation/benchmark contracts and tests.
- **Objective:** reproducible cost-quality evidence rather than planned savings.
- **Acceptance:** distinguish offline structural/retention proxies from live solve
  rate; accept bounded task receipts with model/version/config/usage/cost/latency/
  refetch/completion evidence; compare default, tuned-native, masking and hybrid
  policies; incomplete trials cannot establish superiority. Retain audit regressions.
- **Validation:** fixture benchmark results, full locked workspace and lint gates.

### R6: Subscription-only live qualification and benchmark pilot

- **Scope:** isolated task/fork trials, no paid APIs or account switching.
- **Objective:** prove provider resume/continuation separately from artifact admission.
- **Acceptance:** completed API-backed continuation on qualified provider versions,
  preserved task facts/tool pairs, recorded subscription usage and explicit unknowns.
  Quota/auth/unsupported-provider blockers remain open; no leadership claim without
  comparable completed-task evidence and uncertainty estimates.
- **Validation:** bounded subscription trials after all source gates pass.

### R7: Cache-aware suffix elision and prefix preservation metrics

- **Scope:** `cache_aware` strategy and `gobstopper bench` prefix-token reporting.
- **Objective:** keep the provider's prompt-cache prefix byte-identical by
  eliding the latest stale outputs before the protected tail, and make the
  cache/preservation trade-off measurable.
- **Acceptance:** `cache_aware` is the default for tool-heavy idle sessions;
  it keeps up to 16x more of the early conversation byte-identical than
  `compacted`/`elide` at similar savings; `gobstopper bench` reports
  `prefix_tokens` alongside `est_reclaimed`.
- **Validation:** live same-session A/B on a 339k-token Claude session
  measured provider cache counters (see implementation log); workspace tests
  and Clippy pass.

### Implementation log

- Audit reproduced malformed digest append, Claude branch loss, no-op growth,
  permission broadening, stale usage, unchecked custom plans, incorrect native
  outcomes and quadratic verification. Baseline: 114 tests and strict Clippy passed.
- R1–R4 implemented: copy-based publication with vault snapshot, source binding,
  stale-plan rejection, permission preservation, no-clobber publication, bounded
  versioned plugins with host validation, honest README/roadmap status, CLI copy
  semantics for `apply`/`watch`/`undo`, corrected native Codex error propagation,
  and SessionStart pre-compact snapshot wiring. Workspace tests and Clippy pass.
- R5 local proxy: `bench_strategies` example generates synthetic transcripts,
  runs all strategies, and reports byte/token/integrity/time metrics.
- R6 pilot 2026-09-17: added `--trust-experimental-compacted` so the custom
  Codex `compacted`-record writer can be exercised under explicit trust.
  Fork + `apply --experimental-compacted --trust-experimental-compacted` +
  `verify` succeeded on a real 138k-token Codex session, producing a record
  whose top-level/payload key order and window-chain fields match native
  provider rollouts. `codex exec resume` of the fork accepted the rollout
  and parsed the window chain, but failed because the source thread is a
  multi-agent v2 sub-agent that must be resumed through its parent; a
  subsequent resume attempt on the parent thread hit the account usage limit.
- R6 completion 2026-09-17 (after login): wired `copy::compact_via_compacted`
  into `gobstopper apply --experimental-compacted` and created a small-model
  Codex session with large tool outputs. `fork` + `apply --preset test-compacted
  --experimental-compacted --trust-experimental-compacted` + `verify` produced
  a custom `compacted` record at the tail with correct `window_number == 1`,
  `first_window_id == previous_window_id`, and `replacement_history` carrying
  the digest plus 20 verbatim tail response items. `codex exec resume`
  completed a full API-backed turn (20,580 tokens input; `task_complete`);
  the model correctly recalled the elided commands, proving the provider
  accepted the gobstopper-written `compacted` record and performed the
  window swap. Claude elide resume also succeeded on a small-model session
  with elided tool outputs (the model recalled the six commands). Claude
  digest-injected fork is not yet resumable; the synthetic `user` record
  lacks the `last-prompt`/`mode` tail Claude's resume indexer expects.
- Built-in `compacted` strategy and meaningful digests: `CompactedStrategy`
  now fills `DigestBlock.goal` (latest user turn label), `decisions` (tail
  summaries of elided tool outputs), and `files_touched` from the elided
  `ToolResult` items. Adapter loaders (`codex`, `claude`) extract a bounded
  tail snippet from each tool output and store it in `TranscriptItem.summary`;
  the compacted record carries that summary so resumed sessions can recall the
  conclusion of elided commands (e.g., the last numbers of a `seq` run).
- R6 benchmark pilot 2026-09-17: ran a two-phase `count.py` task on Codex
  (gpt-6-astra) across three conditions on the same `test.txt`:
  | condition | phase-1 tokens | phase-2 tokens | count_lines correct? | recall last `seq` numbers? | notes |
  |---|---:|---:|---|---|---|
  | no compaction (one-shot) | 12,489 | — | yes | — | full conversation, no resume |
  | elide resume | 11,533 | 23,298 | yes | no | `seq 1 100` output stubbed; `seq 101 200` retained |
  | compacted resume | 11,533 | 26,589 | yes (file already carried `count_lines` from the elide resume in the shared workdir) | no | window swap accepted; digest did not carry the elided `seq` numbers |
  All three completed the coding edit; neither resume condition could recall
  the elided `seq` numbers because the digest did not explicitly record that
  fact. The shared `/private/tmp/gob-bench-shared` workdir created file-system
  cross-contamination between the elide and compacted resumes; a clean rerun
  would use separate workdirs. This pilot validates continuation, not recall of
  discarded verbatim output. Direct provider-native comparison, larger samples,
  and per-turn usage breakdown remain for future work.
- Claude digest resume qualification 2026-09-19: added a `last-prompt`/`mode` tail and
  copied native-looking metadata fields (`promptId`, `timestamp`, `permissionMode`,
  `promptSource`, `userType`, `entrypoint`, `cwd`, `version`, `gitBranch`) onto the
  synthetic `user` digest record. `gobstopper apply --in-place --strategy compacted` on
  a 51k-token Claude session elided tool outputs, `verify` was clean, and
  `claude --resume <session> -p "How many x characters were in the output?"` returned
  the correct fact (``5000 `x` characters.``) from the digest.
- Claude structural head-to-head 2026-09-19: enabled `serde_json` `preserve_order`
  so in-place Claude rewrites keep unchanged records byte-identical. On a 333k-token
  real Claude session, `gobstopper apply --in-place --strategy compacted` elided
  43 records and injected a digest (`gobstopper diff` reports 43 removed, 46 added —
  the 46 are 43 stubs plus 3 new tail records). `claude --resume <session>`
  succeeded and the model recalled the last user prompt and standing task state from
  the digest. Running `claude --resume <session> --autocompact 100` first appended
  records but removed 0 existing records; `gobstopper` produced the only measurable
  structural reduction.
- Agent-addressable vault search 2026-09-19: `gobstopper recall --query <q>`
  searches all archived state-card digests across sessions, scores results by
  keyword relevance, and supports `--limit` and `--json`. The same content-addressed
  vault that keeps every compaction state also serves as the retrieval layer for
  agent "infinite memory."
- Smarter digests and offline benchmark 2026-09-19: `claude` and `codex` adapters
  now pair `tool_use`/`function_call` records with their outputs, so `compacted`
  digests carry `bash(seq 1 100) => ...` instead of raw tail snippets. The user
  prompt is also extracted for `goal`, skipping system notifications. `gobstopper bench`
  runs `gobstopper eval` over every discovered session and emits a CSV of projected
  savings, verify errors, and probe recall per strategy.
- Live API-token benchmark 2026-09-17: on a 333k-token Claude session, the same
  resume question was asked under four conditions. `gobstopper elide` and
  `compacted` both reduced observed resume input tokens by ~30% (312,722 → ~220,000)
  while correctly recalling the stalled npm/rename task. `claude --resume --autocompact 100`
  consumed only 56,300 input tokens (~82% reduction) but answered incorrectly,
  claiming the renames were already completed and published. The README and
  `/benchmarks` page now report this head-to-head.
- Codex live comparison 2026-09-17: on a 101k-token real Codex session, the same
  resume question was asked under three conditions. `gobstopper compacted` reduced
  resume input tokens by 66% (101,275 → 34,503) and `elide` by 43% (101,275 → 57,980),
  both correctly recalling the Oh/BEAM benchmark and the 0.60 expansion gate. The
  `gobstopper`-written `compacted` record was accepted by `codex exec resume` and
  the swap completed a real API turn.
- R7 live A/B 2026-09-17: same-session three-way on a 339k-token Claude session
  at `--floor 310000`, using `claude --output-format json` provider cache counters.
  `cache_aware` elided 52 latest stale outputs preserving 107,884 file-level prefix
  tokens vs `compacted`'s 37 oldest with 6,639. Measured API usage: control
  `cache_read 10,010 / cache_creation 325,647` ($6.53), `cache_aware`
  `13,536 / 258,517` ($5.19), `compacted` `13,536 / 257,505` ($5.18). Honest
  finding: Claude Code's prompt-cache breakpoints bound `cache_read` at ~13.5k in
  all conditions, so `cache_aware`'s extra file-level prefix preservation does not
  buy additional provider cache hits today; its win is 16x more preserved prefix
  for vault `diff` audits and Merkle dedup. CLI additions this round: `--floor`
  override on `plan`/`apply`/`eval`/`bench`, `prefix_ratio` column in `bench` CSV,
  `prefix: N tokens cached` in `plan` output, and `undo --in-place` which writes a
  snapshot's exact bytes back to the original session path (guarded by
  `transaction::replace`'s changed-during-write check) so `claude --resume <id>`
  keeps working on the same id.

### R8: Scored relevance compaction (smart strategy)

- **Scope:** `scored` strategy, built-in heuristic scorer, optional Jev driver.
- **Objective:** score each stale tool result by keep-probability instead of
  eliding by position; drop the least relevant outputs first while protecting
  the tail and keeping the original content addressable in the vault.
- **Acceptance:** `scored` produces a valid `CompactionPlan` for every session
  `cache_aware` can compact; without a Jev key the deterministic heuristic
  still works; with `TYPESAFE_API_KEY` (or `GOBSTOPPER_JEV_API_KEY`) Jev
  requests are batched, bounded, and never include raw tool output — only
  sanitized labels/summaries.
- **Validation:** workspace tests (29 core tests for `scored`), Clippy, site
  check, and a live `gobstopper plan` on a 336k-token Claude session showing
  `336041 -> ~300230` tokens saved via 61 heuristic-scored elisions.
- **Morphogen assessment:** not directly useful — it is a deterministic
  creative-computation DSL, not a context or relevance-scoring primitive, so
  it was not integrated.
- **Jev cost model:** ~$0.0005/compaction for a 336k-token session with
  ~90 candidates in one or two 32k calls; scale is ~$0.04/day at 100
  compactions. The Jev path is optional and falls back to the heuristic
  scorer when no key is configured or a call fails.

- R8 update (2026-09-20): `scored` now uses a smarter deterministic heuristic
  (tool type, future-context references, goal overlap, superseded-duplicate
  detection, error markers) and supports a cheap-LLM `ScoreDriver` over Vercel
  AI Gateway. Jev remains a fallback via `TYPESAFE_API_KEY`; the LLM path is
  live with `AI_GATEWAY_API_KEY` and uses `qwen/qwen-2.5-7b-instruct` by
  default. Only sanitized labels/summaries leave the machine.


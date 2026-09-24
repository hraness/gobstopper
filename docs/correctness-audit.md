# Correctness audit — 2026-09-23

This is the historical initial audit of `c797729` and the repairs developed on
`codex/correctness-audit` for [PR 88](https://github.com/hraness/gobstopper/pull/88).
References below to “this change” and residual risks describe that stage, not
the current implementation. The original findings are preserved. For current
behavior and status, see the [assurance case](assurance/README.md) and
[correctness plan](correctness-plan.md); the subsequent foundation work is
tracked in [PR 90](https://github.com/hraness/gobstopper/pull/90).

Gobstopper has useful structural safeguards and substantial regression coverage,
but the system is not end-to-end verified. This audit found defects in recovery
storage, operation identity, provider control, session isolation, and measurement
validation. Passing a provider compaction once does not close those classes.

The source baseline is `c797729` on `main`. Work is integrated on
`codex/correctness-audit`. The latest local Devin conversation titled
“gobstopper” (September 21–23) supplied historical intent and operational context;
its completion claims were not treated as current verification results. No
private session text, live vault objects, credentials, or operational identifiers
are included in this report or the fixtures.

The intended system is an inspectable compaction and recovery layer that preserves
provider ownership, source integrity, recoverability, and truthful evidence.
The historical transcript described three deployed watchers and a successful
Codex native compaction. This audit did not replay that operation on user data.
Unrelated website edits in the original checkout were excluded by using a fresh
worktree from current main.

## What correctness means

1. An admitted operation names the exact provider, store, session, source version,
   transformation, policy, and output identity it will affect.
2. Rejected, failed, or interrupted work does not silently overwrite source data,
   delete a required recovery point, or falsely report success.
3. Every published recovery object is readable and integrity checked while it is
   retained or pinned. Garbage collection respects concurrent publishers/readers
   and pending operations.
4. Provider-owned writes require actual custody or a provider-native operation
   with a qualified ownership/serialization contract.
   File age, a released lock probe, and equal rereads do not establish custody.
5. Rewrites preserve specified structural invariants and protected content. This
   does not imply that a lossy rewrite preserves every future task answer.
6. Unknown provider outcomes remain unknown until reconciled. Cooldowns survive
   restarts, expire when promised, and do not cause duplicate dispatch storms.
7. Inspection stays within its requested session and content permissions. Resource
   bounds apply to allocation, queues, subprocess trees, and disk, not only to the
   final returned string.
8. Token estimates, reset counters, retention proxies, successful continuation,
   task quality, cache hits, and billed savings are separate measurements.

## Coverage map

This is a source review of each major boundary, not a claim that every execution
path has been exercised. The following files are the maintained starting points
for deeper verification; the [plan](correctness-plan.md) gives acceptance gates.

| Capability / state | Entry and owning modules | Existing evidence | Remaining boundary |
|---|---|---|---|
| Normalized context and token arithmetic | core `model.rs`, `estimate.rs` | core examples; adapter fixtures | Saturating arithmetic, ambiguous zero/reset usage, byte/token estimation |
| Plan IR and admission | core `plan.rs`, `validation.rs`; CLI `evaluate_detailed` | adversarial edit tests | One authoritative contract at every adapter entry; arbitrary inputs |
| Strategy selection, protected tail, ranking | core `strategy/*` | unit tests; Hegel surgery suite | All-strategy generated validity, convergence, estimation correspondence |
| Provider discovery and active context | adapters `detect.rs`, `codex.rs`, `claude.rs`, `devin.rs` | fixtures; export and active-branch tests | Provider schema drift, missing data, liveness heuristic versus custody |
| Structural verification | adapters `verify.rs` | malformed/pairing/window regressions | Explicit soundness relative to parser; unsupported shapes fail closed |
| Copy/fork transaction and retry receipts | adapters `transaction.rs`, `copy.rs`, `fork.rs` | source-binding/no-clobber tests | Syscall crash windows, receipt reconciliation, path races |
| Vault, recovery, pruning | adapters `vault.rs`, `recovery.rs` | hash/bounds/permissions/recovery tests | Crash durability, torn index recovery, receipt lifecycle and cross-process custody |
| Devin SQLite mutation/restore | adapters `devin.rs` | transactional fixture tests | Retained provider lock ownership and post-commit uncertainty |
| Native compaction and watch state | CLI `main.rs`; adapters Devin ACP | watch subprocess fixtures | All-order protocol state machines; durable dispatch/reconciliation; live pinning |
| Hooks and settings | CLI `hooks.rs` | installer/advisory/postcompact tests | Concurrent settings writes, backup identity, exact pre/post operation correlation |
| Configuration and plugins | CLI `config.rs`; adapters `plugins.rs` | strict schema; bounded plugin contract tests | Trusted code is unsandboxed; aggregate projection/resource bounds |
| Model scorers/digests | CLI `apple*`, `jev.rs`, `llm_scorer.rs`, `secrets.rs` | cache/input/output/fallback tests | Cross-process cache integrity, privacy opt-ins, model drift and semantic fidelity |
| MCP and CLI inspection | CLI `mcp.rs`, history/vault/recall/show | content opt-in tests; new shared-store fixtures | Bounded JSON-RPC framing and all inspection paths' side effects |
| Evaluation, retention, reporting | adapters `eval.rs`, `study.rs`; core `probe.rs`, `events.rs`; CLI `report.rs` | replay and score tests | Source-bound annotations, missingness, selection bias, counter reset semantics |
| Monitor and experiments | `scripts/monitor.py`, study/probe runners | Python child/allowlist/timeout tests | Independent vault readers, process-tree exit, corpus and registration integrity |
| Build, supply chain, public claims | Cargo manifests/lockfile, CI, docs/site | fmt, Clippy, tests, MSRV; pinned actions | Formal gate, supported-platform matrix, packaging/provenance and claim receipts |

## Findings and repairs in this change

The regressions use synthetic temporary homes and stores. Final command results
are recorded in the delivery evidence; a source repair alone is not proof.

| Finding | Failure mechanism | Repair / evidence |
|---|---|---|
| Snapshot/prune race | A snapshot publishes chunks before appending the index. Prune can collect those chunks during that window, leaving a later index entry unreadable. Index rereads cannot make this sequence atomic. | Stable shared/exclusive vault custody; deterministic exclusion and reader tests; TLA+ interleaving pilot with no-custody negative control. |
| Recovery roots omitted by pruning | Pending copy receipts refer to snapshots not necessarily retained by the per-stream index policy. | Include operation-receipt recovery roots and hold custody across the copy operation's receipt window; negative control drops pins. |
| Retained manifest uncertainty ignored | Skipping an unreadable/corrupt retained manifest makes its reachable chunks appear collectible. | Fail before index publication or deletion when a retained root cannot be validated. |
| Same-second retention chooses by hash | SHA ordering is unrelated to append order; `keep=1` can discard the newest snapshot. | Retain by timestamp plus actual index position; no sleeps needed to exercise ties. |
| Compacted-copy identity incomplete | `digest` and `keep_tail` affect output but were absent from the operation key, allowing a retry to reuse a different result. | Bind both effective parameters; distinct-input/retry regression. |
| Devin planned and written stores can differ | A caller supplies both a planned database path and a separate native data root; identical session IDs and exports could pass the source check in the wrong store. | Require canonical target equality before any write; include database identity in the operation key; two-store negative fixture. |
| Devin restore identity mismatch | A foreign export can contain overlapping node IDs. A restore must reject it before changing the selected session. | Explicit session identity admission and foreign-export regression. |
| Native completion is misattributed or lost | Replies acknowledge dispatch; unrelated turns/sessions and notifications arriving before the reply can disagree with actual compaction progress. | Preserve pre-ack progress; require the Codex compaction item's matching terminal turn and the exact Devin session's terminal event. Session-only ACP matching still lacks an operation identity. |
| Native dispatch uses the wrong authority or path | Claude planner eligibility can suppress the native operation; subprocesses can inherit a different provider home; errors can fall through to surgery. | Decide native dispatch before file-plan eligibility, pass the discovered provider home, and terminate the native branch on every outcome. Synthetic isolated-home fixtures cover these boundaries. |
| Restart forgets an uncertain dispatch | Persisting cooldown only after the provider returns leaves a crash window for duplicate work. | Durably checkpoint conservative uncertainty before dispatch; refuse dispatch if persistence fails. This reduces the crash window but is not provider reconciliation or multiwatch exclusion. |
| Standalone native fork compares incompatible hashes and identities | A manifest digest was compared with raw source SHA; using the original snapshot as the fork's before-state also mistakes fork metadata changes for compaction. | Compare the source-byte SHA and snapshot the exact prepared fork before dispatch; preserve the original recovery snapshot separately. Missing-usage/no-op fixture confirms identical native before/after evidence. |
| Native telemetry claims unmeasured savings | Provider resets, stale counters, missing post-state or estimated plan output can be reported as realized savings. | Freeze post-state in the vault; classify unchanged results as no-op and reset/unknown usage as unresolved with zero claimed savings; attach before/after recovery references. |
| Native protocol/process resources and diagnostics are unsafe | Unbounded frames/queues, inherited pipes, raw provider errors and process-group identity reuse cross resource/privacy/ownership boundaries. | On Unix, bounded Codex/ACP readers and queues, closed error categories, and owned-group cleanup with retained child identity; adversarial subprocess fixtures. Claude lifecycle and non-Unix support remain separate obligations. |
| Native diagnostics expose raw session identity | New stderr diagnostics printed session identifiers; PR CodeQL detected both sites. | Remove raw identifiers from the diagnostics while retaining the exact identity in structured recovery evidence; native no-op fixture checks both sides of this boundary. |
| Native no-op suppression never expires | A no-op records a timed holddown and a permanent unchanged-source settlement. | No-op uses expiring suppression; repeat fixture exercises retry after expiry. |
| MCP verifies database bytes | CLI verify exports the selected Devin session, while MCP treated SQLite as JSONL. | MCP uses the same canonical per-session export; clean fixture remains byte-identical. |
| Shared-store history crosses sessions | Filtering by `sessions.db` path alone includes every session in that database. | Provider, exact resolved session, and path scope in MCP history and CLI history/vault. |
| Path and ID lookup disagree on provider | Filename heuristics discard the configured root's provider; supported Codex envelopes without metadata/ordinal can be misidentified as Claude. The stricter history scope exposed this in the aggregate suite. | Preserve provider identity while scanning roots and recognize supported Codex payload envelopes; original Unicode archived-ID and new custom-root consistency regressions pass without relaxing isolation. |
| Retention accepts malformed counters | Python booleans are integers; falsey missing fields became zero and counts could exceed totals. Stat-before-read did not bound a growing file. | Require complete bounded integer counts, reject impossible triples, bound actual reads and tolerate invalid UTF-8; adversarial fixtures. |

## Material residual risks

These remain open regardless of the new model's result. They are work items, not
claims of observed production data loss.

- **P1 — Provider custody remains incomplete for direct writes.**
  `transaction::replace` rereads before rename; a provider append can occur
  between the final read and rename. An idle age, stable watch fingerprint, and
  Claude opening per write do not remove this race. Devin `session_active`
  releases its flock probe before `apply_store` starts its SQL transaction;
  SQLite writer serialization does not stop a provider from loading stale
  in-memory session state. Preserve opt-in boundaries and qualify a retained
  provider-compatible custody mechanism before stronger safety claims.
  Native admission has an ownership assumption too: Codex/Claude file age is not
  proof that no TUI owns the session, and ACP's lock probe is not a retained lock.
  A private control process does not establish exclusive session ownership.
  Direct Claude watch surgery also passes the plan without its loaded source
  hash to `apply_edits`; the adapter rereads and can apply stale record indexes
  to newer bytes. Binding the planned input is a separate requirement from
  preventing an append during final replacement.
- **P1 — Durable outcome reconciliation is incomplete.** Native dispatch may succeed
  before the local watcher persists a terminal record or cooldown. A process
  restart must not replay an uncertain operation solely because local state is
  absent. The new pre-dispatch checkpoint reduces this risk, but its uncertainty
  cooldown eventually expires without provider reconciliation. Two overlapping
  watchers also lack shared dispatch custody. Likewise a store commit can precede
  a copy receipt. Model these crash
  windows explicitly, then inject failures at the production boundaries.
- **A snapshot is not a transaction log.** Hash checking detects corruption;
  it does not establish recovery from a torn index, power loss, lost fsync, a
  failed directory sync, or a disappearing mounted filesystem. Separate
  “published but durability unknown” from “not applied.”
- **Reader and garbage-collector coverage must stay complete.** The Python
  retention-audit runner independently reconstructs objects. It is not covered
  merely because Rust `read_object` obtains custody. Migrate it to the supported
  reader or the same protocol before running it concurrently with pruning.
- **Read-only MCP is a semantic contract, not a sandbox.** MCP `plan` reaches
  configured strategy plugins/legacy commands and optional scorers. Those are
  explicitly trusted code and may perform I/O. Define a deterministic inspection
  mode and enforce it end to end if “no external execution” is promised. The
  newline-based request reader also needs an actual frame bound.
- **Verifier and provider agreement is partial.** A clean structural report is
  not proof that a proprietary provider will accept, resume, or reason correctly
  from the transcript. Session branch selection, partial tails, system messages,
  compacted windows and tool pairing need frozen provider-version fixtures.
- **Some diagnostic/metadata paths need a dedicated privacy review.** Provider
  errors, paths, model rationale and summaries can cross logs and plan surfaces.
  Use sentinel fixtures to assert the permitted fields, not only a tool-name
  allowlist. The repository's baseline CodeQL inventory at `c797729` contained
  27 open `rust/cleartext-logging` alerts, each automatically rated high severity.
  Inventory intentional requested displays separately from background diagnostics
  and secret-bearing fields before resolving an alert. The PR check reports new
  alerts; passing it is not evidence that the baseline inventory is clear.
- **Semantic retention is empirical.** Literal/lexical matches can miss meaning
  changes, negation, authority, temporal order, and pending tool effects. A reset
  usage counter is not a measurement of zero context or reclaimed tokens. Small
  convenience samples cannot establish whole-system task fidelity or savings.
- **Resource/liveness coverage is not complete.** Bound recursive discovery,
  frame queues, metadata aggregation, slow consumers, subprocess descendants and
  OS lock waits. A finite safety model proves neither fairness nor acceptable
  latency under contention. Claude headless compaction still collects only its
  leader. Process-group custody does not authorize signaling descendants that
  deliberately leave the owned group; cancellable readers must still terminate
  without depending on those descendants closing their inherited pipes.
- **Operational qualification is dated evidence.** The recovered conversation's
  successful trials apply to the observed binary, provider version, account and
  circumstances. They do not prove today's artifact, future provider versions,
  all filesystem targets, or all error/restart paths.

## Formal verification choice

Valhalla's `docs/verification.md` separates
bounded Kani harnesses over production Rust from an unbounded Verus reference
model. Its `AGENTS.md` additionally specifies a production-capacity spent-nonce
Kani pilot and Hegel restart/fault tests. The useful pattern is a narrow explicit
claim, production correspondence, negative examples, and honest exclusions.
Valhalla was inspected in a sibling checkout; it is not a Gobstopper build
dependency. The inspected checkout was clean at
`28c2db2812ff82e989a95384dba4d9a3ac537d69`; its proofs were not rerun for this
audit. Its production-only changes do not currently trigger its Verus
workflow, so that workflow is not a template for drift-safe proof gates here.

- **TLA+ first:** storage publication/pruning and native watcher dispatch are
  interleaving problems. The checked [vault pilot](../verify/vault/README.md)
  explores a finite state space and intentionally broken variants. Its atoms
  stand for verified immutable objects; no filesystem implementation is proved.
- **Kani for small production decisions:** numeric admission, protected-tail
  predicates, edit compatibility, bounded codec helpers and usage transitions.
  Keep unwinding, overflow, memory-safety and cover checks enabled. Avoid symbolic
  `serde_json`/large maps as the first target, following Valhalla's experience.
- **Lean for mathematical structure:** define transcripts, projections, protected
  positions and transformation composition, then prove preservation/identity and
  refinement laws. This is useful only with a shared executable oracle, generated
  artifacts, or a tested correspondence to Rust. A parallel specification that
  can silently drift is not a proof of the shipped binary.
- **Hegel and fault injection connect the layers:** generate command sequences
  against real temporary stores, replay model counterexamples against adapters,
  and turn every minimized failure into a permanent regression. Model checking,
  theorem proving, sampling and provider trials answer different questions.

The [correctness plan](correctness-plan.md) orders that work and defines when each
claim may be upgraded. The target is an auditable assurance case for specific
properties under explicit assumptions, not an unsupported universal guarantee.

Primary references: the [official TLC release](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4),
[Kani harness attributes](https://model-checking.github.io/kani/reference/attributes.html),
[Lean proof validation](https://lean-lang.org/doc/reference/latest/ValidatingProofs/),
and [Cedar's executable Lean specification and Rust differential testing](https://github.com/cedar-policy/cedar-spec).

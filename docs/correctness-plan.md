# Gobstopper correctness assurance plan

## Outcome

Build a reviewable chain from requirements to invariants, production code,
executable specifications, counterexamples, tests, provider qualification, and
release evidence. A reader must be able to determine exactly which property is
established for which revision, input domain, provider version, and environment.

Start from the [2026-09-23 audit](correctness-audit.md) and checked
[TLA+ vault pilot](../verify/vault/README.md). The pilot and immediate repairs are
baseline work, not completion of the phases below. Every future phase starts
`Not started`; mark it complete only with implementation, independent review,
and the phase's acceptance evidence. “No bugs found,” passing compilation, a
timeout, a suppressed model error, and a successful demo are not proof results.

Non-goals: proving proprietary providers or LLM reasoning correct, preserving
every answer after arbitrary lossy compression, proving cryptographic primitives
or the operating system, rewriting the application in a proof language, and
turning a bounded model into an unbounded claim. Site redesign and unrelated
repository changes are excluded.

## Constraints and delivery

- Preserve existing source data and stable JSON fields. Prefer additive schemas;
  use explicit versioned migrations for changed semantics. Never discard evidence
  to recover from an error or make a validation pass.
- Preserve default copy/delegation behavior. New formal results do not authorize
  live in-place mutation. Direct provider-store activation requires retained
  compatible custody or must remain guarded/disabled.
- Use synthetic isolated homes for automatic tests. Existing subscriptions are
  the boundary for any later authorized provider qualification; no new paid API
  account, account switching, or tests on live user sessions are implied.
- Strategies remain pure. Plugins propose within a closed capability boundary;
  trusted commands remain trusted code, not a sandbox.
- One integration owner owns `Cargo.toml`, crate manifests, `Cargo.lock`, CI,
  proof-tool lockfiles, cross-module interfaces, and this plan. Workers own focused
  tests; the integrator runs the aggregate gate after convergence. Independent
  lanes below have disjoint write sets; interface changes converge first.
- Use the repository's conditional host scheduler if installed, by its absolute
  path. No scheduler was installed in the usual command locations during this
  audit; do not resurrect a retired global wrapper. Compute, native and browser
  custody remain separate where the host requires them.
- Deliver task-owned commits through a PR, independent review and the repository
  CI `Required` aggregate. Inspect current branch/ruleset gates at delivery time;
  never bypass them. Ordinary delivery is already authorized. Merge only the
  checked head and verify the merged result. There is currently no release
  workflow in this repository; do not invent a tag or publish a package merely
  to call an audit complete.
- If installing a resulting binary, record binary SHA, source revision and
  supported platform. Inspect exact service configuration before restarting only
  task-owned services. Retain the prior binary and its state compatibility for
  rollback. Do not trigger a real compaction solely to test deployment.

## Invariant ledger

Each invariant gets a source owner, a predicate, assumptions, evidence links,
negative control, input bounds, and a review trigger. No row starts “proven.”

| ID | Obligation | Primary evidence |
|---|---|---|
| ID-1 | Provider/store/session/version identity is exact; retries bind all effective inputs | Rust admission + generated identity laws |
| OWN-1 | Mutation retains exclusive compatible custody through commit/reconciliation | TLA+ control model + provider lock fixtures + version qualification |
| SRC-1 | Copy failures/retries preserve source bytes and no-clobber output | Hegel/fault injection + publication model |
| REC-1 | Every retained index or recovery pin resolves to integrity-checked objects | Vault model + real storage command sequences |
| REC-2 | Crash recovery yields a complete old/new state or explicit repair-required state | Syscall fault/crash matrix and journal model |
| PLAN-1 | Only known eligible indexes are edited; protected tail/control records survive | Production Kani predicates + Lean algebra + differential tests |
| PLAN-2 | Host rewrites meet declared bounds; deterministic repeated rewrites converge under fixed input/policy | Arithmetic proofs + repeated-operation properties; separate native/fairness assumptions |
| DIA-1 | Parser, writer and verifier agree on supported provider dialect semantics | Frozen fixtures + fuzzing + qualified provider continuation |
| CTRL-1 | Applied implies matching terminal operation evidence; unknown never implies success | Native protocol model + out-of-order/duplicate fixture transcripts |
| CTRL-2 | Uncertain dispatch is not replayed; cooldown and retry behavior survives restart | Durable state model + crash fixtures |
| PRIV-1 | Only authorized sessions/fields leave each inspection/log/model boundary | Adversarial sentinel and noninterference fixtures |
| READ-1 | Inspection performs only documented reads; no hidden provider call/mutation | OS/fixture boundary tests and command-effect inventory |
| RES-1 | Work has explicit memory/disk/frame/queue/process/deadline bounds | Boundary tests, stress/resource receipts and subprocess accounting |
| OBS-1 | Measurements preserve missingness, provenance, units and reset semantics | Numeric properties + source-bound observation fixtures |
| SEM-1 | Declared critical task facts/constraints survive within the studied population | Registered held-out tasks, blinded labels, continuation outcomes |
| REL-1 | Delivered bytes correspond to checked source and qualified activation mode | Build/provenance/install receipts + release verification |

## Phase map

| Phase | Independently verifiable deliverable | Depends on | Write owner / scope | Parallel lanes |
|---|---|---|---|---|
| C1 | Contract/claim/effect inventory | none | integrator: docs and schemas | C2, C3 investigations |
| C2 | Safe provider custody and restoration | C1 | adapters: Devin/transaction; coordinated CLI boundary | C3, C5, C8 |
| C3 | Crash-safe vault and publication | C1 | adapters: vault/copy/fork/recovery + `verify/vault` | C2, C5, C8 |
| C4 | Durable native control machine | C1, C2 | CLI native/watch; adapters ACP by explicit handoff | C5, C8, C10 |
| C5 | Dialect/parser/verifier agreement | C1 | adapters: codex/claude/payload/verify; Devin read side after C2 | C3, C8, C10 |
| C6 | Production Rust proof kernels | C1, C5 | core model/validation/strategy arithmetic + Kani | C4, C8, C10 |
| C7 | Lean transcript algebra and correspondence | C5, C6 | new `verify/transcript`; core oracle through integrator | C8, C10, C11 |
| C8 | Trusted-extension and read-only MCP boundary | C1 | plugins/config/MCP, CLI call interface via integrator | C2, C3, C5 |
| C9 | Safe hooks/settings transactions | C1, C2, C4 | CLI hooks + settings fixture tests | C7, C10, C11 |
| C10 | Scorer/digest reliability and privacy | C1 | CLI scorer/digest/secrets modules | C4, C5, C8 |
| C11 | Trustworthy measurement and replay | C1, C5 | probe/events/eval/study/report and study scripts | C7, C9, C10 |
| C12 | Reproducible proof/test admission | C3, C4, C6, C7, C8 | integrator: CI/tool pins/manifests/fuzz runners | C11, C13 preparation |
| C13 | Provider/platform activation qualification | C2–C6, C8–C12 | qualification fixtures/receipts; one owner per provider wait | C14 offline workloads |
| C14 | Bounded adversarial/soak evidence | C3, C4, C8, C9, C12 | harnesses/monitor tests; no overlapping production ownership | C13 |
| C15 | Published assurance case and delivery | C1–C14 | integrator: docs, claim registry, release receipts | none |

The listed directories contain shared files: they are not blanket permission to
edit concurrently. In particular C2/C4/C5 must hand off `devin.rs`, C4/C8/C9 must
serialize shared `main.rs` changes, and C6/C7/C11 must hand off core interfaces.

## C1: Contracts, authority, and claim inventory

- **Status:** Complete — implemented and independently reviewed; delivery gates remain in C15
- **Depends on:** none
- **Objective:** make every correctness claim falsifiable and every mutating
  entry point explicit.
- **Scope:** docs; a machine-readable invariant/claim ledger and command-effect
  inventory covering library APIs, CLI, hooks, MCP, watch, plugins and scripts.
- **Out of scope:** new mutation features or weakening existing gates.
- **Approach:** distinguish source, effective context, archive, exported store,
  provider memory, receipts, event log, watch state, caches and configuration.
  Define supported filesystem/platform/provider assumptions and all unknown
  states. Resolve conflicting roadmap/design comments in favor of actual code.
- **Acceptance:** every coverage-map row has an owner, invariant, strongest current
  evidence, exclusions and a next gate; every write is classified; all public
  "safe/verified/proven/savings" claims have a bounded supporting receipt or are
  corrected. Triage each baseline scanner finding against its actual source,
  sink and authorized output contract; record the repair or reviewed rationale,
  with no blanket dismissal of a rule. Record exact schema/version and
  upgrade/downgrade behavior.
- **Validation:** `cargo test -p gobstopper --locked`; structural ledger check
  added by this phase, with a fixture rejecting an unsupported claim.

## C2: Provider custody and target-safe restoration

- **Status:** Complete — implemented and independently reviewed; delivery gates remain in C15
- **Depends on:** C1
- **Objective:** an operator cannot authorize one session and mutate another, or
  race a newly opening provider through an idle heuristic.
- **Scope:** Devin lock/apply/restore, transaction publication admission, shared
  mutation entry points; preserve native delegation.
- **Out of scope:** inventing a lock that the provider does not honor.
- **Approach:** qualify lifetime ownership for the pinned provider. Hold it before
  source export and through commit; fail closed on missing authority, ambiguous
  identity, lock failure or unsupported versions. Reject foreign snapshot session,
  store and graph identity. Bound SQL updates and preserve unrelated rows.
- **Implementation decision (2026-09-23):** inspection found no documented,
  qualified lifetime lock for direct provider writes. Disable direct-store and
  arbitrary-path rewrite APIs before effects, retain detached byte transforms
  and no-clobber copies, and qualify native delegation separately in C13.
  An arbitrary path labeled "detached" is not proof of ownership.
- **Acceptance:** start-provider versus mutate interleavings cannot admit both;
  live/unknown ownership refuses surgery; foreign restore leaves byte/row state
  unchanged; append during replacement cannot be lost; failed activation leaves
  existing defaults intact. If compatible custody cannot be established, disable
  that direct-write mode through a reviewed source change and tests.
- **Validation:** `cargo test -p gobstopper-adapters --locked devin::tests`;
  `cargo test -p gobstopper-adapters --test audit_regressions --locked`;
  two-process lock/restore fixtures. Complete this phase's artifact admission
  with unsupported or unqualified modes guarded/disabled. Live activation
  qualification is recorded separately in C13 and is not a circular prerequisite
  for admitting those guarded artifacts.

## C3: Storage publication, recovery pins, pruning, and crashes

- **Status:** Complete — implemented and independently reviewed; delivery gates remain in C15
- **Depends on:** C1
- **Objective:** every admitted recovery promise survives concurrent operations
  and each declared crash boundary, or produces explicit recovery-required state.
- **Scope:** vault/copy/fork/recovery; extend the checked vault model.
- **Out of scope:** proving APFS/ext4 internals or silently deleting damaged data.
- **Approach:** define publication linearization and fsync ordering; version the
  durable intent state and pin lifecycle. Include snapshot, receipt intent,
  output publication, receipt completion, reader, prune, and crash/restart in one
  model. Add syscall injection seams for write/sync/link/rename/unlink/SQL commit.
- **Implementation decision (2026-09-23):** a durable prepared operation pins
  both its verified recovery snapshot and exact prepared output bytes. Generated
  identities make replaying a transform an inadequate recovery mechanism.
  Reconciliation reads the recorded bytes and never overwrites a conflicting
  output. Pins remain until an explicit retirement policy; malformed state
  prevents collection. Publication errors distinguish visible effects from
  confirmed durability.
- **Acceptance:** fail each boundary before and after its effect; repeated recovery
  is idempotent; torn index and malformed roots never authorize deletion; shared
  chunks/legacy objects remain readable; same-second and nonmonotonic-clock
  retention obey declared ordering; abandoned pins require explicit recoverability
  policy. Independent readers use the same custody protocol. Test process death,
  not only returned errors; “durability unknown” is distinguishable from refusal.
- **Validation:** `cargo test -p gobstopper-adapters --test recovery --locked`;
  `cargo test -p gobstopper-adapters --test surgery_hegel --locked`;
  `cargo test -p gobstopper-adapters --locked vault::tests`;
  `python3 verify/vault/check.py --java "$JAVA" --tlc-jar "$TLC_JAR" --output "$EVIDENCE"`.
  Tool variables name pinned executables/files; evidence directory must be new.

## C4: Durable native dispatch and watch-state model

- **Status:** Complete — implemented and independently reviewed; delivery gates remain in C15
- **Depends on:** C1, C2
- **Objective:** dispatch exactly the intended operation, classify outcomes
  honestly, and reconcile uncertain progress across process restarts.
- **Scope:** native Codex/Claude/Devin controls, watch state and receipts.
- **Out of scope:** retries that assume a timed-out remote operation did nothing.
- **Approach:** explicit states `prepared`, `dispatched`, `observed_applied`,
  `observed_noop`, `rejected`, `unknown`, `reconciled`; persist intent before
  dispatch. Match session+operation+turn identities. Buffer bounded pre-ack events,
  reject malformed/oversized frames, preserve progress while draining pipes.
- **Acceptance:** exercise ack-before/after-terminal, duplicate/unrelated events,
  lost acks, EOF, quota, auth, unsupported model, timeout, abort, reset usage,
  crashes at every state, two watchers and provider append during failure.
  Include delayed on-disk publication after terminal, unreadable/malformed
  post-state and exact same-target before/after evidence. Bind suppression
  decisions to all effective policy inputs, including relevant environment and
  provider/strategy versions, separately from durable unresolved operations.
  Unknown never reports applied or automatically falls back to file mutation.
  No-op expires; configuration generations do not silently discard unresolved
  operations. A provider protocol without an operation identifier gets an
  explicit correlation assumption or an unknown result, never an invented
  identifier. Child identity remains reserved until owned-group cleanup;
  blocked or inherited pipes cannot leave detached reader threads. Claude's
  leader-only cleanup must join the same lifecycle contract. Fairness assumptions
  and starvation limits are explicit.
- **Validation:** `cargo test -p gobstopper --test watch --locked`;
  `cargo test -p gobstopper --locked codex_`;
  new `verify/watch/check.py` must reject replay/false-success mutants and report
  the explored bounds before becoming a required CI gate.

## C5: Supported dialects and independent verifier oracles

- **Status:** Complete — implemented and independently reviewed; delivery gates remain in C15
- **Depends on:** C1
- **Objective:** parser, rewrite anchor, effective-context projection and verifier
  agree for each documented supported provider version.
- **Scope:** Codex/Claude/Devin read adapters, payload eligibility, verifier.
- **Out of scope:** claiming undocumented provider behavior beyond frozen evidence.
- **Approach:** frozen synthetic dialect corpora with source/version provenance;
  independent expected context/linkage rather than deriving expected output from
  the same parser. Fuzz JSON/UTF-8/tails/nesting/index bounds and differential
  round-trips. Promote Hegel minima into named regression fixtures.
- **Acceptance:** cover compacted windows, dead branches, duplicate IDs, missing
  heads, cycles, overlapping tool IDs, call/result ordering, negative/huge indexes,
  mixed system/reasoning/tool blocks, Unicode and truncated writes. Unknown shapes
  are unavailable or rejected, never assumed safely elidable. Verification scope
  is stated separately from provider resume acceptance.
- **Evidence:** frozen hand-authored corpus for all three providers, deterministic
  adversarial runner (512 cases per declared seed), 20 named dialect regressions,
  26 Hegel properties and five checker failure tests. Root review also identified
  and closed metadata FIFO/size gaps, duplicate effective tool identities and
  unknown output fields. Invalid generator graphs were retained as named negative
  regressions; valid-graph generators were repaired rather than weakening checks.
  Corpus provenance is synthetic and explicitly not provider qualification.
- **Validation:** `cargo test -p gobstopper-adapters --locked`;
  bounded fuzz commands and corpus checks added with this phase; every discovered
  failure has a deterministic replay independent of the fuzzer seed database.

## C6: Small proofs over production Rust

- **Status:** Complete — implemented and independently reviewed; delivery gates remain in C15
- **Depends on:** C1, C5
- **Objective:** prove high-value total predicates used by the shipped program.
- **Scope:** core policy bounds, edit eligibility, index uniqueness/protection,
  usage transitions and checked/saturating arithmetic; adjacent Kani harnesses.
- **Out of scope:** substituting a tiny capacity constant for the production one.
- **Approach:** factor pure kernels only where useful for production. Pin Kani and
  toolchain; partition fixed input lengths rather than symbolically expanding
  large containers. Start with eligibility and numeric admission, then bounded
  sequences. Compare the same functions against a simpler independent oracle.
- **Acceptance:** successful assertions, satisfied unwinding checks and reachable
  cover conditions; prove failure atomicity, no panic/overflow, protected indices
  excluded, and provider-control edits unmixed. Every bound and trusted primitive
  is recorded. A boundary mutation must fail verification. No `assume(false)` or
  unchecked admission shortcut can make a proof pass.
- **Validation:** `cargo test -p gobstopper-core --locked`;
  `cargo kani -p gobstopper-core --output-format terse` with exact pinned toolchain,
  named harnesses, timeouts and unwind bounds documented when implemented.
  Unsupported/inconclusive runs fail the proof gate.

## C7: Lean transcript algebra and Rust correspondence

- **Status:** Complete — implemented and independently reviewed; delivery gates remain in C15
- **Depends on:** C5, C6
- **Objective:** establish unbounded structural laws where they provide value
  beyond bounded Rust checks.
- **Scope:** `verify/transcript/` Lean package, executable test-vector oracle,
  integration-owned Rust correspondence tests.
- **Out of scope:** asserting that a Lean reimplementation proves Rust automatically.
- **Approach:** define finite sequences with stable record IDs, protected positions,
  partial elision and digest insertion, effective-context projections, and
  well-formed tool links. Prove identity/no-op, rejection identity, protected
  projection preservation, order preservation and composition under explicit
  preconditions. Keep byte codec/OS behavior separate. Prefer an executable shared
  specification or generated decision tables over two unconnected algorithms.
- **Acceptance:** `lake build` succeeds with pinned dependencies; no `sorry`,
  `admit`, new unreviewed axioms or disabled checking. Audit theorem axioms.
  Independently generated vectors agree with real Rust execution; changing either
  side breaks a correspondence fixture. State exactly which semantic properties
  are intentionally unprovable for lossy output.
- **Validation:** `lake build` in `verify/transcript`; proof hygiene/negative-control
  runner added by this phase; `cargo test -p gobstopper-core --locked` with the
  correspondence suite. Resolver: integrator decides whether Lean's maintenance
  cost is justified after the first production-connected theorem; Verus remains
  a viable Rust-oriented alternative, not a second mandatory duplicate proof.

## C8: Plugins and read-only inspection

- **Status:** Complete — implemented and independently reviewed; delivery gates remain in C15
- **Depends on:** C1
- **Objective:** a read-only call cannot silently execute broader capabilities,
  allocate unbounded input, or cross session/content scope.
- **Scope:** plugin host, config admission, MCP framing/tools, shared CLI lookups.
- **Out of scope:** claiming exact-hash trust is an OS sandbox.
- **Approach:** enumerate effects reachable from every MCP tool; deterministic
  inspection mode must reject/disable external strategies/scorers before invoking
  them. Bound request lines before allocating them. Use typed argument validation
  and exact resolved provider/session/store identities. Validate aggregate token
  arithmetic and metadata, not only each collection's length.
- **Acceptance:** oversized/no-newline frames, malformed JSON-RPC, malformed
  argument types, ambiguous selectors, shared-store sessions, plugin child floods,
  executable drift and unexpected response capabilities fail closed. Sentinel
  fields do not reach unauthorized output. Descendants are collected after both
  normal exit and timeout; inspection does not create vault/config/event state.
- **Validation:** `cargo test -p gobstopper --test mcp --locked`;
  `cargo test -p gobstopper-adapters --test plugin_contract --locked`;
  `cargo test -p gobstopper --locked mcp::tests`;
  `cargo test -p gobstopper --locked config::tests`.

## C9: Hooks and settings updates

- **Status:** Complete — implemented and independently reviewed; delivery gates remain in C15
- **Depends on:** C1, C2, C4
- **Objective:** hooks install idempotently without clobbering concurrent settings
  and correlate recovery evidence to the actual compaction.
- **Scope:** hook installer/uninstaller/advisory/callbacks and settings transaction.
- **Out of scope:** broad replacement of user configuration or removing trust prompts.
- **Approach:** immutable or no-clobber backups, private exclusive temp creation,
  exact-source precondition, safe symlink handling, write/sync/publication intent;
  use provider settings API/custody where required. Correlate pre/post callbacks
  with operation/session and tolerate duplicate/missing/out-of-order hooks.
- **Acceptance:** unrelated settings survive every merge/uninstall; failed writes
  retain recovery; concurrent installers cannot lose updates; a post-hook cannot
  attribute an unrelated compaction as this tool's success. Hook timeout or
  telemetry failure does not corrupt provider behavior.
- **Implementation decision:** no compatible provider/editor settings custody API
  is available. Direct settings mutation is disabled, including missing files;
  `--output <new-file>` publishes an inert private bundle with original bytes,
  source/candidate hashes and exact command ownership. Provider-owned application
  remains outside this artifact. Callbacks have no operation identifier, so they
  record unattributed observations and never fabricate causal before/after pairs
  or count a provider hook as this tool's applied result.
- **Validation:** `cargo test -p gobstopper --locked hooks::tests`; isolated
  multi-process settings and provider-hook fixtures. Admit the guarded artifact
  here; C13 separately decides live activation against pinned providers.

## C10: Scorer and digest boundaries

- **Status:** Complete — implemented and independently reviewed; delivery gates remain in C15
- **Depends on:** C1
- **Objective:** optional inference cannot violate admission, privacy, caching or
  resource invariants when a provider misbehaves or changes.
- **Scope:** Apple/Jev/LLM scoring, digest drivers, secrets and caches.
- **Out of scope:** proving semantic summaries universally faithful.
- **Approach:** bind cache identity to available model/client identity, prompt,
  schema and the exact submitted bounded source/task projection; omitted raw
  source context and opaque model weight changes are outside that identity. Use
  bounded privacy-approved inputs and strict outputs; conservative fallback with
  explicit provenance. Exercise stale, malformed, duplicated, partial and missing
  responses and cross-process cache publication.
- **Acceptance:** secrets never appear in argv/log/error fixtures; unknown scores
  do not imply low importance; partial responses cannot erase unscored candidates;
  generated digest cannot bypass host edit bounds; no extra model calls occur
  without opt-in; cancellation reaps children and does not publish partial cache.
- **Validation:** `cargo test -p gobstopper --locked` filtered to scorer/digest/cache
  modules during development; final full gate. Mac-only Apple evidence has its
  own native owner; unavailable service tests do not count as semantic qualification.

## C11: Measurements and semantic confidence

- **Status:** Measurement foundation complete — held-out/blinded semantic qualification remains unavailable
- **Depends on:** C1, C5
- **Objective:** every metric carries units, provenance, missingness and a defensible
  interpretation; every quality claim has an appropriate study.
- **Scope:** probes/events/report/eval/study/monitor and experiment registration.
- **Out of scope:** inferring billing savings from bytes, estimates, resets or
  one-provider convenience samples.
- **Approach:** source-bound typed facts, separate literal/lexical/semantic/task
  outcomes; held-out data; blinded annotation checks; baseline, tuned native,
  masking and hybrid arms with comparable budgets. Add adversarial negation,
  superseded instruction, approval, pending side effect and tool-state cases.
- **Acceptance:** zero/unknown/reset/absent are distinct; counters cannot exceed
  denominators or wrap; events carry before/after evidence identity; incomplete
  trials remain incomplete. Report confidence intervals, sample selection, version,
  policy, refetches, latency, cache and charged usage independently. Register before
  outcomes and keep failed trials. Never treat retained substring as obeyed rule.
- **Validation:** `cargo test -p gobstopper-core --locked probe::tests`;
  `cargo test -p gobstopper --test eval_study --locked`;
  `cargo test -p gobstopper --test eval_score --locked`;
  `python3 -m unittest discover -s scripts -p 'test_*.py' -v`;
  registered synthetic study commands from the existing runner documentation.

## C12: Reproducible verification admission

- **Status:** Implemented — final exact-tree local and Linux integration gates tracked in C15
- **Depends on:** C3, C4, C6, C7, C8
- **Objective:** proof/test evidence is reproducible for the exact integrated tree.
- **Scope:** CI, pinned proof tools, artifact checksums, schema for receipts,
  deterministic corpus and bounded fuzz/nightly runners.
- **Out of scope:** weakening a complete-history or required check to save time.
- **Approach:** small required proof gates plus bounded scheduled stress; include
  checker version/hash, source/tree hash, harness/config, bounds, assumptions,
  timeout, exit code, property and cover results. Keep detailed logs as artifacts.
- **Acceptance:** model syntax failure, tool mismatch, timeout, queue not exhausted,
  unexpected counterexample and a negative control that fails for the wrong reason
  all fail admission. Mutating one intended invariant causes a known failure.
  Tools/dependencies are pinned; proof changes trigger review and the right gates.
  Security/dependency/license scans are additive; receipts never replace final CI.
- **Validation:** complete aggregate gate below; test runner failure fixtures;
  one clean Linux reproduction and one supported local platform reproduction.

## C13: Provider and platform activation matrix

- **Status:** Guarded artifact complete — no live provider cell qualified; activation remains disabled
- **Depends on:** C2–C6, C8–C12
- **Objective:** permit only operational modes with relevant current evidence.
- **Scope:** synthetic provider sessions and exact artifact/environment receipts.
- **Out of scope:** changing real user context, quotas, subscriptions or accounts.
- **Approach:** matrix of provider binary/version, account entitlement, model,
  explicit home, session ownership, OS/filesystem and feature mode. Test closed
  native, live owner delegation, copy resume, and any direct-write mode separately.
- **Acceptance:** completed provider continuation confirms source selection and
  resume shape; at least one critical-constraint task succeeds; refusal/timeout
  paths remain truthful. Account/quota failures leave cells unqualified. Version
  change invalidates only relevant cells, with conservative defaults.
- **Validation:** documented bounded synthetic runner per provider, source/vault
  pre/post hashes, process tree cleanup, matching turn evidence and usage records.
  Qualification is an activation prerequisite only for claims/modes needing it,
  not a universal condition for publishing inert artifacts.

## C14: Adversarial operation and soak

- **Status:** Implemented and independently reviewed — final bounded receipt and delivery gates tracked in C15
- **Depends on:** C3, C4, C8, C9, C12
- **Objective:** reveal long-sequence failures that finite models and happy paths miss.
- **Scope:** isolated stores/watchers, monitor fixtures and stress harnesses.
- **Out of scope:** unbounded load or destructive fault injection on user data.
- **Approach:** deterministic command sequences with restart, clock jumps, full
  disk, permission changes, corrupt objects, multiwatch contention, disappearing
  provider, delayed output, descendants holding pipes, and cache/prune pressure.
- **Acceptance:** bounded memory/disk/child count, no lost source/recovery roots,
  no retry storm, useful diagnostics, no stale locks after process death, and
  replayable minima for all failures. Set duration/capacity budgets before running;
  record coverage and untested faults rather than calling soak a proof.
- **Validation:** Hegel suites, native fixture scripts, Python monitor tests;
  exact stress commands and budgets stored in the phase receipt. One owner waits
  on each long external operation; do unrelated work without holding its lane.

## C15: Assurance case and delivery

- **Status:** Implementation and independent review complete — exact-head CI, merge and guarded-install evidence are tracked in [PR 90](https://github.com/hraness/gobstopper/pull/90)
- **Depends on:** C1–C14
- **Objective:** ship a foundation whose claims and limitations can be independently
  checked, with a maintained process for preserving them.
- **Scope:** claim ledger, docs/README, release/install evidence, operational runbook.
- **Out of scope:** expanding a claim merely because all available checks passed.
- **Approach:** independent final review maps each invariant to exact evidence;
  list trusted computing base, environmental assumptions, remaining risks and
  expiry triggers. Publish small understandable assurance cases per capability.
- **Acceptance:** no unsupported universal correctness/safety/savings claim;
  reproduction instructions work; current integration checks pass; merged tree and
  delivered binary identity verified; rollback/recovery paths validated without
  modifying user data. Versioned releases/install paths retain risky-mode guards.
- **Validation:** aggregate gate, independent review, required PR checks, exact-head
  merge, post-merge checks and applicable artifact/install health verification.

## Aggregate gate after convergence

The integration owner runs the repository-required commands once on the converged
tree. Repeat only when changes or failures invalidate evidence. CI independently
checks the merged result.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --all-features --locked
rustup run 1.85.0 cargo check --workspace --lib --bins --locked
python3 scripts/check_assurance.py
python3 -m unittest discover -s scripts -p 'test_*.py' -v
python3 verify/vault/check.py --java "$JAVA" --tlc-jar "$TLC_JAR" --output "$EVIDENCE"
```

The implemented formal gates are also required, each with a fresh output directory:

```sh
python3 verify/watch/check.py --java "$JAVA" --tlc-jar "$TLC_JAR" --output "$WATCH_EVIDENCE"
python3 verify/core/check.py --kani "$KANI" --output "$CORE_EVIDENCE"
python3 verify/transcript/check.py --lake "$LAKE" --output "$LEAN_EVIDENCE"
python3 verify/stress/check.py --output "$STRESS_EVIDENCE"
python3 scripts/check-dialects.py --cases 512 --seed 78422376057123
```

CI independently reproduces the proof gates with checksum-pinned tools. Python
negative tests for each runner are included. Site gates apply when public site
files change (`bun run check` from `site/`). A missing local MSRV toolchain is reported and may be
covered by the required exact-head CI MSRV check; it is not silently called passed.

## Decisions, remaining qualification, and stopping conditions

| Topic | Current decision | Evidence required to expand it |
|---|---|---|
| Provider custody and correlation | Direct store writes disabled; released native dispatch guarded | Pinned provider lifetime ownership/correlation, isolated continuation and critical-constraint task |
| Recovery-pin retirement | No automatic expiry; preserve unresolved and completed recovery evidence | Reviewed recovery/retention policy, model and crash tests before deletion |
| Proof tools | TLA+ for finite protocols, Kani for selected production kernels, Lean for list algebra plus finite Rust correspondence | Further production refinement and maintenance evidence before wider proof claims; Verus remains an option, not an unfulfilled prerequisite |
| Filesystems and platforms | Local macOS fixtures; Linux exact-head CI required; Windows mutation and network filesystems unqualified | Exact platform sync/lock/link and process-lifecycle evidence |
| Semantic retention | Registered synthetic tasks and explicit incomplete native arms; no universal semantic or billing claim | Domain-specific held-out tasks, reviewed labels, accepted uncertainty and qualified provider continuation |
| Uncertain dispatch | Durable no-replay blocking; reconciliation only from recorded matching evidence | Provider idempotency or authoritative operation/turn query contract before broader recovery |


Only missing authority/credentials, unavoidable interactive authentication,
material product decisions, unsafe out-of-scope destruction or an unresolvable
release failure require user input. Other bounded repairs and ordinary delivery
continue under standing authorization. A failed proof yields a counterexample to
resolve or an honest bounded claim; do not weaken the property just to turn green.

## Implementation log

- 2026-09-23: Execution resumed after PR #88 (`ffc7148`) on branch
  `codex/correctness-foundation`. C1 implementation is active; C2/C3 investigations
  run in parallel without downstream edits. Source delivery does not activate
  unqualified live-provider mutation. Phase completion requires its recorded
  implementation, independent review and acceptance evidence.
- 2026-09-23, C1: Added the assurance ledger, 16 coverage/invariant rows,
  30 effect profiles, all 33 CLI commands/10 MCP tools/7 hooks/8 scripts and
  188 conservative Rust callable classifications. Individually triaged 21
  baseline scanner alerts; removed seven credential/background disclosure sinks
  and corrected public claims. Structural checker and 13 negative/coverage tests
  pass; CLI acceptance has 160 passing tests; site gate has 31 source/unit tests
  plus one built-runtime test, with the existing image lint warning. Independent
  review verified scanner excerpts, identities and model hashes, and approved
  C1. No scanner closure, live qualification or whole-system theorem is claimed.
- 2026-09-23: C1 checkpoint `497afb3`; C2, C3 and C8 implementation admitted
  in disjoint lanes. Root owns CLI/shared interfaces, plan and inventory updates.
- 2026-09-23, C2: Disabled direct provider writes and removed SQL/file mutation
  implementations; retained bounded byte transforms and no-clobber publication.
  Independent review found and repaired blocked-event projected savings. Focused
  gates passed Devin32, eval11, study6, compacted8, audit20, custody2, CLI replay6,
  benchmark compile, and all 26 Hegel properties (the fork property was updated
  for C3 exact-operation idempotency and rerun). Synthetic two-process and foreign
  store/graph tests leave source/WAL/SHM state unchanged. Native activation remains
  separately unqualified. Adapter receipts: `gobstopper-audit-evidence/2026-09-23/c2`.
- 2026-09-23, C8: Deterministic MCP inspection rejects external authority before
  planning, bounds/validates frames and arguments, and binds vault lookup scope.
  Plugin execution captures declared hash-bound artifacts and uses a bounded
  nonblocking reactor with owned-group cleanup. MCP integration4, plugin11,
  MCP unit12 and config7 pass; independent source review accepted the lane.
  Trusted interpreters/loaders/explicit external paths remain outside the
  capability check; this is not an OS sandbox. C4 and C5 implementation admitted.
- 2026-09-23, C3: Durable v2 receipts pin exact recovery and candidate bytes;
  recovery never reruns a transform. Added strict root decoding, Reader custody,
  scoped append-order retention, truthful publication errors, and native recovery
  pins. Independent review repaired duplicate-field admission and unintended
  existing-directory permission changes. Storage15, fork9, vault14, recovery10,
  Python reader7 and earlier study10 focused tests pass. The eleven-case model
  receipt is `gobstopper-audit-evidence/2026-09-23/c3-model-qualified/receipt.json`:
  custody25,810 and publication28,082 distinct states, exhausted safe queues,
  nine intended counterexample/witness checks. SQL commit fault injection is
  inapplicable because C2 removed direct SQL mutation. Process death and returned
  I/O faults establish their declared boundaries, not power-loss guarantees.

- 2026-09-23, C4/C9: Durable version 1 native journal, exact canonical target
  locks and generation 9 conservative watcher state are implemented. Unresolved
  dispatch never expires or falls back to surgery. Codex matches session/turn/item
  while first-item association remains an explicit protocol assumption. Hook
  settings now export private no-clobber candidate bundles only; callbacks archive
  exact bounded input and report unattributed zero-savings observations. Independent
  reviews accepted both lanes. Native journal9, watch24, hooks24 and custody4
  focused fixtures passed. The 13-case final model receipt is
  `gobstopper-audit-evidence/2026-09-23/c15-watch/receipt.json`: 32,251 exact-contract
  and 24,609 assumption-only distinct safe states; five mutants and six witnesses.
- 2026-09-23, C6/C7: Implemented production Kani kernels (15 harnesses, 159
  assertions, 46 satisfied covers and exact boundary mutant) and 27 Lean theorems
  with fresh kernel replay/axiom audit. Independent Lean vectors exercise 69 actual
  Rust steps across three dialects and kill both oracle and Rust lowering mutants.
  Integration owner reviewed the proof bodies and mutation admission. These prove
  their stated kernels/list algebra and finite correspondence, not semantic
  equivalence or proprietary provider behavior. Exact-source receipts are refreshed
  after C14 event-log hardening; earlier passing receipts remain historical.
- 2026-09-23, C12/C13: Added pinned Java/TLC/Kani/Lean archives, bounded download
  and proof runners, expected-failure admission and retained CI evidence. Independent
  runner review found and repaired a trickle-read deadline gap. No live native
  cell is qualified. The reviewed release guard blocks all CLI native dispatch,
  including old auto_compact_closed configurations, before provider execution.
  Standalone native apply also refuses before fork/snapshot effects; watchers may
  preserve a recovery snapshot before guard admission. Exact isolated debug
  fixtures are the only development exception.
  qualification.json preserves future continuation/custody/critical-task criteria.
  Guarded artifact admission does not silently complete those live criteria.
- 2026-09-23, C14: A fixed 64-step snapshot/copy/prune/recover sequence with
  16 corruption/restore cycles found read-only vault listings silently skipped
  damaged history. Strict index admission now rejects those reads; the repaired
  sequence passed in 28.31 seconds, peaking at 402 files and 334,635 bytes (limits:
  90 seconds between steps, 1,200 files, 16 MiB). Returned write/sync failures cover
  all seven native journal states; they are not physical power-loss evidence.
  Wider review found event-log FIFO/growing-input hazards and monitor foreign-store
  joins; bounded repairs and final stress receipt are in progress.
- 2026-09-23, C15: Reconciled current source effects, compatibility and public
  claims; added exact-source summarized proof receipts with raw receipt identities.
  Structural admission now checks production proof-tool helpers and rejects stale
  source hashes, failed/duplicate cases and changed input receipts. Public docs/site
  explain disabled mutation and qualification limits. Final aggregate, independent
  whole-feature review, Linux CI, merge and guarded installation remain delivery gates.

- 2026-09-23, independent final review: Repaired reused-object durability
  confirmation before vault references; bounded Devin locks/database admission,
  Claude liveness metadata, configuration and direct-path sniffing; bound undo
  snapshot bytes to the selected session/hash/size before effects. Replay rejects
  raw SQLite archive images while preserving archive bytes. Evaluation freezes
  database exports, validates policy overrides and retains failed benchmark rows;
  benchmark artifacts are private and no-clobber. Monitor/CLI retention requires
  exact non-conflicting pairs and qualified actions. Receipt admission now checks
  complete stage/tool/input inventories, exact mutants, Kani coverage counts and
  stress resource envelopes. Final review and local receipts are retained under
  `gobstopper-audit-evidence/2026-09-23/`; current normalized proof receipts are in
  `docs/assurance/receipts/`. CI includes all six required gates and weekly bounded
  stress. No receipt authorizes live provider activation or proves whole-system
  semantic correctness.
- 2026-09-23, final adaptive-history review: Past-yield tuning now requires
  exact canonical store identity and valid paired context observations, uses
  measured positive reduction, and excludes replay/conflicts, legacy estimates,
  unknown/error/reset usage and foreign same-ID stores. The isolated regression
  covers all admission and ordering branches. This remains a heuristic, not a
  billing or semantic-success measurement.
- 2026-09-23, final local admission: All 504 Rust tests and doctests passed
  without ignored tests, along with formatting, Clippy and the Rust 1.85.0
  compatibility check. All current TLA+/Kani/Lean source receipts are refreshed;
  the site aggregate passed. Required CI independently reproduces admission on
  the published integration candidate. Final branch/head, merge and guarded
  artifact-install outcomes are recorded in PR 90 and the retained delivery
  receipt, rather than predicted by this source document.

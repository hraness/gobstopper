# Runtime measurement and overhead improvements

## Outcome

Repair the concrete gaps found in the September 24 local-data review: useful
partial usage accounting, valid long-history ancestry, early refusal of disabled
native work, explicit monitoring coverage, trustworthy study outcomes and event
aggregation, and bounded storage accounting. Deliver a checked source build and
verify its installed behavior without activating unqualified provider mutation.

The baseline is `eb476a7771367c2a09ecf6299962a496eced005f`. The private review and
frozen inputs are retained at
`gobstopper-audit-evidence/2026-09-24/data-review/assessment.md`. Its nine current
monitor passes establish a short observation window, not sustained reliability.

## Constraints and delivery

- Preserve provider bytes, recovery pins, uncertainty and all activation guards.
  Do not prune live data, enable native calls, run paid studies, or call private
  providers to close a software test. New study protocols are offline artifacts;
  held-out behavioral and live-provider qualification remain separate evidence.
- JSON changes are additive. Partial numeric components never become a complete
  context or savings claim. Unknown historical source/config/cohort stays unknown.
- Use bounded, source-bound reads and no-follow file admission. A successful
  command is separate from complete provider coverage and from effectiveness.
- One integrator owns this plan, shared schemas outside the explicitly assigned
  usage model, CLI report/monitor integration, manifests, CI, proof inventories,
  normalized receipts, commits and delivery. Workers own focused tests and do not
  commit. Shared-file handoffs are explicit, never simultaneous edits.
- No installed host scheduler was found in the documented command locations.
  Use repository-native bounded commands and `--jobs 2` for Rust validation;
  do not install retired global wrappers. Keep one owner per expensive command.
- Follow repository review and all six required CI gates. Merge only the checked
  candidate, validate merged source, build the release artifact, preserve the old
  artifact/state, and restart only the four admitted Gobstopper jobs. Record
  exact source/binary/monitor identities and fresh service observations. The
  repository has no package release workflow; do not invent a release tag.

## Phase map

| Phase | Outcome | Depends on | Write owner / scope | Parallel with |
|---|---|---|---|---|
| D1 | Partial usage and complete bounded ancestry | none | usage worker: core/model, adapters Devin/Claude, focused fixtures | D2, D3 |
| D2 | Cheap disabled-watch refusal | none | watch worker: CLI main/native/watch tests | D1, D3 |
| D3 | Honest probe criteria and study claims | none | study worker: provider probe/tests, design note, offline protocol | D1, D2 |
| D4 | Consistent evidence and visible coverage | D1, D2 API handoff | integrator: core/events, CLI report/hooks, adapter discovery, monitor | D3; handed-off D5 |
| D5 | Bounded vault accounting | D2 handoff for CLI entry point | storage worker: adapters vault/accounting, CLI integration by owner | D4 |
| D6 | Formal correspondence, review and delivery | D1–D5 | integrator, independent reviewer | independent checks |
| D7 | Discovery and monitor overhead | D6 observation record | integrator: adapter discovery, Devin reader, monitor deadlines | owner allowlist refresh |

## D1: Usage and ancestry

- **Status:** Complete
- **Depends on:** none
- **Objective:** retain measured components when Devin optional metrics are null;
  recover valid Claude usage beyond the discovery tail without accepting broken
  branch structure.
- **Scope:** `core/src/model.rs`, adapter `devin.rs`, `claude.rs`, relevant fixtures
  and narrowly required usage-literal migrations by explicit handoff.
- **Out of scope:** assuming null means zero, enabling mutations, or provider calls.
- **Approach:** keep complete context separate from a partial component subtotal
  and closed reason. Use a bounded full structural projection or source-bound
  incremental ancestry index; preserve duplicate/cycle/interior-parent checks and
  conservative behavior on concurrent source changes.
- **Acceptance:** null/missing/zero/malformed/overflow/reset have explicit semantics;
  partial evidence is never returned by the complete-context accessor; valid long
  Claude history yields current usage; invalid ancestry still refuses; memory/I/O
  limits and lifetime scope remain explicit.
- **Validation:** `cargo test -p gobstopper-core --locked model::accounting_tests`;
  `cargo test -p gobstopper-adapters --locked devin::tests`;
  `cargo test -p gobstopper-adapters --locked claude::tests`.

## D2: Early native refusal

- **Status:** Complete
- **Depends on:** none
- **Objective:** released native-disabled watchers do no export/snapshot work to
  discover that dispatch is unavailable.
- **Scope:** CLI `main.rs`, `native_operations.rs`, `tests/watch.rs`.
- **Out of scope:** weakening final admission, erasing uncertain state, live native
  activation or changes to unrelated planning strategies.
- **Approach:** early artifact eligibility before fallback transcript loading and
  snapshot where the policy already determines a native-only route; retain final
  checks. Record an expected blocked decision and suppress unchanged repeats with
  artifact/policy/source-aware invalidation; uncertainty always takes precedence.
- **Acceptance:** repeated release refusal leaves source, vault/index and journal
  untouched; no provider call; no needless export; configuration/artifact changes
  invalidate only terminal suppression, never unresolved holds. Expose enough
  bounded checkpoint information for heartbeat and decision monitoring.
- **Validation:** `cargo test -p gobstopper --test watch --locked`;
  `cargo test --release -p gobstopper --test watch --locked unqualified`.

## D3: Experimental result integrity

- **Status:** Complete
- **Depends on:** none
- **Objective:** every response shape produces an honest result; declared rule
  criteria govern their own recorded outcome and overall success.
- **Scope:** `scripts/provider-retention-probe.py`, provider-free tests,
  `docs/design.md` experimental claim, new registered offline study protocol.
- **Out of scope:** paid/provider runs, retrospective regrading without provenance,
  claims of behavioral enforcement from answer recall.
- **Approach:** validate object/types, persist explicit invalid response outcomes,
  separate literal facts/markers/behavior. Register independent held-out labels,
  control arms, size/malformed strata, immutable assignments and full denominators.
- **Acceptance:** correct facts/missing markers fails the registered marker arm;
  list/string/number/bool/null/malformed answers save results without escaping;
  no real provider process in tests. Documentation describes confounded exploratory
  outcomes accurately. New protocol does not claim execution or qualification.
- **Validation:** `python3 -m unittest discover -s scripts -p 'test_provider_retention_probe.py' -v`;
  existing experimental-runner tests and structural assurance after integration.

## D4: Evidence and monitoring

- **Status:** Complete
- **Depends on:** D1/D2 API handoff
- **Objective:** consumers agree on qualified evidence and distinguish command
  health, selected coverage, measurement scope, watcher freshness and activation.
- **Scope:** core event qualification/reader, report and hook provenance, adapter
  discovery diagnostics, monitor output/tests/docs. Main changes only after D2.
- **Out of scope:** widening monitor execution authority, retrospective invented
  build/cohort identities, or turning missing data into zeros.
- **Approach:** shared conflict-aware pair qualification, closed compaction action
  checks, canonical source identity, additive decision-time config/build/cohort
  fields and malformed/omitted counts. Export partial usage and coverage/caps;
  read bounded watcher checkpoints and policy fingerprints without extension calls.
- **Acceptance:** permutation/replay invariance, conflicts excluded, foreign stores
  isolated, historical cohort assignment stable under changed current config;
  missing legacy provenance explicit. Monitor flags empty selected overlap,
  discovery/measurement gaps and stale heartbeat while retaining isolated dry-run
  policy and unchanged deadline. No private freeform output in new diagnostics.
- **Validation:** focused core event and CLI report/hook tests;
  `python3 -m unittest discover -s scripts -p 'test_monitor.py' -v`;
  `cargo test -p gobstopper --test report --locked`.

## D5: Storage accounting

- **Status:** Complete
- **Depends on:** D2 CLI ownership handoff
- **Objective:** inspect storage magnitude and deduplication cost without reading
  all object content or acquiring exclusive prune custody.
- **Scope:** bounded vault metadata accounting with CLI read-only JSON access;
  monitor integration only if its existing deadline and cost envelope permit.
- **Out of scope:** deletion, automatic pruning, physical APFS usage claims, or
  unverified reclaimable-byte claims.
- **Approach:** nonblocking shared custody, bounded metadata walks and index scan;
  explicit completeness, logical bytes, origin counts, references/pins and limits.
  Reclaimability stays unavailable unless complete validated evidence establishes it.
- **Acceptance:** busy/malformed/symlink/limit cases are explicit; no object-content
  scan or data mutation; no invented disk or growth figures. Recovery/pin safety
  remains unchanged and tests establish bounded interruption/cleanup.
- **Validation:** focused adapter storage tests and CLI accounting integration test;
  read-only live metadata comparison only after source review.

## D6: Proofs, review and delivery

- **Status:** Complete except the post-bootstrap receipt, which is an owner action; see the
  [delivery record](assurance/data-delivery-2026-09-24.md)
- **Depends on:** D1–D5
- **Objective:** tie changes to regression and formal evidence, merge and verify
  installed behavior, and retain exact delivery records.
- **Scope:** affected Kani/TLA+/Lean production correspondence, inventories,
  assurance documentation/receipts, final integration and service handoff.
- **Acceptance:** independent review has no unresolved blocking findings; semantic
  negative controls still fail for intended reasons; no partial-as-complete proof
  claim; all six exact-head CI gates and merged-source verification pass. Installed
  binary matches measured source; original configuration and recovery state remain;
  fresh usage/coverage/blocked-work observations match the new contract.
- **Validation:** fmt; Clippy `--workspace --all-targets --all-features --locked -- -D warnings`;
  all Rust tests/doctests; MSRV 1.85; all Python/dialect tests; `check_assurance.py`;
  exact affected proof runners and bounded stress; site copy/site gates as applicable;
  release-only refusal tests; independent main CI and guarded installation checks.

## D7: Discovery and monitor overhead

- **Status:** Planned
- **Depends on:** D6 observation record
- **Objective:** keep watcher passes and monitor commands inside their budgets on
  a busy host without weakening bounded reads, custody or the shared deadline
  contract by an ad hoc skip.
- **Scope:** adapter discovery cache validity, Devin per-pass read cost, monitor
  command deadlines, and the launch job's monitor allowlist.
- **Out of scope:** raising deadlines to admit slow passes, widening discovery
  windows, modifying or checkpointing the Devin store, or any native activation.
- **Approach:** treat a fingerprint-equal cache entry (length, mtime, device,
  inode, ctime) as valid beyond the 60-second sample interval so unchanged files
  are not reread each pass; skip the Devin read snapshot or bound its bytes when
  a session's store rows are unchanged; give report and dry-run watch separate
  deadlines or run the cheaper command first; refresh the allowlist with active
  session IDs through the owner's launch job change.
- **Acceptance:** a warm second pass over an unchanged corpus reads no transcript
  bytes; a changed file is still rescanned on any fingerprint difference; a slow
  report cannot starve watch; selected overlap is non-empty for an active
  allowlist. Existing detect, watch, Devin and monitor tests pass unchanged.
- **Validation:** focused detect/Devin/monitor tests with cold and warm cache
  fixtures; the six CI gates; a fresh read-only observation window after install.

## Implementation log

- 2026-09-24: D1–D3 started in disjoint implementation lanes. Main remains at
  baseline `eb476a7`; branch `codex/data-quality-and-overhead` owns this follow-up.
  The integration owner handles shared reporting and delivery.

- 2026-09-24: D1 focused model/Devin/Claude/measurement checks passed 5/39/7/5
  tests. Private receipt: `/private/tmp/gobstopper-d1-focused-receipt.json`.
- 2026-09-24: D2 watch suite passed 26; artifact identity and affected heartbeat
  checks passed; release unqualified tests passed 4. Independent review found
  control-cohort ordering; root repaired it and a regression is pending.
- 2026-09-24: D3 independent review fixed exponent-overflow JSON, spoofed native
  boundary records, and the fourth rule marker. All 22 provider-free probe tests
  passed. Study-runner tests passed 11. The held-out study remains unexecuted.
- 2026-09-24: D4 monitor suite passed 34 before follow-up strict checkpoint and
  coverage parsing. CLI compile passed; shared event/report review in progress.
- 2026-09-24: Independent D1 review repaired an absent selected-leaf graph;
  final Claude/measurement checks passed 8/5. D5 metadata checks passed 12 and
  the CLI checks passed 2, including the final cooperative elapsed-budget check.
- 2026-09-24: D2 independent review passed after control-cohort ordering,
  no-export live delegation and bounded checkpoint-save repairs. Release
  unqualified checks passed 5; the oversized-save regression preserved the
  previous readable uncertainty bytes.
- 2026-09-24: D4 independent review passed after strict malformed measurement,
  provenance, source identity and quiet-live discovery repairs. Focused checks:
  events 15, discovery 3, prompt-policy hooks 7, report units 16, telemetry 1,
  report CLI 4, monitor 37. A follow-up detect serializer handles non-UTF8 paths
  explicitly and displays unknown usage as unknown; aggregate validation and
  final review include that patch.
- 2026-09-24: D6 source converged for aggregate validation. The stress inventory
  requires 155 exact named tests. Kani now has 16 harnesses and binds the added
  production accounting/event callers; fresh formal receipts are pending.
- 2026-09-24: Full validation found a frozen Claude dialect regression: a
  permitted unlinked attachment invalidated the selected message branch. The
  projection now preserves that protected attachment and the existing frozen
  branch contract; discovery/full-load parity is included in a new regression.
- 2026-09-24: Final review reproduced a malformed-event reader gap: dropping
  an invalid duplicate before qualification preserved the other record's claim.
  D4 is reopened to reject qualification from incomplete event histories across
  reports, adaptive history, cohort/retention readouts and the independent monitor.
- 2026-09-24: Reopened D1/D4 repairs passed independent review. Focused
  checks passed Claude 9, core events 15, report CLI 5 and monitor 40. Strict
  event reads now refuse incomplete histories; diagnostic reports retain counts
  with explicit unavailable qualification. The stress inventory now requires
  158 named tests. Final aggregate and fresh Linux receipts remain pending.
- 2026-09-24: Main advanced through site-only PR 93; integration preserves those
  changes. This task changes no site source or deployment target.
- 2026-09-24: Integrated local gate passed: 558 Rust tests, doctests (no
  examples), fmt, Clippy, MSRV 1.85 and the separate 512-case dialect replay.
  All Python and verification-runner checks passed. Fresh Linux evidence passed
  24 TLA+ configurations, 16 Kani harnesses (178 assertion checks: 148 successful
  and 30 unreachable safety checks; 52 satisfied covers), 27 Lean laws with
  69 Rust correspondence steps, and 158 stress tests. Exact intended negative
  controls passed. New normalized receipts bind current source hashes.
  Draft PR 94 now proceeds to final exact-candidate CI and merged-source checks.
- 2026-09-24: Candidate CI passed quality, MSRV and all three formal jobs, but
  the serial watch stress suite reached its 180-second limit near its final
  tests. The unchanged suite passed all 28 tests in 152 seconds locally and
  previously in 163 seconds on Linux. Reviewed startup hashing explains the
  added debug-test cost; focused and timed full reruns found no hang. The watch
  budget is now 240 seconds, with all 158 tests and the 900-second aggregate
  bound preserved. Fresh stress evidence and final candidate CI are required.
- 2026-09-24: Independent budget review passed. Fresh macOS stress evidence
  passed all 158 tests in 288 seconds (watch: 148 seconds), and every saved log
  was readmitted against the exact inventory. The new `2026-09-24-stress-r2`
  receipt preserves the earlier Linux record as history. Assurance inventory and
  its negative controls pass; final Linux CI remains a separate required gate.
- 2026-09-24: The next Linux run passed bounded stress and all formal gates,
  but parallel quality tests found accounting custody still held after a scan.
  Closing a file alone can retain its lock through a duplicated or fork-inherited
  descriptor. A focused repair adds explicit release for accounting's root and
  index custody, with a deterministic duplicate-descriptor regression. The exact
  concurrent child in the failed run was not observed. Final source review,
  aggregate checks and affected Lean/stress evidence must be refreshed.
- 2026-09-24: The accounting repair passed independent source review, the full
  adapter library (158/158), and the new 13-test accounting slice. Fresh pinned
  Lean correspondence and the bounded stress run passed against the repaired
  source; stress now admits 159 exact tests and completes 159/159 locally. The
  converged macOS workspace aggregate reached format, Clippy and the existing
  workspace tests but its sequence test exceeded its 90-second internal bound
  at 93–103 seconds on this host; the same exact sequence passed in the fresh
  bounded run in 38 seconds. Linux candidate CI remains the authoritative
  aggregate gate.
- 2026-09-24: PR 94 merged as `7ff13e3`; merged-source CI passed all six jobs.
  The checked binary (`31a5b3ea…`) and monitor (`114b23f4…`) were installed
  under the guarded cutover; the four launch jobs restarted at 17:06 UTC with
  the configuration restored from its exact bytes. Details and hashes are in
  the [delivery record](assurance/data-delivery-2026-09-24.md).
- 2026-09-24: Read-only readiness evaluation. Eleven monitor instants between
  17:11 and 17:36 UTC met the post-bootstrap criteria; 497 events since the
  restart carry the installed hash and match the early-refusal contract. The
  protocol's receipt step was declined by the session's permission reviewer and
  remains an owner action; no other route was used.
- 2026-09-24: Observation window 17:06–17:50 UTC: 14 of 28 monitor observations
  fully healthy, 14 dry-run watch timeouts under the shared 45-second budget.
  Timeouts began at 14:00 UTC under the previous binary on a saturated host
  (load 25–35, volume 99 percent full, Devin appending ~9 MB/s). Cold-cache
  rescans of 1,961 Codex transcripts (~1.08 GB per pass) and Devin page reads
  of a 17 GB store dominate the stack samples. D7 records the follow-ups; the
  stale 14-session monitor allowlist and the 26.4 GB Devin write-ahead log are
  owner items. No product code changed for these findings.

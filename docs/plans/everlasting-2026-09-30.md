# Gobstopper: context resilience and local session visibility

Requested September 30, 2026. Starting commit `650e478`; branch `codex/everlasting-gobstopper-20260930`.

## Delivery scope

1. Correct request semantic identity and retain bounded original observations across compaction and restart. Preserve original transcripts and provider tool linkage. Root integrates; context worker implements and tests adapters.
2. Add explicit, temporary context reservations with provider/client/output limits; capability-scoped request binding; bounded, optional rescue on unchanged evidence rereads after eviction. Root owns control state and proxy integration.
3. Add portable local observations, transactional version migrations, idempotent import/export, pure metric calculations, crash/incomplete-request accounting and CLI inspection. Data worker owns the module and tests; root wires live collection.
4. Harden user startup services with identity, readiness, rollback, diagnosis and repair. Prevent idle system sleep only during active inference; release on completion, cancellation and process death. Service worker implements lifecycle/power; root wires proxy.
5. Independently review integrated behavior, run required repository and platform checks, repair findings, publish a reviewed release, install and verify the owned local service without interrupting active inference. Preserve unrelated checkouts and production data.

No cloud service or leaderboard publication. AI Charts is a design reference, not a dependency. Raw prompts, reasoning, tool arguments/results, paths and credentials do not enter the observation database. Unknowns remain unknown. Workload evidence cannot establish improved model task accuracy without a corresponding task experiment.

## Validation

Focused adapter regressions cover semantic cache changes, multiple compactions, full versus excerpted observations, multimodal limits, provider pairing, and deterministic reconstruction. Control regressions cover capacity, expiry, concurrent allowance, scope separation, secret stripping and adaptive positive/negative controls. Local upstream stubs cover stream timing, usage, retries, disconnects, incomplete streams and power lifecycle. Data regressions cover migration, transactional crash recovery, deduplication, import/export, backup and mathematically valid metric denominators. Service tests cover generated platform definitions, identity, edited files and rollback; live qualification is recorded separately by platform.

Required final checks follow `.github/workflows/ci.yml`, assurance records and `docs/release.md`. Final evidence includes the integration tree, independent review, exact commands/results, PR, CI, merge, immutable release, installation and service health.

## Status

- Investigation complete; contracts agreed.
- Implementation complete in four shared-tree lanes; original checkout preserved.
- Independent reviews repaired capacity bypasses, reservation races, response classification, concurrent first startup, Windows recovery and disabled evidence budgeting. Source frozen for final proof records.
- Aggregate Rust validation: 834 tests passed, three explicit native power tests ignored by the ordinary run (separately exercised on macOS); strict workspace Clippy, doctests and 512-case dialect replay passed.
- Site validation: 168 tests, lint, type checks, 24-page production build and three postbuild runtime checks passed.
- Fresh proof records passed: core 178 assertions/54 covers plus the required mutant; transcript 14 checks including negative controls; stress 151 tests in 11 suites. Assurance inventory check passed.
- Python checks passed: 225 tests across scripts, benchmark packaging and verification runners, plus validation of nine public benchmark artifacts. The exact CI minimum-version check passed on Rust 1.85.0 (`cargo check --workspace --lib --bins --locked`).
- [PR #205](https://github.com/hraness/gobstopper/pull/205) opened. Initial CI passed all checks except Windows context-store contention. Follow-up repairs use read-only schema reopening, bounded writer waits, native Windows data paths and recovery after startup storage failure. Independent review and focused regressions pass; refreshed aggregate (834 Rust tests, strict Clippy, doctests and 512-case replay) and final native macOS power checks pass; fresh integration CI pending.
- Release preflight: reused the user-authorized Apple signing setup. Certificate identity and non-publishing notarization authentication passed; all five secrets are configured in Gobstopper’s existing `hraness-apple-release` environment, restricted to `v*` tags. Actual release notarization and artifact verification remain pending.
- Existing local proxy is 0.7.2 under the legacy LaunchAgent with active connections. First migration must wait for an idle maintenance interval because this version cannot atomically drain; active inference will not be interrupted.
- Integration CI at `d64ddc1` passed every independent job except one Windows observation test. Startup contention passed. The failure exposed idle observation queues that had no retry owner after a temporary database write failure; review also found current-schema queries unnecessarily taking writer locks. An owned retry worker and coherent read-only schema reopening repair both paths. Independent review passed; 15 recorder tests, 150 repeated recorder checks and 27 data-store tests passed.
- A read-only local upgrade check found that legacy implicit `keep_tail_percent=40` would otherwise become the new default of zero. Migration now preserves the observed effective value in both the managed service and separately prepared rollback definition. Original backups stay immutable; interrupted recovery preserves busy processes and retains its journal, verifies identity and idle activity before restarting, and requires the prepared setting at readiness. Independent review and all 24 focused service tests passed. The live legacy service remains unchanged.
- A private exercise on two completed audited transcripts imported 424 observations (six Codex and 418 Claude), inserted zero duplicates on repeated native imports, passed integrity checks, and preserved metrics through portable export/import. Original transcripts and the production database were untouched.
- Final integration after observation and migration repairs: `cargo fmt --all -- --check`, strict workspace Clippy, CI-pinned nextest 0.9.146 (850 passed, three opt-in native power tests skipped), workspace doctests and the 512-case dialect replay all passed. Refreshed stress evidence passed 151 tests across 11 suites with unchanged inputs; the current receipt is `2026-09-30-080-everlasting-final-recovery-stress.json`.

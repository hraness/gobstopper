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
- Aggregate Rust validation: 826 tests passed, three explicit native power tests ignored by the ordinary run (separately exercised on macOS); strict workspace Clippy, doctests and 512-case dialect replay passed.
- Site validation: 168 tests, lint, type checks, 24-page production build and three postbuild runtime checks passed.
- Fresh proof records passed: core 178 assertions/54 covers plus the required mutant; transcript 14 checks including negative controls; stress 159 tests in 11 suites. Assurance inventory check passed.
- Python checks passed: 225 tests across scripts, benchmark packaging and verification runners, plus validation of nine public benchmark artifacts. The exact CI minimum-version check passed on Rust 1.85.0 (`cargo check --workspace --lib --bins --locked`).
- Local validation complete. PR, fresh integration CI and merge pending.
- Release preflight: GitHub environment, repository and inherited organization signing-secret lists are empty. Local code-signing identity discovery reports no valid identity. Apple credential setup is required by the existing release gate; no unsigned release workaround is permitted.
- Existing local proxy is 0.7.2 under the legacy LaunchAgent with active connections. First migration must wait for an idle maintenance interval because this version cannot atomically drain; active inference will not be interrupted.

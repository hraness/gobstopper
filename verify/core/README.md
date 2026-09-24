# Proofs over production Rust kernels

The `#[cfg(kani)]` harnesses are adjacent to the **same functions called by the
program** in `gobstopper-core`. They do not replace production capacities, swap
in a toy implementation, stub dependencies, or constrain invalid inputs away
with assumptions. Kani checks memory safety, arithmetic, assertions and loop
unwinding with the default safety checks enabled. This is deliberately a small
production-connected proof surface, not a claim that every strategy, adapter or
the complete application is verified.

## Scope and production use

| Kernel / harness family | Production callers | Input scope and assertion |
| --- | --- | --- |
| `admission::plan_bounds` | `validation::validate_edits` | All platform `usize` values; real limits 64 edits / 100,000 items |
| `admission::add_digest_bytes` | Digest field accumulation in `validate_edits` | All two-`usize` inputs; exactly reject totals above 32,768 or overflow |
| `admission::is_elidable`, `target_allowed` | `TranscriptItem::is_elidable`, all core candidate/tail filters, adapter projection gate, validator | All `u64`, `Option<u64>` and protection flags; require live positive payload and exclude protected content |
| `admission::unprotected_len` | Core candidate suffix boundaries | All two-`usize` inputs; no underflow and retained suffix excluded |
| `EditAdmission::admit` | Every edit in `validate_edits` | All internal scalar states, including invalid states, and all four edit kinds; accepted invariant, one digest, exclusive controls, rejection identity |
| `edit_sequences_four` | Same `EditAdmission::admit` | Four arbitrary edit kinds from valid empty state; invariant after every prefix; production capacity remains 64 |
| `strictly_increasing`, `find_index` | Sorted physical-identity table in `validate_edits` | Separate lengths 0, 1, 2 and 4, arbitrary full-width IDs/target; duplicate/reverse detection, no invented identity, exact lookup for ordered input |
| `estimate_token_bytes` / `estimate_tokens` | Adapter byte estimates, model and digest accounting | Full `u64` / platform `usize`; independent `u128` rounding oracle including maximum and zero |
| `estimate::add_tokens` | Model totals, auto preserved-prefix totals, structured strategy savings, digest overhead | All two-`u64` inputs; independent wide saturating-sum oracle |
| `estimate::elision_savings` | `TranscriptItem::estimated_elision_savings` | Full `u64`, optional bytes and `u32` part count; no overflow/truncation, live payload required, saving no larger than item estimate |
| `UsageSample::observe_cumulative_report` | Codex cumulative usage ingestion | Arbitrary old/new scalar state and all provenance enums; preserve absent reports, distinguish reported zero from explicit reset/unavailable context, cumulative input establishes full accounting, cached <= known input, positive window updates, idempotent repeated report |
| `UsageSample::observe_context_components` | Devin metrics and Claude usage projection | Optional full-width components with an absent or explicit null reason; partial numbers never establish complete context, overflow remains unavailable, complete zero is measured |
| `EvidenceAgreement::join` | Shared event-pair qualification for reports and adaptive history | Arbitrary states and full-width scalar evidence; commutative, associative, idempotent, absorbing conflicts; JSON parsing, collection/key construction and evidence equality on records remain regression-tested |
| `policy::validate_policy` | CLI `config::validate_policy` | Full integer and optional `f64` domain, including NaN/infinity; exact policy admission boundaries |

Every harness requires reachable boundary/branch covers, including real maximum
capacities, `usize::MAX`, `u64::MAX`, invalid internal state, reported zero / explicit reset and NaN
rejection. In particular, the one-step admission proof covers count 63 -> 64 and
refusal at invalid full-width counts. The four-edit sequence bound does not
stand in for the production capacity proof. It covers composition prefixes only.

The ordered-index proofs compare binary search against an independent linear
oracle and ordering against all earlier pairs. Unsorted inputs are still memory
safe and cannot produce an invented match; exact absence is claimed only when
ordering holds. Production sorts copied references and validates strict order
before lookup. Sort correctness, allocation, hash sets for selected/tool IDs,
string/JSON decoding and the rest of `validate_edits` remain tested trusted
boundaries; these harnesses do not symbolically execute the complete validator.
Provider projections own supported payload shape and branch authority. The
core eligibility predicate does not prove those facts from bytes.

The production changes also remove unchecked accumulation in auto prefix totals,
structured savings and digest overhead, refuse zero-byte/dead elision candidates,
cap per-item estimated savings, and normalize inconsistent cumulative cache
reports. Ordinary core regressions exercise the real capacity, physical IDs,
duplicate/protected targets, unchanged rejected input, full-width totals and
usage resets. The proof is about numeric accounting, not actual tokenizer cost,
semantic preservation of lossy summaries or provider billing guarantees.

## Toolchain and admission

The reviewed toolchain is Kani **0.68.0**, CBMC **6.11.0**, bundled
`rustc 1.100.0-nightly (8925ea358 2026-08-20)` (`nightly-2026-08-21`). The runner
requires those versions and records the installed executable hashes. Platform
target and `usize` width are recorded in Kani JSON; a run proves that target,
not every Rust target. Cross-platform release qualification remains separate.
The full installation, standard-library models, compiler, solver, Python and
host remain trusted tools; observed binary hashes alone are not a signed supply
chain attestation or a machine-checked proof of the compiler.

```sh
python3 verify/core/test_check.py
python3 verify/core/check.py --kani /absolute/path/to/cargo-kani --output "$NEW_EVIDENCE_DIR"
cargo test -p gobstopper-core --locked
```

The matching installed bundle must be at the shim's documented default
`~/.kani/kani-0.68.0`; inherited `KANI_HOME` and injected proof/compiler flags are
removed for this check. No installation or global configuration change is made.
Cargo runs offline, so dependency setup must already exist. The output directory
must be new. The production invocation is `cargo-kani -p gobstopper-core
--output-format terse -Z unstable-options --export-json ... --harness-timeout 60s
--target-dir ...` with no disabled checks. The aggregate process has a 15-minute
limit and 32 MiB log bound, using the bounded Unix reactor in
[`../watch/check.py`](../watch/check.py); that dependency is evidence-hashed.

The 16 named harnesses use bounds 2/3/4/6 for empty/one/two/four-index cases and 5
for four-edit composition. Scalar kernels have no input-length bound. Evidence
agreement is proved for scalar equality; its use with complete observation
records relies on Rust-derived equality and the tested pair-key construction. Every
reported unwind check must succeed; missing harnesses, timeouts, unsupported
reachable behavior, undetermined checks, a missing/unknown JSON schema, or any
unsatisfied/unreachable cover fail admission. Named specification assertions
must be reachable and successful. Redundant safety checks in statically
impossible branches may be `Unreachable` (for example indexing an empty set),
but cannot substitute for a specification assertion or cover. Kani sometimes
reports compiled unsupported panic machinery with a successful exclusion check:
the runner admits only `Success` for that check, meaning it was proved
unreachable; it does not ignore unsupported behavior.

As a semantic negative control, the runner copies the actual crate and proof
sources to a private workspace, changes exactly `edits <= MAX_EDITS` to
`edits < MAX_EDITS` in the production kernel, and reruns the unchanged real-bounds
harness. The only failed check must be `exact production plan bounds`. A build
error, arbitrary panic, unwind failure or wrong assertion never counts as a
killed mutant. The positive run checks the actual workspace; the mutant never
modifies it. Source and tool hashes are checked again before receipt admission.

`receipt.json` binds all core sources, manifests/lockfile, reviewed CLI/adapter
callers (including usage ingestion and report/adaptive event consumers),
runner/tests/docs, versioned tool hashes, commands and complete Kani
reports. It is valid only for those exact inputs. It is not a Rust refinement
proof for the TLA+ model, an unbounded theorem for arbitrary sequences, a proof
of sorting/hash collections, or a substitute for final integration gates.

Primary tool references: [Kani loop unwinding](https://model-checking.github.io/kani/tutorial-loop-unwinding.html),
[proof and unwind attributes](https://model-checking.github.io/kani/reference/attributes.html),
and [verification and cover results](https://model-checking.github.io/kani/verification-results.html).

# Transcript algebra with production correspondence

This dependency-free Lean **4.34.0** package proves structural laws over lists of
arbitrary finite length. Its executable oracle also generates a small, frozen
fixture family consumed by **actual Rust admission and provider transforms**.
Those are distinct evidence layers: a theorem about the algebra is not a proof
of the JSON parsers, Rust compiler, provider resume protocol or entire system.

## Theorems and assumptions

`Transcript.lean` defines records with stable natural-number identities, live /
eligible / protected flags, symbolic content and optional tool-call/result IDs.
Masking replaces only an admitted payload symbol with zero. Live-branch
selection, supported payload shape, policy retention and byte-size eligibility
are **inputs** to the algebra. They are established by other code and evidence;
the Lean package does not infer them from a provider transcript.

Its 27 checked theorems establish:

- Empty masking and explicit rejection preserve the transcript. A chosen,
  eligible, unprotected live record gets the mask; unselected, inactive and
  protected records retain their content. Every admitted selector names a known
  live, eligible, unprotected record.
- Original record identities, order and length are preserved. Masking commutes
  with effective-context projection and preserves the protected projection.
- Masking is idempotent and composes as selection union **with fixed flags and
  identity interpretation**. This is a structural law, not a guarantee that a
  second public request is admitted after fresh byte-size/policy projection.
- The complete tool-link sequence is unchanged. The defined well-formedness
  predicate requires a unique earlier call for each result and forbids repeated
  result IDs; pending calls are allowed. Masking preserves that predicate.
- A quiet digest appended with a fresh identity retains the complete old record
  prefix, preserves tool-link validity, and retains identity uniqueness provided
  the original identities were unique and the new identity was absent.

Natural numbers and list lengths are unbounded in these theorems. They do not
assert full recoverability, semantic equivalence of lossy content, digest truth,
correct token accounting, provider acceptance or concurrency/durability. The
separate [Kani kernel gate](../core/README.md) covers numeric admission; the
[dialect corpus](../../crates/gobstopper-adapters/tests/fixtures/dialects-v1/README.md)
and regression suites cover parsing and adversarial graph shapes.

## Executable correspondence

`Vectors.lean` evaluates the proved operations and emits `vectors.json` without
consulting Rust. The checker regenerates it in a fresh private build and requires
byte-for-byte equality with the reviewed fixture. The Rust integration test
`crates/gobstopper-adapters/tests/lean_correspondence.rs` independently constructs
real synthetic Codex and Claude JSONL, resolves physical positions,
projects with the real adapters, invokes `validation::validate_edits`, and invokes
the real pure `transform` only when admitted. Wire observations are decoded
separately from the production content projection to reduce common-mode errors.

Twenty cases execute 23 steps per provider: 12 accepted, 11 rejected and three
digests. They include empty selection, each eligible output, union / reversed
selection order, protected/user/tool-call targets, unknown / duplicate / mixed
selectors, digest insertion, sequential composition and repeated requests.
After a short mask is emitted, the oracle makes it unavailable for another
size-qualified edit; Rust must reject that repeat and preserve the result.
The newest retained output stays eligible throughout, so the one-output policy
suffix does not move in this fixture family.

The test compares payload observations, stable semantic identities/order,
every original semantic record envelope and tool endpoints, effective projection,
and provider verifier findings. Claude new digest identity and parent
binding are checked. Codex appends a semantic item without a provider UUID; its
fresh algebra identity represents the new position, **not** an invented guarantee
that the wire format allocated a globally unique provider identity.

This finite correspondence family covers small **linear, valid synthetic
histories** with supported string payloads above 256 bytes, bounded stubs,
one retained output and a valid bounded digest. It does not cover arbitrary JSON,
branch selection, duplicate provider identities, malformed tool graphs, Unicode
codec behavior or actual live resume. Those obligations remain in the dialect /
property / qualification gates. The protected/inactive algebra laws hold for all
flags; this fixture family is not evidence for every production flag derivation.

## Gate and negative controls

```sh
python3 verify/transcript/test_check.py
# Prepare dependencies, including the dev dependency's nested engine build.
cargo test -p gobstopper-adapters --test lean_correspondence --locked --no-run --jobs 2
python3 verify/transcript/check.py --lake /absolute/path/to/lean-4.34.0/bin/lake --output "$NEW_EVIDENCE_DIR"
cargo test -p gobstopper-adapters --locked --test lean_correspondence
```

The selected `lake` must have sibling `lean` and `leanchecker` executables from
Lean 4.34.0, commit `293d5d0c0c3f3dded4688b3ccd6a33939ac5102b`. Official archive
pins live in [`../tools.lock.json`](../tools.lock.json). There are no external Lean
packages. The preparation command may access the dependency registry. A plain
`cargo fetch --locked` is insufficient on a clean cache: the Hegel dev dependency's
build script builds an engine through a separate generated Cargo workspace.
The checker then builds in a fresh target directory with Cargo offline, using
that populated dependency cache. No installation or global configuration changes
are made by this runner.

The positive gate runs a fresh `lake build`, the exact theorem axiom inventory,
and `lake env leanchecker --fresh Transcript`, which replays declarations and
imports into a fresh kernel environment. Every theorem is checked for axioms;
the current permitted set is only standard `propext` and `Quot.sound`. There are
no admitted proofs, custom axioms, native proof shortcuts or disabled kernel
checks. Source tripwires reject common escapes and unexpected imports/modules;
these checks supplement source review and kernel replay rather than proving an
arbitrary Lean metaprogram safe. The selected Lean compiler/kernel, bundled core
library, runtime, Rust compiler, standard library, OS and Python remain trusted.

Two independent semantic mutations must fail the same production correspondence
fixture for the **specified payload counterexample**:

1. A private copy of the Lean oracle serializes a masked symbol as one instead of
   zero. Its unchanged structural proofs still build. Real Rust emits zero;
   correspondence must reject the faulty serialization (`left: 0`, `right: 1`).
2. A private copy of actual `codex::apply_inner` passes an empty target set into
   its existing lowering function. Real output retains symbol three while the
   unchanged Lean oracle expects zero (`left: 3`, `right: 0`).

A compiler error, timeout, skipped/filtered test, arbitrary panic or wrong
assertion does not count as a killed mutant. Production files are never mutated
by these controls. The Rust mutant keeps the original complete workspace
manifests and lockfile; its CLI sibling has only an uncompiled metadata target.
Only the real core/adapters and correspondence test are compiled by the exact
package/test selector. The bounded Unix reactor from `../watch/check.py` owns all
children with process-group cleanup before reaping; each Lean step is limited to
120 seconds, each Cargo step to 600 seconds, and each log to 8 MiB.

The receipt hashes proofs, oracle, checked-in vectors, tests, runners, all core /
adapter Rust sources, manifests/lockfile and observed tool binaries. Inputs must
remain unchanged through completion. The receipt is evidence for exactly that
source/tool set and these stated scopes; final integration remains a separate
required gate. The Lean executable compiler-to-runtime connection is tested by
the oracle mutation and production comparison, not proved by the kernel.

Rustup dispatchers are recorded separately from the actual Cargo and compiler
selected by a bounded `rustc --print sysroot` query. All Rust cases invoke that
Cargo by absolute path and set `RUSTC` to the corresponding hashed compiler.
Version and sysroot logs have recorded digests. Compiler libraries, the linker,
dependency cache and operating system remain trusted inputs; the receipt does
not claim a hermetic Rust build. The repository lockfile does not govern the
transitive resolution of Hegel's separate engine workspace, although Hegel pins
the engine crate version. Locking and attesting that nested build is a remaining
dependency-provenance obligation.

Primary references: [Lean proof validation and axiom auditing](https://lean-lang.org/doc/reference/latest/ValidatingProofs/),
[Lean 4.34.0 release](https://github.com/leanprover/lean4/releases/tag/v4.34.0), and
[the bundled kernel-replay implementation](https://github.com/leanprover/lean4/blob/v4.34.0/src/LeanChecker.lean).

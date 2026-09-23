# Assurance case

This inventory records the current correctness foundation, its assumptions and
bounded evidence. It does not certify whole-system correctness. The immutable
baseline is `ffc71480564f0d0077f27e59a04df3174d5335ef`
([PR 88](https://github.com/hraness/gobstopper/pull/88)); foundation delivery is
tracked in [PR 90](https://github.com/hraness/gobstopper/pull/90).

- [ledger.json](ledger.json): state ownership, assumptions, invariant obligations,
  audit coverage, evidence, schema compatibility and activation gates.
- [effects.json](effects.json): CLI, MCP, hooks, production Python and conservative
  Rust public-callable effects, including trusted external execution.
- [claims.json](claims.json): the scope and limits of public assertions.
- [codeql-triage.json](codeql-triage.json): all 21 baseline alerts with individual
  authorization/source/sink review. These records do not dismiss or suppress a
  scanner finding. Source repair and a fresh scan are different evidence.
- [qualification.json](qualification.json): provider/platform/mode matrix. No live
  native cell is qualified; released CLI native dispatch is guarded, including
  configurations that previously opted in to automatic closed-session compaction.
- [operations.md](operations.md): copies, settings bundles, durable uncertainty,
  reconciliation, recovery and downgrade limits.

## What each layer establishes

Production Rust [Kani harnesses](../../verify/core/README.md) check selected scalar
admission, arithmetic and usage kernels over their declared numeric domains, plus
explicitly bounded index/edit sequences. [Lean laws](../../verify/transcript/README.md)
prove structural list properties for arbitrary finite lists; independent emitted
vectors test finite correspondence with the actual three Rust dialect adapters.
Neither is a theorem about all Rust parsing or all future task answers.

The [vault/publication](../../verify/vault/README.md) and
[native dispatch](../../verify/watch/README.md) TLA+ models exhaust specified finite
state graphs. Required semantic mutants and reachability witnesses detect disabled
behavior and broken safety conditions. A parser/compiler failure, timeout or wrong
assertion does not count as a successful negative control. The models assume atomic
durable abstract actions, stable cooperating custody and no fairness; correspondence
tables are reviewed arguments, not machine-checked whole-program refinements.

Storage fault injection, killed-process fixtures, protocol scripts and longer
command sequences test implementation behavior separately. Synthetic data does not
qualify a proprietary provider/version/account, physical power-loss recovery or
network filesystem semantics. Registered retention controls distinguish literal
presence, judge estimates, task-specific labels, missing data and incomplete arms.
Recorded occupancy reduction is not billed savings.

## Structural admission

Run `python3 scripts/check_assurance.py` and
`python3 -m unittest discover -s scripts -p test_assurance.py -v`.
The checker has no network/provider/build/mutation effects. Its negative fixtures
reject missing surfaces, unsupported assertions, unknown effects and damaged evidence.
It requires explicit owners, assumptions, bounds, exclusions and next gates, and
compares CLI/MCP/hooks, production Python files and public Rust declarations with
source. It checks current receipt source hashes and reviewed case inventories, and checks
the TLC jar against its repository pin. Native tool fingerprints are recorded
receipt evidence; raw logs and artifacts are reviewed separately, not authenticated
by this structural checker.
Historical baseline evidence remains explicitly historical.

The callable scan includes crate-visible declarations and test-only helpers
conservatively. It is not a Rust effect analyzer: unchanged symbols can acquire new
effects, and private implementations, trait methods and external code require review
of enclosing profiles. Empty write lists state direct effects, not sandboxing.
A summarized receipt preserves its raw receipt SHA and exact inputs; complete logs
remain local or in the associated CI artifact. Receipts are evidence, never permission
to skip a required final integration, release or installation gate.

## Trust and compatibility

The Rust/Lean/Kani/TLC toolchains, dependencies, kernel, filesystem, hashes, trusted
executables and provider implementations remain in the trusted boundary. Parent
directories and executable paths must remain stable and owner-controlled. Directory
`flock` coordinates cooperating vault readers/writers, including the Python retention
reader; it does not lock provider memory or hostile processes. Plugins and explicitly
admitted model/scorer commands run with user authority, without an OS sandbox.
Apple bridge identity cannot attest opaque model weights.

Disclosure is field- and operation-specific. Requested IDs, digests, exports and
transcript records may be sensitive. Background output and credential diagnostics
use narrower closed categories. MCP transcript-content permission does not authorize
external effects; MCP has no external-execution opt-in. Recovery objects and settings bundles can contain private
content and require the same care as their sources.

Compatibility is explicit: copy receipts write version 2 and conservatively inspect
legacy records; watch state writes generation 9; native journals use version 1.
Unknown or damaged state refuses rather than becoming empty success. Pins do not
expire automatically. Older binaries that ignore the journal cannot safely restart
watchers against unresolved new state. Preserve the old artifact, configuration and
state; never delete evidence to force a downgrade or retry.

Artifact admission permits a checked guarded installation. It does not activate an
unqualified native/provider-store path. Any later activation requires exact provider,
mode, version, custody/correlation, bounded outcome and continuation evidence plus a
reviewed guard change. The [phase plan](../correctness-plan.md) records implementation,
review, validation and delivery status separately.

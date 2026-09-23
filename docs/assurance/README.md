# Assurance inventory

This is the C1 contract inventory for the correctness foundation. It records
obligations and bounded evidence; it does not certify whole-system correctness.
Its source-review baseline is `ffc71480564f0d0077f27e59a04df3174d5335ef`
([PR 88](https://github.com/hraness/gobstopper/pull/88)). C1 privacy corrections
are identified separately from that immutable baseline.

- [ledger.json](ledger.json): state ownership, trusted assumptions, invariants,
  all audit coverage rows, evidence, schema compatibility and activation gates.
- [effects.json](effects.json): all CLI variants, advertised MCP tools, hook
  callbacks, production scripts, and a conservative public-callable index.
- [claims.json](claims.json): bounded replacements for public safety,
  verification, read-only and savings language.
- [codeql-triage.json](codeql-triage.json): individual source/sink/authorization
  review of all 21 open alerts at the baseline. No alert is dismissed or rule
  suppressed by these files. A source repair is not a fresh scanner result.

Run `python3 scripts/check_assurance.py` and
`python3 -m unittest discover -s scripts -p test_assurance.py -v` from the
repository root. The first command has no network, provider, build or mutation
effects. Tests use in-memory mutated ledgers and temporary synthetic sources.
The unsupported-claim fixture must fail admission.

The structural check requires coverage and referential integrity, explicit
owners, assumptions, bounds, exclusions and next gates. It compares CLI variants,
MCP tools, hooks, scripts and public callable declarations with current source.
The callable scan is deliberately conservative: it includes crate-visible
helpers and constructors, and is not a Rust effect analyzer. An unchanged symbol
can acquire a new effect without this check noticing; code review must update
its classification. Private implementations, trait methods and external code
are accounted for by their enclosing effect profiles, not mechanically proved.
New profiles must name every written state and delegated authority. Empty write
lists mean no direct durable writes in that profile, not a sandbox guarantee.

The state model distinguishes original provider bytes, canonical exports,
effective context, provider memory, immutable archive objects, mutable indexes,
recovery receipts, telemetry, watcher decisions, model caches, configuration,
credentials, private experiment outputs and observable output. A canonical export
is not the live database; a manifest hash is not its reconstructed source hash;
a provider acknowledgement is not terminal evidence; a token estimate is not a
billing receipt. Recovery data may itself contain private transcript content.

Trust currently includes the Rust toolchain/dependencies, OS and filesystem,
cryptographic primitives, provider implementations, configured executable paths,
trusted plugins and scorer commands, and the user-selected account/home. Unix
directory `flock` protects cooperating vault participants; it does not lock a
provider session or a Python reader using a different protocol. Atomic rename
does not prevent a concurrent provider append. Native control still needs a
qualified ownership/correlation contract. The finite TLA+ vault result assumes
atomic durable abstract actions. It includes process termination releasing
custody, but excludes production refinement, filesystem crash ordering,
power-loss recovery, unbounded fairness, and semantic preservation.

Inspection may reveal exactly requested session IDs, digests or transcript
content. Background diagnostics and authentication output have a narrower
required contract: no raw session identity, provider response, or credential
fragments. C1 repairs the seven named baseline sinks, not every diagnostic;
session-prefix, path, working-directory and raw-error surfaces remain C8/C10
privacy work and must not be described as globally sanitized.
Requested output can still be captured by a caller; authorization is field- and
operation-specific, not evidence that an identifier is universally nonsensitive.
MCP content-tool opt-in does not sandbox trusted planning extensions.

Compatibility rows distinguish a documented contract from actual reader
behavior. There is no blanket upgrade/downgrade promise: unknown receipt and
manifest versions may be refused, watch state currently tolerates future
generations, and stable CLI JSON is additive by repository policy rather than a
version-negotiated schema. Preserve original state and rollback artifacts; never
delete a receipt or regenerate provider data merely to make an older reader work.

Artifact admission requires the current integration gates and independent
review. Live provider qualification records exact binary/version, home/account,
fixture population, custody, outcomes and recovery separately. Publishing guarded
source is not permission to install a binary or restart a watcher into an
unqualified mutation mode. Later phases update this inventory with evidence for
their exact revision and environment; historical receipts are not reusable gates.

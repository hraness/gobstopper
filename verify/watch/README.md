# Native dispatch protocol model

This directory checks a **finite abstract safety model**, not production Rust,
provider semantics, or filesystem durability. It models two cooperating watchers
sharing custody of one canonical provider/home/store/session target, with two
operation slots and one change of suppression generation. A generation stands
for any change in source, policy, configuration or provider/strategy version; it
is not evidence that the runtime hashes every relevant input correctly.

`Watch.tla` separates a durable `dispatched` append from the actual provider
call. Crashing between them leaves the same unresolved durable state as a lost
outcome. Neither cooldown expiry nor a generation change clears `dispatched` or
`unknown`. A never-dispatched `prepared` operation may be superseded by a fresh
operation, preserving the old record and recovery root. Each operation has at
most four durable records:

```text
prepared -> rejected
prepared -> dispatched -> observed_applied | observed_noop | unknown
unknown with retained exact terminal evidence -> reconciled [explicit request]
```

Applied and no-op observations require a matching terminal **and** an abstract
exact-target post-state observation of reduction or no change. A terminal with
unavailable post-state records `unknown` while retaining terminal evidence.
Explicit reconciliation may release that uncertainty only with already-retained
exact provider evidence. It makes no provider call, accepts no caller-supplied
outcome, and does not claim measured savings. Session-only or connection-based
correlation cannot admit reconciliation. A `dispatched` record without terminal
evidence remains blocked.

## Checked properties and non-vacuity

Both `safe.cfg` (exact provider identity contract) and
`safe-assumed-correlation.cfg` (explicit private-connection correlation
assumption) exhaust their finite reachable state graphs. Their invariants check
custody exclusivity, valid immutable journal bindings and transitions, recovery
roots before intent/dispatch, dispatch-before-call, at most one call per
operation, no call past an earlier unresolved operation, no concurrent provider
work, terminal-backed outcome classification, exact evidence for reconciliation,
and absence of automatic surgery fallback.

The model includes crash/restart at each volatile phase, abandoned pre-dispatch
writes, provider completion after watcher death, lost acknowledgements or
terminals, terminal-before-ack delivery, duplicate messages, and unrelated
session/operation/turn messages. Foreign message identities use one representative
of each mismatch class. Correct correlation is an explicit adapter contract;
synthetic operation/turn tokens do not invent identifiers a provider lacks.
In particular, Codex assumes the first `contextCompaction` item on the private
stream belongs to the dispatched request. Matching its session, turn and item
IDs does not establish a provider-attested binding from Gobstopper's local
operation ID to a provider operation. That association remains an explicit
trusted protocol assumption, including in the exact-ID configuration.

Five semantic mutants must fail their specified invariant:

| Mutation | Required counterexample |
| --- | --- |
| Permit a new operation after unresolved work's cooldown expires | `NoUnresolvedReplay` |
| Treat an acknowledgement as applied | `AppliedNeedsTerminal` |
| Omit the recovery root | `RecoveryPinned` |
| Fall back automatically after uncertainty | `NoFallback` |
| Reconcile without retained exact terminal evidence | `ReconcileNeedsExactEvidence` |

Six separate negated reachability assertions must produce witnesses for applied,
no-op, unknown, explicit reconciliation, a crash leaving durable dispatched, and
terminal-before-ack. Thus a model that disables all sends cannot pass the suite.
These are existence witnesses, not eventual-completion guarantees. A parse
error, local-listener denial, timeout, truncated log, wrong invariant or generic
nonzero exit is **not** an accepted counterexample.

## Production correspondence and remaining obligations

| Model component | Runtime boundary to review/test |
| --- | --- |
| `Acquire` / `Relinquish` / `Crash` | `native_operations.rs`: stable per-target advisory lock held through the attempt; process death releases it |
| Target and immutable event binding | `Target`, `Record`, `read_records`: canonical provider/home/source/session, unique operation digest, binary/source/snapshot/policy digests and protocol contract |
| `RetainPin`, `PersistPrepared` | `Operation::prepare` calls `vault::retain_operation_snapshot` before durable intent publication |
| `PersistDispatched`, then `Send` | `Operation::dispatch` must succeed before a native adapter is called; failed intent/dispatch persistence admits no call |
| `Terminal`, `Ack` | Protocol-specific bounded adapters in `main.rs`, `claude.rs`, `devin.rs`; actual session/turn/item checks or explicitly documented weaker correlation assumptions |
| `ObserveTerminal`, `UnobservedPostState`, `Uncertain` | Exact-target post-state observation and `Operation::finish`; unknown cannot be labelled applied or credited as measured savings |
| `ExplicitReconcile` | `can_reconcile`, `reconcile`, `read_records`: only retained matching Codex session/turn/item evidence can append `reconciled` |
| `FreshAllowed`, `NoUnresolvedReplay` | Both pending-work admission and journal validation must block unresolved work independently of watch cooldown/generation |
| Disabled `Fallback` | Native error/no-op/uncertainty must never route to a direct provider-file/database mutator |

This table is a reviewed correspondence argument, **not a refinement theorem**.
The checked state space makes the following assumptions and exclusions explicit:

- Custody coordinates only cooperating processes using the same owner-controlled
  data root and stable lock inode. It does not lock the provider, a second data
  root, or a hostile same-user process. Target canonicalization, symlink defenses,
  path ownership and advisory-lock semantics are trusted/runtime-tested boundaries.
- A successful journal append is atomic and durable in the model. Process
  termination occurs between abstract steps. Torn writes, failed `fsync` that
  nonetheless persisted bytes, power loss and filesystem reordering are outside
  this model. The Rust reader must fail closed on damaged/exhausted journals;
  fault-injection tests and the separate vault publication model cover different
  parts of that argument. Production tests inject failures before/after write,
  after a partial write and before/after sync across all seven journal states;
  failed dispatch persistence must admit no provider call. These returned-error
  fixtures do not simulate physical power loss or every registry syscall. Their
  composition with this model is not mechanically proved here.
- A pin denotes valid, retained recovery data. Snapshot content/identity,
  encryption/manifest correctness, the pre-pin publication window and GC are
  separate storage obligations. Pins are never removed in this model.
- The generation is an abstract binding. Source fingerprint freshness, the
  hash-to-exec race for an owner-controlled native executable, dynamic loaders,
  interpreters, environment inheritance and exact executed provider version are
  not proved by recording a binary digest.
- Provider completion and post-state measurement are nondeterministic oracles.
  The model does not prove token arithmetic, delayed on-disk visibility, correct
  session projection, remote account/home selection, provider idempotence,
  subprocess cleanup, cancellation, or correctness of native compaction.
  Provider protocol fixtures, live qualification and other plan phases remain
  necessary. Exact-correlation and assumption-only graphs distinguish the
  reconciliation boundary; neither establishes a provider contract externally.
- No weak or strong fairness is assumed. A watcher can starve, a provider can
  never return, and unresolved work can remain blocked forever. Deadlock checking
  is disabled because quiescence and bounded-history exhaustion are allowed;
  safety, not eventual service or recovery, is checked. Crash/restart, duplicate
  events and clock expiry can cycle without increasing the finite state space.
- Two watchers, two operations and four records per operation are the explored
  bounds. There is no cutoff proof for arbitrary counts, no unbounded TLA+/TLAPS
  theorem, and no Lean proof in this directory. TLC's fingerprinting, interpreter,
  Java, Python, OS and hardware remain part of the verification toolchain trust.

## Running the check

Use the official [TLA+ tools v1.7.4 release](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4)
`tla2tools.jar` with a recorded Java runtime (11 or newer). The runner rejects a
jar whose SHA-256 differs from
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`.
That is a locally computed pin of the official artifact, not a signed publisher
attestation. The release's SHA-1 is
`bee4a54f3ee3d4afc347c3240ec2d9e93b075104`; the jar reports TLC 2.19.

```sh
python3 verify/watch/test_check.py
python3 verify/watch/check.py --java "$JAVA_BIN" --tlc-jar "$TLC_JAR" --output "$NEW_EVIDENCE_DIR"
```

The output directory must not already exist. It contains source/config copies,
complete logs, commands, state counts and a JSON receipt binding model, checker,
tests, documentation, Java executable and jar digests. A receipt passes only if
all 13 cases pass and those inputs remain unchanged. It applies to those exact
inputs; it is not a required integration-gate exemption after later edits.

Each TLC case uses one worker, fixed seed/fingerprint selection, a 256 MiB JVM
heap, 45-second deadline and 32 MiB combined log bound. JVM option injection is
removed; per-case home/temp directories opt out of TLC statistics. TLC still
requires permission to create its local RMI listener. A denied run is recorded
as failed evidence; obtain the applicable execution permission rather than
disabling the gate.

The Unix runner uses nonblocking output with no pipe-reader threads. It observes
leader exit with `waitid(WNOWAIT)`, signals only its newly created process group
before reaping, and then closes/drains bounded output. On Darwin, `killpg1` can
report `EPERM` for an unreaped zombie-only group: the runner admits this edge only
after observing leader exit and pipe EOF. A live-leader/inherited-pipe denial
still fails. This assumes the pinned Java does not create privilege-changing or
detached children; the runner is not an arbitrary-code sandbox. See
[Apple's implementation](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_sig.c#L1579-L1624)
and [Python's `WNOWAIT` contract](https://docs.python.org/3/library/os.html#os.WNOWAIT).

For the distinction between finite model checking and deductive proof, see the
primary [TLA+ specification text](https://lamport.azurewebsites.net/tla/book.html).

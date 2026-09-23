# Vault custody and publication models

This pilot checks the concurrency design of snapshot publication, operation
recovery pins, readers, and garbage collection. It does **not** prove the Rust
implementation, provider resumability, or filesystem durability.

The model contains two publishers, one reader, one prune cycle, three snapshot
identities, and four chunk identities. Both publishers share a chunk with the
initial snapshot. A publisher may crash at every intermediate step; prune and
the reader may also crash and release their custody. Prune can choose any
retained subset, including an empty set. This deliberately abstracts away the
recency policy and checks storage safety for all those choices.

## Checked properties

| Property | Meaning |
| --- | --- |
| `TypeOK` | Every state remains inside the declared finite domains. |
| `IndexedData` | Every indexed snapshot has its manifest and all its chunks. |
| `PinnedRecoveryData` | Every operation receipt's snapshot remains reconstructible, including after a publisher crash. |
| `ReaderData` | A reader holding shared custody cannot lose the snapshot it is reconstructing. |
| `ExclusiveCustody` | Prune never holds exclusive custody alongside a publisher or reader. |

`safe.cfg` checks every reachable state with custody and receipt roots enabled.
`no-custody.cfg` must produce an `IndexedData` counterexample: a publisher writes
chunks, prune marks them before their manifest exists, the publisher verifies
and indexes its snapshot, and prune deletes its chunks. Even the model's
atomic index comparison does not prevent this race.

`no-pins.cfg` keeps custody but must violate `PinnedRecoveryData`: excluding a
receipt's snapshot from retention makes its recovery data collectible after
the owner releases its lock. `witness-completion.cfg` must reach a state where
both publishers, the reader, and prune have completed. `witness-pending.cfg`
must reach a crashed publisher whose receipt still pins its snapshot. These
expected counterexamples check that the model admits useful executions and
unfinished operations; they are not passing safety results for the mutants.

## Run

Use Java 11 or newer and the official immutable
[TLC v1.7.4 artifact](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4):

```sh
curl -fL https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar -o /tmp/gobstopper-tla2tools-v1.7.4.jar
python3 verify/vault/check.py --java java --tlc-jar /tmp/gobstopper-tla2tools-v1.7.4.jar --output /tmp/gobstopper-vault-evidence
```

The output directory must be new. The runner checks the jar's SHA-256 before
execution: `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`.
This digest was computed from the release artifact whose SHA-1 matches the
publisher's release page (`bee4a54f3ee3d4afc347c3240ec2d9e93b075104`); it is
not a publisher signature. The runner uses one TLC worker, a 256 MiB JVM heap,
a fixed seed/fingerprint, a 45-second limit per case and a 32 MiB log limit.
It reuses the watch runner's nonblocking owned-process reactor and includes
that runner's hash in the receipt. An incomplete log or wrong invariant fails
admission, even if an intended failure also appears. Checked configurations must
match explicit invariant sets and mutation flags. It saves each complete
log and a receipt binding the model/configuration/runner, jar, Java executable,
commands, exit status, and state counts. No telemetry preference or Java option
from the host is imported. TLC may require permission for a local ephemeral
Java listener; a listener denial or timeout is a failed check, never evidence.

The 2026-09-23 local run used Temurin 21.0.12.1+1 on macOS arm64, with Java
executable SHA-256
`9be1d0a740ff6502df1a762145e62860f5de4b7e17658d9cb9498da3acf9d16c`.
The official Temurin distribution archive SHA-256 was
`3623232f33a9c3baadf304480b2535f9a3cba8a58d42ecbb438ba267315d9998`.
TLC reports engine version 2.19 even though the artifact release is v1.7.4.
The safe configuration generated 53,608 states, found 25,810 distinct states,
and finished with zero states on the queue. All four expected counterexample
controls passed. These numbers are observations of this model version, not a
threshold to preserve by weakening the model.

## Production correspondence

The correspondence is reviewed, not mechanically proved. Both source paths
below are relative to the repository root.

| Model action/state | Production boundary |
| --- | --- |
| `Start`, `Holding`, `CanShare`, `CanPrune` | `vault.rs::Custody` locks the stable vault directory inode on Unix. Snapshot publication and copy operations acquire shared custody; `prune` takes exclusive custody. |
| `PublishChunks`, `PublishManifest`, `VerifySnapshot`, `AppendIndex` | `vault.rs::snapshot_data` publishes immutable content-addressed objects, checks reconstruction, then durably appends the index. |
| `PublishReceipt`, `pins` | `copy.rs::{compact, compact_via_compacted, publish_prepared}` retains shared custody across snapshot/receipt publication. `prune` treats prepared and complete operation references, plus explicit native recovery pins, as roots. Direct Devin store mutation is disabled. |
| `Mark` | `vault.rs::prune` selects retained entries, resolves operation roots, validates retained manifests, and computes unreachable objects while holding exclusive custody. |
| `ReplaceIndex`, `DeleteManifests`, `DeleteChunks` | `prune` replaces the index before unlinking its selected unreachable objects. |
| `StartRead`, `FinishRead` | `vault.rs::Reader` holds shared custody across selection and complete reconstruction. `read_object`, `diff` and `read_record` also take shared custody. The independent Python reader uses the same directory flock; the internal locked helper avoids recursive exclusive acquisition inside prune. |
| Crash actions | Process termination releases its locks. Already modeled publications remain durable; future steps do not happen. |

Keep this table current when those functions change. A stable custody inode is
essential: a lock on an index inode does not follow a later rename. Opening the
existing directory for reader/prune custody avoids creating files or changing
permissions on a read or dry run. Production currently refuses this custody
operation on non-Unix platforms. The directory itself must not be replaced
while operations are active. Every
writer and reader of the modeled objects must participate in the custody
protocol. A foreign process that ignores the lock is outside this contract.

## Explicit limits and follow-up

- This finite model has one operation per publisher, one read, and one prune.
  It does not establish an unbounded theorem, retries after crash, repeated
  prune cycles, starvation freedom, or eventual completion. Deadlock checking
  is disabled because ordinary terminal states have no next action; no
  liveness or deadlock-freedom claim follows from this run.
- Publication and index replacement are abstract atomic durable actions;
  `ReplaceIndex` even models a true conditional swap. The Rust read/recheck/
  rename sequence is not such a primitive. Shared custody is what excludes
  cooperative writers during pruning. The model does not certify filesystem
  crash ordering, directory fsync, torn writes, disk-full behavior, read-only
  media, network filesystems, Windows locking, SQLite, or open-file handling.
- Hashes are distinct symbolic names, chunks are valid by construction, and
  byte-level parsers/serializers are absent. Corrupt/unknown retained manifests
  must fail closed in production. Digest collision resistance, actual
  reconstruction, size bounds, and legacy formats need separate evidence.
- The model abstracts retention to a set. Duplicate index entries, stream
  identity, timestamp ties, ordering, index truncation, and metadata preservation
  are production-test obligations. A pinned snapshot need not remain indexed
  in the model, but its objects must remain present.
- Receipt creation is atomic while shared custody is held. Receipt parsing,
  schema versions, missing/corrupt pins, operation identity, output publication,
  uncertain completion, and receipt retirement are not modeled. Root retirement
  needs its own recovery/retention protocol before old receipts can be collected.

C3 adds deterministic storage fault tests and killed fixture processes at these
boundaries. The publication model below covers a bounded restart and two prune
cycles. Unbounded command-sequence refinement and provider ownership remain
separate obligations; a checked abstract transition is not a Rust proof.
An inductive TLA+/TLAPS, Lean, or Verus proof can later remove finite actor/cycle
bounds; it would still need the implementation correspondence obligations.

TLC checks the reachable graph of the configured finite model; successful
model checking does not automatically establish implementation correctness.
See the [official TLC explanation](https://learning.tlapl.us/intro/platform/)
and [TLC command-line documentation](https://docs.tlapl.us/using%3Atlc%3Astart).


## C3 publication and bounded recovery

`Publication.tla` adds one publisher, one reader, at most two prune cycles and
one publisher restart. It includes source chunks/manifest/index, prepared
candidate chunks/manifest, durable intent, visible output, directory-sync
confirmation, completion receipt, torn index/receipt states, conflicting
outputs, failed manifest unlink, and process interruption. A prepared intent
pins **both** the source and exact candidate. Recovery never runs a transform.
These are bounded additions; they do not remove the original model's limits.

| Property/control | Obligation |
| --- | --- |
| `IndexedData`, `PinnedData`, `ReaderData` | Indexed, pinned and actively read data remains reconstructible. |
| `AllManifestsValid` | A surviving manifest never loses its chunks, including after failed deletion. |
| `ExclusiveCustody` | Collection excludes publishers and active readers. |
| `OutputHasIntent` | Published candidate bytes have an already durable prepared/completed record. |
| `CompletedDurable` | Completion follows output visibility and sync confirmation. |
| `DamageBlocksCollection` | Unknown index/receipt state prevents collection. |
| `publication-recovery` | A witness reaches completion after interruption and restart. |

The `publication-no-intent`, `publication-no-sync`,
`publication-ignore-damage` and `publication-ignore-unlink` configurations must
violate their named obligation. The runner checks the exact invariant and trace,
not merely a nonzero process status. `publication-safe` must exhaust its queue.
Both safe models and all nine counterexample/witness controls passed on
2026-09-23 with the same pinned Java/JAR documented above. The publication model
generated 59,790 states, found 28,082 distinct states, and exhausted its queue.
The initial sandbox run could not open Java's required ephemeral local listener
and failed admission; a separately admitted run produced these results.

| Model boundary | Production and regression correspondence |
| --- | --- |
| Source/candidate publication | `vault::store_object_locked` publishes, hashes and reconstructs each object under root custody. `snapshot_data` strictly admits the index before append. |
| `Prepare` | `copy::publish_prepared` writes a schema-2 receipt binding source identity, effective inputs and both exact object references before output publication. Pins have no automatic expiry. |
| `Publish`, `SyncOutput`, `Complete` | `transaction::publish_new`, `confirm_publication`, and receipt `replace`. Typed failures distinguish visibility from confirmed sync and retain the underlying I/O error. |
| `Restart`, `ReconcileVisible`, `RefuseConflict` | `copy::recover_operation` validates the stored identity and pins, reconciles exact output, or refuses conflicting/unsupported state. Source changes do not cause a transform replay. |
| Damage refusal | Strict index and operation decoders reject torn/unknown rows; unclassified temporary files and malformed roots are preserved for explicit repair. Tolerant listing cannot authorize collection. |
| `FailedUnlink`, `RemoveChunks` | Prune propagates manifest-removal failure, syncs the manifest directory before removing chunks, and reports incomplete cleanup instead of successful planned counts. |
| Crash/reader actions | `storage_tests` terminates isolated child processes with SIGKILL at declared checkpoints, checks custody release and fresh recovery; a separate reader process excludes exclusive collection until collected. |

Returned-error tests inject failures before and after publication and deletion
steps, including a partial index append. Tests also cover nonmonotonic clocks,
exact store/session retention, legacy receipts/objects, corrupt roots,
no-follow/nonblocking leaf reads, and Python/Rust reconstruction parity.
`tests/recovery.rs` and `scripts/test_retention_reader.py` cover the independent
reader. All inputs are synthetic; provider stores and live user vaults are not
part of these checks.

The correspondence remains reviewed, not mechanically established. Durable
abstract object/receipt writes assume the underlying successful file/directory
sync contract; the model does not simulate loss or reordering of unsynced disk
writes after power failure. Returned errors and process death cannot establish
hardware/filesystem guarantees. Parent directories and the root inode must be
stable and owner-controlled; external writers must honor custody. Garbage
left by an interrupted operation is preserved, and unclassified artifacts can
require manual repair. There is no automatic pin expiry or general repair tool.

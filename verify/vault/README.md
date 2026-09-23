# Vault custody model

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
a fixed seed/fingerprint, and a 45-second limit per case. It saves each full
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
| `PublishReceipt`, `pins` | `copy.rs::{compact, compact_via_compacted, compact_devin_store}` retains shared custody across snapshot/receipt publication. `prune` treats operation receipt snapshot references as roots, whether complete or pending. |
| `Mark` | `vault.rs::prune` selects retained entries, resolves operation roots, validates retained manifests, and computes unreachable objects while holding exclusive custody. |
| `ReplaceIndex`, `DeleteManifests`, `DeleteChunks` | `prune` replaces the index before unlinking its selected unreachable objects. |
| `StartRead`, `FinishRead` | `vault.rs::read_object` holds shared custody around complete reconstruction; the internal locked helper prevents recursive exclusive acquisition inside prune. |
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

The next correspondence step is deterministic Rust fault/interleaving replay
at these same action boundaries, including both negative-control traces.
Follow it with repeated snapshot/prune/read/restart Hegel command sequences
against a simple reference store. Only after those checks exist should this
pilot expand to publication/receipt reconciliation and provider ownership.
An inductive TLA+/TLAPS, Lean, or Verus proof can later remove finite actor/cycle
bounds; it would still need the implementation correspondence obligations.

TLC checks the reachable graph of the configured finite model; successful
model checking does not automatically establish implementation correctness.
See the [official TLC explanation](https://learning.tlapl.us/intro/platform/)
and [TLC command-line documentation](https://docs.tlapl.us/using%3Atlc%3Astart).

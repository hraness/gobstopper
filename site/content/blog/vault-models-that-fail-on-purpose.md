A backup can disappear even when saving and cleanup each work correctly on their own. A writer creates the pieces of a snapshot. Cleanup sees pieces that no completed snapshot uses and marks them for deletion. The writer finishes, reports success, and cleanup deletes the pieces it marked earlier.

Gobstopper’s archive prevents that race by coordinating saves, reads, and cleanup with locks. Its TLA+ models make the ordering explicit, and deliberately broken variants show which loss each protection prevents.

## A snapshot becomes visible only after it can be rebuilt

The archive splits a transcript into chunks named by their content hashes. A snapshot has a list of its chunks, and an index makes completed snapshots discoverable. Saving follows this order:

1. Write the chunks.
2. Write the snapshot’s chunk list.
3. Rebuild the snapshot and check it.
4. Add the snapshot to the index.
5. For a compaction operation, record that recovery depends on this snapshot.

That last record acts as a pin. Cleanup keeps pinned snapshots even when ordinary retention rules would remove them. A crash can end the writer’s process without ending the need for its recovery data.

Saves and reads hold a shared lock on the archive folder. Cleanup needs an exclusive lock. It can therefore choose and delete unused chunks only while no cooperating reader or writer is using them.

## Model the dangerous order

TLA+ describes a system in terms of states and steps. Its TLC checker explores every reachable ordering within a finite configuration. Gobstopper’s archive model includes writers, a reader, cleanup, shared chunks, and crashes between steps.

One invariant, a rule that must remain true, says that every indexed snapshot has all its data. In the model, `Parts(s)` names the chunks a snapshot needs:

```tla
DataValid(s) == s \in manifests /\ Parts(s) \subseteq objects
IndexedData  == \A s \in index : DataValid(s)
```

The checker also follows pinned snapshots, active readers, and lock ownership. Its configurations use a small fixed number of participants and chunks, so the result applies to those modeled sizes and actions. It does not prove the behavior of a filesystem during a power loss.

## Remove one protection and follow the loss

Switch off the lock and the opening race becomes possible:

```text
Writer:  stores new chunks
Cleanup: marks those chunks as unused
Writer:  finishes and indexes the snapshot
Cleanup: deletes the marked chunks
Reader:  finds an indexed snapshot it cannot rebuild
```

Rechecking the index just before cleanup acts is insufficient in this model. Another writer can still pass through the gap. Holding the lock covers the whole decision and deletion sequence.

A second broken variant keeps the lock but ignores pins. A writer saves and pins a snapshot, then crashes. Cleanup runs after the lock is released and removes the snapshot that recovery needs. The lock protects concurrent activity; the pin protects unfinished work after the process is gone.

The checks require each broken variant to fail on its expected invariant. A syntax error or timeout does not count. Separate configurations must reach completed work and interrupted work, so a model that silently disables all operations cannot pass merely because nothing happened.

## Publishing a compacted copy has its own ordering

Saving the original is one part of compaction. Publishing the smaller copy also needs a recovery record. Gobstopper records the operation before the copy appears and marks it complete only after the output and its containing folder have been synced.

After a crash, recovery compares the saved record with the actual output. It confirms a matching result or refuses to guess. A damaged index or operation record stops cleanup, preserving data while the inconsistency is investigated.

The [archive model reference](https://github.com/hraness/gobstopper/blob/main/verify/vault/README.md) connects these steps to the implementation. That connection is documented and tested; the model checker itself does not execute Rust.

## Exercise the implementation with changing histories

Gobstopper also tests actual transcript files through sequences of edits, snapshots, restores, and appended records. Hegel, a property-testing library, generates those sequences and reduces a failure to a shorter sequence that still fails.

After an edit, the tests check record links and protected output. After a restore, they compare the restored bytes with the original. This catches a different class of problem from the concurrency model: a transform can preserve the archive’s locking rules while still writing the wrong bytes.

For example, a generated history exposed an edit whose replacement stub was larger than the output it replaced. Another exposed a repeated edit that changed the bytes a second time. Keeping those short histories as regression tests checks the concrete mistakes alongside the more general rules.

Models, file tests, and process-interruption tests cover different failure conditions. Keep a separate backup for hardware loss, and use the archive’s own commands for reads and cleanup so they participate in its locking protocol.

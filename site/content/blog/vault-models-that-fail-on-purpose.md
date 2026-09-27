Before Gobstopper writes a compacted copy of a Claude Code or Codex session, it saves the original transcript into a local archive, and a cleanup command can later remove old snapshots. To trust that archive, you need to know what happens when the machine dies halfway through a save, or when cleanup runs during one. Gobstopper checks the archive's design with a model checker that tries a crash at every step, in every order, within a small fixed world, and each model has broken copies, each with one safety rule switched off, that must reproduce the loss that rule prevents. It also tests the Rust code with random sequences of edits and restores. Neither check covers a power cut that scrambles unsynced disk writes.

**Status:** Latest release: {{release.version}}. The release includes the models and tests described here.

## Storage bugs show up weeks after the cause

Two programs share a folder. One is writing a new file into it, piece by piece. The other is tidying up, deleting anything it thinks nobody needs. Each works when it runs alone. Run them at the same moment, and the tidier can decide a half-written file is junk, wait for the writer to finish and register it, and then delete the pieces it already marked. The writer reports success, and the file is gone.

Gobstopper's archive has this shape. Storage bugs like this one are the hardest to find late, because the symptom shows up weeks after the cause. A typical one appears only if you start a save, lose power, reopen, and then run cleanup before the next save. Nobody types that sequence into a test by hand, and code a model wrote quickly, checked only on the path where nothing goes wrong, rarely handles it.

An interrupted save is ordinary: laptops sleep, terminals close, processes get killed. What matters is what the half-finished save leaves behind, and whether another part of the program can mistake that leftover state for garbage, or for success.

## Why each check comes with a broken copy

You can write down the few things that must always be true about the archive, such as "a snapshot the index lists still has all its pieces," and have a tool try every ordering of saves, reads, cleanups, and crashes in a small world, stopping if any ordering breaks a rule. When that search finishes clean, a whole class of failure is ruled out for that design at that size, including orderings no person would think to test.

A search can also pass for a bad reason: a model that never lets anything happen will never break a rule. So each check comes with broken versions, copies of the design with one safety rule switched off, and each must fail with the loss that rule exists to prevent. If a broken copy passes, the check is not looking at what it claims to.

## How Gobstopper saves a snapshot

The archive stores a transcript as chunks named by their content hash, a per-snapshot list of the chunks it needs, and an index of snapshots. A save runs in a fixed order:

1. Write the chunks.
2. Write the snapshot's chunk list.
3. Read it back and check that the snapshot reconstructs.
4. Add it to the index.
5. When the save belongs to a compaction, record a pin, a note that an operation depends on this snapshot, so cleanup must keep it.

Saves and reads take a shared lock on the archive folder. Cleanup takes an exclusive lock, so it cannot run while any save or read holds the shared one. Cleanup keeps the snapshots its retention setting selects plus every pinned one, and deletes chunks that no kept snapshot refers to.

## The four rules the archive model checks

The archive model is written in TLA+, a language for describing a system as states and the steps between them, and the TLC model checker walks every reachable state. The model has two writers, one reader, one cleanup pass, three snapshots, and four chunks, with one chunk shared between the old snapshot and both new ones. A writer can crash after any step. The reader and cleanup can crash too, and a crash releases the lock. Cleanup may keep any subset of snapshots, including none, so the check covers every retention setting at once.

The rules, in plain words:

- Every snapshot in the index still has its chunk list and every chunk.
- Every pinned snapshot can still be rebuilt, even after the writer that pinned it has crashed.
- A reader that has started rebuilding a snapshot cannot have it deleted underneath it.
- Cleanup never holds its exclusive lock while a writer or reader is active.

Here is the first rule as the model states it (a manifest is a snapshot's chunk list, and `Parts(s)` is the set of chunks it names):

```tla
DataValid(s) == s \in manifests /\ Parts(s) \subseteq objects
IndexedData  == \A s \in index : DataValid(s)
```

In the run recorded on 24 September 2026, the safe configuration explored 25,810 distinct states and finished with nothing left to explore, so every reachable state satisfied all four rules.

## Removing the lock loses indexed data

The first broken copy removes the lock, and the checker must then find a way to break the index rule. It does: a writer stores its chunks, cleanup marks them as unreferenced because no chunk list names them yet, the writer finishes and indexes its snapshot, and cleanup deletes the chunks it marked. The index now lists a snapshot that cannot be rebuilt. The model even gives cleanup an atomic check that the index has not changed before it acts, and the race still gets through. That is why saves and cleanup share a lock instead of relying on a last-moment comparison.

The second broken copy keeps the lock but lets cleanup ignore pins. The checker must break the pin rule: a writer pins its snapshot, crashes, releases its lock, and cleanup collects the data that recovery would have needed.

Two more configurations check the opposite direction. One must reach a state where both writers, the reader, and cleanup all finish. Another must reach a crashed writer whose pin still protects its snapshot. Together they show that the model lets work finish and lets it be interrupted, so the rules held while things happened.

The test runner does not accept just any failure as the expected one. Each broken copy must fail on its named rule, with a complete log. A timeout, a parse error, or a different broken rule counts as a failed check.

## Publishing a copy and recovering after a crash

A second model covers what happens after the snapshot: writing the compacted copy and recovering if the process dies. It has one writer, one reader, up to two cleanup passes, and one restart, and it includes torn index and record states, a conflicting file at the destination, and a failed delete. Its rules:

- The compacted copy appears only after a durable record says an operation is in progress.
- An operation is marked complete only after the copy is visible and the folder has been synced.
- A damaged index or operation record stops cleanup entirely.
- A snapshot's chunk list never outlives its chunks, even when a delete fails partway.

Four broken copies each switch one of those off and must fail on it. A fifth configuration must reach a completed operation after an interruption and a restart. Recovery never reruns the compaction; it checks the stored record and either confirms that the output matches it exactly or refuses. In the same 24 September run, this model explored 28,082 distinct states and finished.

## A model for a feature that is still off

Gobstopper's watch mode has a separate model for asking a provider to compact a live session. That path is turned off in released builds, and the model checks the design before it is turned on. The main rule is to write down "about to send" before sending. A crash between that note and the request leaves the same record as a request whose answer was lost, so the only safe reading is "unknown." The model checks that:

- a timer or a configuration change never clears an unknown outcome;
- each operation is sent at most once;
- an acknowledgement alone never counts as success;
- the recovery snapshot is pinned before the operation is recorded;
- uncertainty never triggers an automatic fallback to editing files;
- an unknown outcome is resolved only with saved evidence that exactly matches the request.

Five broken copies must each fail on one of those rules, and six more configurations must reach real outcomes, including a crash that leaves a request in the "sent, outcome unknown" state. The safe configuration covers two watchers and two operations. It explored 32,251 distinct states in the 24 September run.

## Random edits against the Rust code

The models run none of Gobstopper's Rust. To test the code itself, Gobstopper uses Hegel, a property-testing library that draws random inputs, runs a random sequence of commands, and shrinks any failure to a short case a person can read.

Each test case generates a random Claude Code or Codex transcript, with tool calls and results, side branches, bookkeeping records, and awkward text. It then runs between one and twelve random commands against the production code: shorten some records, insert a summary, snapshot and restore from the archive, or append new records the way a provider would. After every command it checks that the transcript still passes Gobstopper's structural checks, and after each shortening that the fields linking records together have not moved. The shape of the loop, simplified from the test file:

```rust
// Simplified sketch; the real test inlines each step.
for _ in 0..tc.draw(integers().min_value(1).max_value(12)) {
    match tc.draw(integers::<u8>().max_value(3)) {
        0 => shorten_random_records(&tc, &path),
        1 => insert_summary(&tc, &path),
        2 => snapshot_edit_and_restore(&tc, &path, &archive),
        _ => append_like_the_provider(&tc, &path),
    }
    assert_passes_structural_checks(&path);
}
```

The restore step asserts that a snapshot comes back byte for byte, into a new file, without touching the original. A separate property saves the same transcript twice and checks that identical content gets the same identity and restores unchanged. Each property runs 64 generated cases. The tests edit only files they created; released builds do not edit a provider's own session files.

Two regression tests in the same file came from shrunk failures. In one, shortening a Codex output that was already under the size floor produced a stub larger than the original. In the other, shortening the same records twice changed the bytes a second time. Both cases now have fixed tests alongside the random ones.

Process crashes in the Rust code get their own tests. The storage tests kill child processes with `SIGKILL` at declared points, then check that the lock is released and a fresh process recovers.

Gobstopper also keeps a machine-readable record of what each check covers, its limits, and what it excludes. Each model run records the SHA-256 of every model, configuration, and runner file it used, plus hashes of the Java and TLC programs that ran it. A CI script fails the build if a recorded run did not pass or if any listed file has changed since the run, so editing a model without rerunning it cannot leave an out-of-date claim in place. A separate CI job reruns both models and all their broken copies on every pull request and push to main.

## Limits of the models and tests

Gobstopper as a whole is not formally verified. The models check a finite design with a small, fixed number of writers, readers, and cleanup passes. They prove nothing about larger counts, about eventual progress, or about the Rust code itself; the match between model steps and Rust functions is a reviewed table, not a proof. Every save in the models is atomic and durable, so a power cut that loses or reorders unsynced writes, a full disk, a network filesystem, Windows locking, and another program writing into the archive folder without the lock all fall outside them. The Hegel tests run 64 random cases per property on synthetic transcripts, and the kill tests stop processes without cutting power to hardware.

Gobstopper shortens a coding agent's transcript by replacing stale tool output with a short stub, and two proof tools check the parts of that edit most likely to fail at an edge. Kani checks the limits and token arithmetic for every value of their numeric inputs. Lean proves that masking keeps each record's ID, its order, and its tool links, for transcripts of any length. Neither proof shows that a session stays under a context budget or that the original transcript can be rebuilt from the copy.

**Status:** Latest release: {{release.version}}. The release includes the limit functions and proofs described here.

## Where limit bugs hide

A compaction tool edits the record a coding agent depends on most: the running transcript of what it has read, run, and decided. If the rule that picks what to replace is wrong at one edge, the damage shows up later, as an agent that has lost the one error message it needed or a session file its provider refuses to reopen.

Much new code is written quickly by a model and checked against a few examples. It passes the tests someone thought to write and breaks at an input nobody typed; people have started calling this vibe-coded slop. Limit checks are where that kind of bug hides best. A rule that says "at most 64 edits per plan" can be written so it quietly allows 65, or refuses the 64th. A running total of token counts can grow past the largest number the computer stores and wrap around to a small one, so a huge transcript reads as tiny. A "keep the most recent outputs" rule can subtract past zero on a short session. Each of these bugs stays hidden on the session you tested with and appears on the longest, oldest session, the one you most wanted compacted.

The edit itself has the same problem. Replacing old output looks simple, but a careless version can drop a record, swap two, change a record you asked it to protect, or leave a tool call whose result has disappeared. In a Claude Code or Codex transcript, every tool result has to follow its call.

## What a proof adds over a test

A test checks the examples it was given. A proof checks a statement for every input the statement describes. When the statement is "this limit check accepts 64 and refuses 65, for every possible number," no untested value is left for that check, including the largest number the machine can hold. Because the proofs run against the code that ships, a later change that breaks the rule fails the repository's checks before it reaches your transcripts.

Gobstopper uses two tools. Kani checks small Rust functions against every value of their inputs, and Lean proves laws about transcripts of any length.

## Kani checks the limits for every number

Kani is a model checker for Rust. You write a small proof function that asks for arbitrary values, calls the real code, and asserts what must hold. Kani then checks that assertion for every value those inputs can take.

Gobstopper's plan limits live in one small Rust module that the normal validation code calls. Here is the plan-size check and its proof, from the source with the coverage checks left out:

```rust
pub const MAX_EDITS: usize = 64;
pub const MAX_ITEMS: usize = 100_000;

pub fn plan_bounds(edits: usize, items: usize) -> bool {
    edits <= MAX_EDITS && items <= MAX_ITEMS
}

#[kani::proof]
fn production_plan_bounds() {
    let edits: usize = kani::any();
    let items: usize = kani::any();
    let valid = plan_bounds(edits, items);
    assert_eq!(valid, edits < 65 && items < 100_001, "exact production plan bounds");
}
```

The proofs call the functions Gobstopper runs, with the real limits rather than smaller ones that would be easier to check. Each of these laws holds for every value of its numeric inputs:

- A plan with at most 64 edits over at most 100,000 transcript items passes, and anything larger is refused.
- Summary text added to a plan stays at or under 32,768 bytes, and an addition that would overflow is refused instead of wrapping.
- A token estimate is the byte count divided by four and rounded up, never zero, with no overflow at the largest byte count.
- Adding token counts stops at the maximum value instead of wrapping around, so a running total never shrinks.
- The savings estimated for replacing one output, after subtracting the stub's own cost, are never larger than that output's token estimate, and an output with no bytes or a zero estimate saves nothing.
- The count of records outside the protected recent tail is the total minus the tail, or zero when the tail covers everything, with no wraparound for any two inputs.
- Adding an edit to a plan either succeeds and keeps the plan valid, or fails and leaves the plan unchanged. A plan holds at most one summary, and a provider or cache control edit must be the plan's only edit.

Two groups of proofs cover fixed sizes only. The edit-sequence proof checks every sequence of four edits, each of any of the four edit kinds, while the one-step proof covers the real 64-edit limit. The sorted lookup of record positions is checked for lists of length 0, 1, 2, and 4. Everywhere else the inputs are single numbers, and Kani covers their whole range.

To show that the proof can fail, the check plants a bug. It copies the crate, changes `edits <= MAX_EDITS` to `edits < MAX_EDITS` in the copy, and reruns the proof, which passes only if that one assertion fails. CI runs the proofs and this planted bug on every pull request and every push to main.

## Lean proves what masking does to a transcript

Lean is a proof assistant. It checks proofs about definitions, for lists and numbers of any size, down to a small trusted kernel.

Gobstopper's Lean model treats a transcript as a list of records. Each record has a stable ID, flags for whether it is live, eligible, and protected, some content, and an optional tool call or tool result. Masking a set of selected IDs replaces the content of each selected record that is live, eligible, and unprotected with a stub, and leaves every other record alone. Two of the laws, shown without their proofs:

```lean
-- Masking twice with the same selection is the same as masking once.
theorem idempotence (selected : List Nat) (records : List Record) :
    mask selected (mask selected records) = mask selected records

-- A selection that is refused leaves the transcript exactly as it was.
theorem rejection_identity (records : List Record) (selected : List Nat)
    (h : admitted records selected = false) : execute records selected = records
```

The model's 27 theorems include these laws, for transcripts of any length:

- Masking keeps the transcript's length, the order of records, and every record's ID.
- Protected records and records that are not live keep their content.
- Every selection the rules accept names a known record that is live, eligible, and unprotected.
- The sequence of tool calls and tool results is unchanged, so if every result followed its one call before, it still does.
- With the records' flags held fixed, masking with one selection and then another equals masking once with both.
- Appending a summary record with a fresh ID keeps every earlier record in place, keeps tool links valid, and keeps IDs unique.

The checker rebuilds the proofs from scratch, replays them in a fresh Lean kernel, and allows only Lean's two standard axioms.

## Testing that the Lean model matches the Rust code

A proof about a model helps only if the model matches the program, and Gobstopper checks that match with tests. A Lean program runs the proved operations on 20 synthetic cases and writes the expected results to a file. A Rust test builds synthetic transcript files in the Codex and Claude Code formats, runs the same 23 steps through Gobstopper's validation and transform code for each provider, and compares the output record by record. Of the 23 steps, 12 are accepted, including 3 that add a summary, and 11 are refused.

Two planted bugs show that the comparison can fail from either side. One changes the Lean side to write a masked record's value as 1 instead of 0. The other changes the Codex writer in Rust so it replaces nothing. Each must make the comparison fail on the expected record. CI runs the Lean proofs, the comparison test, and both planted bugs on every pull request and push to main.

The public repository keeps a claims register that states the scope and exclusions of each check, and it lists both the Kani and the Lean claim as holding only for their stated inputs and limits. The passing runs for both are dated September 24, 2026, and each run record names hashes of the source files and tool binaries it checked. A change to any of those files invalidates the record until the checks pass again; on September 24 the recorded hashes still matched the main branch.

## What the proofs do not cover

The proofs do not show that a session stays under any context budget, because Gobstopper's token counts are estimates at four bytes per token, not the provider's tokenizer or bill. The Lean laws describe the structure of an edit. They do not show that a compacted transcript can be turned back into the original, or that a stub keeps what a later task needs; getting the original back relies on Gobstopper's archive, which these proofs do not cover. The Rust comparison covers 20 small, valid, linear synthetic histories, so arbitrary provider files, branched sessions, and a live resume fall outside it. Parsing, file writes, sorting, allocation, the compiler, and the operating system are trusted rather than proved, and a Kani result holds for the platform it ran on. Gobstopper's own records list whole-system correctness as a goal with no complete proof.

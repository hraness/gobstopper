Shortening a transcript changes two things: how much text it carries and which records remain connected. An edit can save space while breaking a tool call, changing a protected result, or overflowing the arithmetic that decides whether it is allowed. These are separate problems, and they need different checks.

Gobstopper uses Kani to check small functions in its Rust implementation and Lean to prove laws about a transcript model. The distinction matters: one checks the numeric decisions the program executes; the other describes what a structurally valid edit must preserve.

## Start with a boundary that can go wrong

Consider Gobstopper’s plan-size boundary: at most 64 edits over at most 100,000 transcript items. The limit includes its endpoint: 64 edits are allowed, and 65 are refused. An accidental change from `<=` to `<` would reject a valid plan without crashing the program.

This is the production check, with a shortened version of its Kani proof:

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
    assert_eq!(plan_bounds(edits, items), edits < 65 && items < 100_001);
}
```

Kani explores arbitrary values of these machine-sized integers. It checks the endpoint and the largest representable values, where ordinary arithmetic may overflow. Similar checks cover adding token estimates, calculating how much an edit saves, and excluding the protected recent tail.

Each statement has a scope. Scalar arithmetic can be checked across its full numeric range; a sequence of edits needs an explicit length bound. The [proof reference](https://github.com/hraness/gobstopper/blob/main/verify/core/README.md) records those scopes beside the functions they cover.

Token estimates still use bytes as a proxy for tokens. Correct arithmetic makes that estimate internally consistent; the provider’s tokenizer determines the actual token count.

## Preserve relationships while replacing content

Consider three records: a tool call, its result, and the user’s next instruction. Replacing the result’s long text with a stub should leave the call-result relationship and the instruction intact. Removing the result record entirely can break that relationship even if the remaining text looks readable.

Gobstopper’s Lean model represents a transcript as a list of records with stable IDs, content, protection flags, and optional tool links. Masking replaces the content of selected eligible records. The model proves that masking preserves record order, IDs, and the sequence of tool calls and results. Protected records keep their content.

Two useful laws can be expressed compactly:

```lean
-- Repeating the same edit has no further effect.
theorem idempotence (selected : List Nat) (records : List Record) :
    mask selected (mask selected records) = mask selected records

-- Refusing an edit leaves the transcript unchanged.
theorem rejection_identity (records : List Record) (selected : List Nat)
    (h : admitted records selected = false) : execute records selected = records
```

With the selected IDs and protection flags held fixed, idempotence makes a repeated masking operation predictable. Rejection identity prevents a failed operation from leaving a partly modified transcript. Lean checks these laws for lists of any length within the model’s definitions.

## Connect the model to the program

A proved model can still describe the wrong program. Gobstopper therefore runs the same synthetic histories through the Lean model and the Rust transforms for Codex and Claude Code, then compares the resulting records. The cases include accepted edits, refused edits, and appended summaries.

Deliberately broken variants check the comparison itself. One makes the model write the wrong replacement value; another makes a Rust writer skip the replacement. Both must produce a mismatch. A check that accepts those variants would provide no useful evidence about correspondence.

These comparisons cover their chosen histories. They do not extend Lean’s proof to every possible provider file. The [transcript reference](https://github.com/hraness/gobstopper/blob/main/verify/transcript/README.md) describes the model and its connection to the Rust code.

## Use each result for the decision it supports

The arithmetic checks support enforcing edit limits. The list laws support preserving transcript structure. Neither decides whether a removed paragraph will matter to the agent’s next task, and neither guarantees that a provider will resume a prepared copy.

For file-based edits, Gobstopper writes a separate copy and archives the original. Inspect the proposed edit, try the copy with your provider, and keep the archive available when you need an exact detail that compaction removed. The [archive article](/blog/vault-models-that-fail-on-purpose) explains how saves and cleanup coordinate to keep those originals recoverable.

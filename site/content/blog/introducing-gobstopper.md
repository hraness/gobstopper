Gobstopper is a free, open-source command-line tool that makes long Claude Code and Codex sessions smaller. For a saved session, it removes stale tool output by a rule you choose, and before it writes anything it stores the original transcript in a local archive. If a compaction drops something you needed, you can search the archive for it or restore the whole session.

**Status:** Latest release: {{release.version}}. The release includes `gobstopper proxy`, which compacts the requests of a running session, and has carried the OpenAI Chat Completions dialect for opencode, Crush, Aider, and Goose since 0.4.0.

## Old tool output crowds out what the next turn needs

Late in a long coding-agent session, much of the context can be old tool output: test logs from runs that have since passed, file listings from before a refactor, a stack trace for a bug that is already fixed. The agent rereads all of it on every turn. Somewhere in that pile is the one error message or constraint the next step depends on.

Providers offer their own compaction, which usually replaces the history with a summary a model writes. The context gets smaller, but the summary can lose detail, and it does not tell you what it left out.

Gobstopper treats compaction as an edit you can inspect and undo. A rule decides which records to shorten, and a preview shows what would change. The original bytes go into an archive before Gobstopper writes a new copy, and it checks the copy for structural damage, such as a tool call left without its result, before publishing it.

## Who should use it, and who should use something else

Gobstopper suits developers who run long Claude Code or Codex sessions and want a smaller context without silently losing the details the next turn needs. It also suits anyone who wants to measure that tradeoff on their own sessions before trusting a compaction method.

The file commands in this post work on saved session files and never touch a running session, because a live session's context belongs to the provider process that loaded it. To keep a running session small, use `gobstopper proxy`, which sits between the agent and its provider and shrinks each request past a threshold, or the provider's own `/compact`.

If you use xcb, it already applies Gobstopper's elision policy to drop stale tool output from Claude Code and Codex prompts once context passes a threshold, and it keeps the original output in local history. That plugin is on by default, and you can turn it off.

## Try it on a saved session

Install it from source:

```sh
cargo install --git https://github.com/hraness/gobstopper gobstopper
```

List your sessions and preview a compaction at the size you want:

```sh
gobstopper detect
gobstopper plan <session> --trigger 100000 --floor 30000
```

The trigger is the context size at which Gobstopper starts to act, and the floor is the size it aims for. `plan` writes nothing. When it decides not to act, `plan --json` gives a reason code such as `below_trigger` or `minimum_savings_not_met`. One code, `strategy_returned_no_plan`, means the strategy declined for a reason Gobstopper cannot see.

The simplest rule, `elide`, replaces stale tool outputs with a short stub, oldest first, until the estimate reaches the floor. It leaves the most recent outputs alone; by default that is the last eight. A replaced record reads like this:

```text
[output elided by gobstopper: 512 bytes]
```

Other built-in rules remove duplicate outputs by content hash, keep only the newest outputs per tool, protect both ends of the transcript, or add a short state card built from session metadata. They run on local logic and call no model; model scoring for the `scored` rule is opt-in. To compare the rules on the same frozen copy of a session before you pick one, run:

```sh
gobstopper eval <session>
```

When you are ready, `apply` writes a separate compacted copy for Claude Code or Codex and leaves your original session file alone:

```sh
gobstopper apply <session> --strategy elide
```

Before the copy is published, the source and the new bytes go into an archive under `~/.local/share/gobstopper/vault/`, stored by content hash. Every search or read checks the stored bytes against their hashes. To find an error message that a compaction removed, search one archived snapshot and read only the matching record:

```sh
gobstopper search-snapshot <snapshot-sha> --query 'exact error text' --json
gobstopper read-snapshot <snapshot-sha> --record 42 --max-bytes 4096 --json
```

Search matches literal, case-sensitive text; there is no semantic search. To go back to the whole session, `gobstopper undo <session>` restores a snapshot into a new fork.

`gobstopper mcp` runs a read-only Model Context Protocol server with tools for recall, history, diffs, plans, and structural checks, and none of its tools changes a transcript. An agent can use the snapshot search and read tools only when you start the server with `--allow-transcript-content`; archived text the agent retrieves then goes to that agent and its model provider.

## What a replay of 729 sessions showed

On September 19, 2026, an offline replay ran the `compacted` strategy over 729 archived sessions from one Mac. Across all 729, the median projected reduction was 0%, because 637 sessions produced no plan. Across the 73 high-context archived Codex root tasks, the median projected context reduction was 36.4%, and 76.9% of sampled strings survived. These are offline estimates; they do not measure billing savings or task accuracy. The [benchmarks page](/benchmarks) has every cohort and the method.

## Where it is going

The roadmap describes Gobstopper as the context-compaction layer for the Hraness agent stack: a policy engine that agent runtimes such as xcb embed, sharing measurement with AI Charts. The plan is to steer provider-native compaction rather than only trigger it, and to compact at natural breaks between tasks instead of at a flat token count. None of this has shipped.

## Limits

Some core pieces are checked with more than tests. The archive's concurrency design has TLA+ models, each with deliberately broken variants that must fail; [How Gobstopper model-checks its archive against crashes](/blog/vault-models-that-fail-on-purpose) walks through them. The limits on an edit plan, such as at most 64 edits and at most one state card, are small Rust functions that the production code and Kani proofs share, and Lean proves list laws about transcripts; [What Kani and Lean prove about Gobstopper's compaction](/blog/proofs-for-the-admission-math) explains both. Each check covers one component at a stated scope. The TLA+ models check a finite design rather than the Rust code, Kani covers selected arithmetic, and the Lean laws are checked against the Rust code only on finite cases. None of them establishes whole-system correctness, behavior after a power loss, or that a provider will accept a copy.

A copy that passes Gobstopper's structural checks may still fail to resume in Claude Code or Codex, so test a copy before you rely on it. Released builds cannot ask a provider to compact a live session, and direct rewrites of provider files or stores are turned off. The archive returns a missing detail only when you or your agent ask for it.

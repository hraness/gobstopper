Gobstopper is a free, open-source command-line tool that makes long Claude Code and Codex sessions smaller. It removes stale tool output by a rule you choose, and before it changes anything it stores the original transcript in a local archive. If a compaction drops something you needed, you can search the archive for it or restore the whole session.

**Status:** Latest release: {{release.version}}. The release includes the request proxy described on the homepage, with the OpenAI Chat Completions dialect for opencode, Crush, Aider, and Goose since 0.4.0.

## The problem it solves

Late in a long coding-agent session, much of the context can be old tool output: test logs from runs that have since passed, file listings from before a refactor, a stack trace for a bug that is already fixed. The agent still reads all of it on every turn. Somewhere in that pile is the one error message or constraint the next step depends on.

Providers offer their own compaction, which usually replaces history with a model-written summary. That makes the context smaller, but a summary can lose detail, and it does not show you what it left out.

Gobstopper treats compaction as an edit you can inspect and undo. A rule decides which records to shorten. A preview shows what would change. The original bytes go into an archive before a new copy is written, and the copy is checked for structural problems, such as a tool call left without its result, before it is published.

## Who it is for

Gobstopper is for developers who run long Claude Code or Codex sessions and want a smaller context without silently losing the details the next turn needs. It also suits people who want to measure that tradeoff on their own sessions before trusting any compaction method.

Use something else if you want a running session's context shrunk in place. A live session's context belongs to the provider process that loaded it, and Gobstopper does not reach into it. For that, use the provider's own `/compact`; Gobstopper can tell you when a session has crossed your threshold.

If you use xcb, it already applies Gobstopper's elision policy to drop stale tool output from Claude Code and Codex prompts once context passes a threshold, and it keeps the original output in local history. That plugin is on by default, and you can turn it off.

## What it does today

Install it from source:

```sh
cargo install --git https://github.com/hraness/gobstopper gobstopper
```

Find your sessions and preview a compaction at the size you want:

```sh
gobstopper detect
gobstopper plan <session> --trigger 100000 --floor 30000
```

The trigger is the context size at which Gobstopper starts to act, and the floor is the size it aims for. `plan` writes nothing. When it decides not to act, `plan --json` returns a reason code such as `below_trigger` or `minimum_savings_not_met`. One code, `strategy_returned_no_plan`, means the strategy declined for a reason Gobstopper does not know.

The simplest rule, `elide`, replaces stale tool outputs with a short stub, oldest first, until the estimate reaches the floor. The most recent outputs are protected; by default that is the last eight. A replaced record reads like this:

```text
[output elided by gobstopper: 512 bytes]
```

Other built-in rules remove duplicate outputs by content hash, keep only the newest outputs per tool, protect both ends of the transcript, or add a short state card built from session metadata. The built-in rules use local logic and need no model; model scoring for the `scored` rule is opt-in. You can compare them on the same frozen copy of a session before you pick one:

```sh
gobstopper eval <session>
```

When you are ready, `apply` writes a separate compacted copy for Claude Code or Codex. Your original session file is left alone:

```sh
gobstopper apply <session> --strategy elide
```

Before the copy is published, the source and the candidate bytes go into a content-addressed archive under `~/.local/share/gobstopper/vault/`. Searching or reading a snapshot checks the stored bytes against their hashes. To find an error message that a compaction removed, search one archived snapshot and read only the matching record:

```sh
gobstopper search-snapshot <snapshot-sha> --query 'exact error text' --json
gobstopper read-snapshot <snapshot-sha> --record 42 --max-bytes 4096 --json
```

Search matches literal, case-sensitive text; there is no semantic search. To go back to the whole session, `gobstopper undo <session>` restores a snapshot into a new fork. `gobstopper mcp` runs a read-only Model Context Protocol server with tools for recall, history, diffs, plans, and structural checks. It has no tool that changes a transcript. An agent can use the snapshot search and read tools only when you start the server with `--allow-transcript-content`; archived text the agent retrieves then goes to that agent and its model provider.

On September 19, 2026, an offline replay ran the `compacted` strategy over 729 archived sessions from one Mac. Across 73 high-context archived Codex root tasks, it had a median projected context reduction of 36.4% and kept 76.9% of sampled strings. Across all 729 sessions, 637 produced no plan and the median reduction was 0%. These are offline estimates, not billing savings or measures of task accuracy. The [benchmarks page](/benchmarks) has every cohort and the method.

## Where it is going

The roadmap describes Gobstopper as the context-compaction layer for the Hraness agent stack: a policy engine that agent runtimes such as xcb embed, with measurement shared with AI Charts. The intent is to steer provider-native compaction rather than only trigger it, and to compact at natural breaks between tasks instead of at a flat token count. None of this has shipped.

Some core pieces are checked with more than tests today. The archive's concurrency design has TLA+ models, including deliberately broken variants that must fail; [Vault models that fail on purpose](/blog/vault-models-that-fail-on-purpose) walks through them. The limits on an edit plan, such as at most 64 edits and at most one state card per plan, are small Rust functions shared by the production code and Kani proofs; [Proofs for the plan-size arithmetic](/blog/proofs-for-the-admission-math) explains what those proofs cover.

## Limits

Each of those checks covers a specific component at a stated scope. The TLA+ models check a finite design, not the Rust code. The Kani proofs cover selected arithmetic, not whole programs. Lean proves list laws about transcripts, with finite checks against the Rust code. None of them establish whole-system correctness, behavior after a power loss, or that a provider will accept a copy.

A compacted copy passing Gobstopper's structural checks does not mean Claude Code or Codex will resume it; test that yourself before relying on it. Released builds cannot ask a provider to compact a live session, and direct rewrites of provider files or stores are turned off. The archive returns a missing detail only when you or your agent ask for it.

Gobstopper is a free, open-source command-line tool that makes long coding sessions smaller. Its main tool, `gobstopper proxy`, runs on your machine between a coding agent and its model provider, and once a request passes a size you set, it sends the start of the session and the last three turns word for word and replaces the middle with a summary copied out of the session, not written by a model.

**Status:** Latest release: {{release.version}}. Since v0.7.3 the proxy keeps only the last three turns verbatim by default, because in our one-trial benchmark at a 45,000-token threshold, keeping more cost more.

A coding agent has no memory between steps, so every time it asks the model for the next move it sends the whole conversation again: your first request, every file it opened, every command it ran. Gobstopper keeps the brief and the latest exchanges intact and trims the old middle to a list of what was said and which commands ran. Long outputs are left out of that list because they are still on disk, and your saved session is never edited.

![Diagram: a tall stack of nine blocks crosses a threshold line. Beside it, with Gobstopper, five blocks sit under the line: your task, one summary block, and the last three turns.](/blog/introducing-gobstopper/gob-fuse.png)

*Figure 1. Gobstopper keeps the start and the last three turns word for word and replaces the middle with a mechanical summary. No model writes it. Your files and your saved session are not changed.*

{{film}}

## Every step resends the whole session

Request 40 of a session carries the 39 steps before it, and request 41 carries them again. If each step adds a few thousand tokens, the total a session sends grows with the square of its length. Prompt caching discounts the repeated part, but the provider still counts it: in the Terminal-Bench run below, 87% of the input Claude Code sent with no proxy was cache reads, the same context read back again on each step.

![Diagram: five columns of stacked blocks, one per step. Each column is one block taller than the last, because every request resends all earlier steps.](/blog/introducing-gobstopper/gob-resend.png)

*Figure 2. A coding agent has no memory between steps, so each request carries your first message and every file and command output since. By step 40 it resends 39 steps it has already sent.*

Agents have their own answer. Claude Code compacts on its own as the context nears its window, by asking a model to summarize the history. That shrinks the context, but until it fires every request carries the full load, and the summary can drop a detail without saying which. Gobstopper's proxy acts earlier, at a threshold you choose, and its summary is copied rather than rewritten, so the agent's own compaction does not reach its trigger.

## What Terminal-Bench showed

On September 27 and 28, 2026, we ran Terminal-Bench 2.1's 89 tasks three times with Claude Code 2.1.283 and GLM 5.3 Flash through Vercel AI Gateway: once through Gobstopper at tail 0 (the new default), once through Gobstopper at tail 40 (the old default), and once with no proxy. Each arm had one trial per task. Gobstopper was v0.7.2 at a 45,000-token threshold, the setting CliffCompaction's paper used; the shipped default is 128,000. The tasks ran as x86 images under emulation on one Mac.

Tokens first. Gobstopper, tail 0 sent 84.3 million input tokens over the 89 tasks, against 118.6 million for Claude Code, no proxy: 29% fewer, with 33% fewer cache reads (68.7 million against 102.6 million). New input (15.6 million against 15.9 million) and output (2.64 million against 2.71 million) were about equal, so the difference is the resent context, which is exactly what the proxy trims. These are provider-reported counts from one trial.

![Bar chart of total input tokens over 89 Terminal-Bench tasks. Gobstopper, tail 0: 84.3 million, 61 solved. Claude Code, no proxy: 118.6 million, 60 solved. Gobstopper, tail 40 (old default): 118.5 million, 59 solved. Cache reads make up most of each bar.](/blog/introducing-gobstopper/gob-tokens.png)

*Figure 3. Total input over 89 tasks. Almost all of the difference is cache reads, the context resent on every step: 68.7M vs 102.6M. New input and output were about equal.*
Terminal-Bench 2.1 · 89 tasks · one trial per arm · Claude Code 2.1.283 with GLM 5.3 Flash via Vercel AI Gateway · Gobstopper v0.7.2, 45,000-token threshold (default 128,000) · September 27–28, 2026 · 21 of 89 tail-0 trials may have run an earlier build.

Tasks solved did not move in a way one trial can detect. Gobstopper, tail 0 solved 61 of 89, Claude Code, no proxy 60, and Gobstopper, tail 40 (old default) 59. Each rate has a 95% interval about 19 points wide, and a paired test between tail 0 and no proxy gives p = 1.0. That is no measurable difference, not an improvement.

![Dot plot of tasks solved out of 89 with 95% intervals: Gobstopper, tail 0, 61 (58.3 to 77.2%); Claude Code, no proxy, 60 (57.1 to 76.3%); Gobstopper, tail 40 (old default), 59 (56.0 to 75.3%). The intervals overlap almost completely.](/blog/introducing-gobstopper/gob-solved.png)

*Figure 4. Share of 89 tasks resolved, with Wilson 95% intervals. One trial per arm.*
Terminal-Bench 2.1 · Gobstopper v0.7.2, 45,000-token threshold (default 128,000) · September 27–28, 2026 · 21 of 89 tail-0 trials may have run an earlier build.

The churn between arms shows why. 43 tasks were solved by every arm and 15 by none; the other 31 were solved by some arms and not others. One trial cannot tell whether a flip came from the arm or from ordinary run-to-run variance, so a one- or two-task difference in solved counts is not evidence either way.

![Bar of 89 tasks: 43 solved by all three arms, 31 solved by some arms but not others, 15 solved by none.](/blog/introducing-gobstopper/gob-churn.png)

*Figure 5. With one trial per arm, 31 of 89 tasks resolved differently between arms, so a one- or two-task difference in solved counts is within single-trial noise.*
Terminal-Bench 2.1 · one trial per arm · September 27–28, 2026.

Cost, labelled for what it is: provider-reported, metered through Vercel AI Gateway, for this model. Gobstopper, tail 0 came to $5.72 in total ($0.064 per task) and Claude Code, no proxy to $6.82 ($0.077 per task), about 16% lower. With one trial that is not statistically significant: the 95% interval runs from 32% lower to 2% higher. GLM 5.3 Flash prices cache reads at a fifth of new input; providers that price them lower would see less of the token cut in dollars, and a subscription is not billed per token at all.

## Why the default changed

The tail budget was Gobstopper's own idea. CliffCompaction keeps the last three turns verbatim; `--keep-tail-percent 40` also kept older whole turns while they fit 40% of the room under the threshold, on the theory that more recent context would spare the agent some re-reads. Until v0.7.3 that was the default.

It lost. At the same threshold, Gobstopper, tail 40 (old default) sent 118.5 million input tokens, as many as no proxy, and cost $7.97, 39% more than tail 0. It also cost 17% more than no proxy in this run, which is not significant (95% interval −14% to +60%). Put the other way, tail 0 cost 28% less than tail 40, with a 95% interval of 1.6% to 46.5% less. It is the only cost comparison in the run whose interval excludes zero, and only just.

![Diagram: two stacks after a rewrite. Tail 0 keeps your task, a summary and the last three turns. Tail 40 also keeps older turns, so it sits closer to the threshold and is rewritten again sooner.](/blog/introducing-gobstopper/gob-tail.png)

*Figure 6. In the benchmark, tail 40 cost 39% more than tail 0 in provider-reported terms (95% interval 2% to 87% more), at a 45,000-token threshold. v0.7.3 makes tail 0 the default.*
Terminal-Bench 2.1 · one trial per arm · Gobstopper v0.7.2 · September 27–28, 2026 · 21 of 89 tail-0 trials may have run an earlier build.

We think the reason is the tail itself, though this run does not prove it. A bigger tail leaves each compacted request closer to the threshold, so the next rewrite should come sooner, each rewrite throws away a larger cached block, and every request in between is heavier. Replay points that way: over 24 recorded sessions, tail 40 compacted 369 times against 343 for tail 0 at 32,000 tokens, and 50 against 38 at 128,000. The live logs do not settle it. The median compacted request was about the same size in both arms (31,000 and 31,500 estimated tokens), and the tail 0 log covers only about 68 of its 89 trials. The tail 40 arm ran while host load was about 58 to 68, which may have stretched some of its tasks.

![Bar chart of extra cost for tail 40 over tail 0 per task: video-processing plus 1.02 dollars, winning-avg-corewars plus 0.52, path-tracing-reverse plus 0.29, path-tracing plus 0.29, schemelike-metacircular-eval plus 0.25, and the other 84 tasks together minus 0.12.](/blog/introducing-gobstopper/gob-gap.png)

*Figure 7. Provider-reported cost difference, tail 40 minus tail 0, per task. Five of 89 tasks account for 105% of the $2.25 gap; the other 84 net slightly negative. This is why the one significant cost result should not be over-read.*
Terminal-Bench 2.1 · one trial per arm · September 27–28, 2026.

That caveat matters: one task, video-processing, carries $1.02 of the $2.25 difference. We changed the default because tail 40 bought nothing measurable and cost more on the tasks where it mattered, not because the result is settled. Tail 40 remains a flag.

## How it keeps the cache warm

This section is for readers who want the mechanism. When a request's estimated size passes the threshold (characters divided by four), `gobstopper proxy` sends three parts:

1. **The head, verbatim.** The system prompt and everything before the first assistant message.
2. **One user-role summary.** Your messages, the assistant's visible text, each tool call as a one-line signature, and tool results of at most 500 characters. Longer results, images and documents are dropped, and signed thinking is never resent.
3. **The last three turns, verbatim.**

The rewritten prefix is kept in memory, keyed by a hash of the original. Until the next crossing, every request reuses the same head and summary bytes, so the provider's prompt cache keeps matching a short prefix instead of a growing one. Each crossing costs one cold prefix and then matches again. Request size traces a sawtooth: it drops to a floor, grows a step at a time back to the threshold, and drops again, so a session's total input grows about linearly instead of with the square of its length.

![Line chart of estimated tokens per request over 383 requests of one session. Without the proxy, request size climbs steadily to about 491,000. With Gobstopper at a 45,000-token threshold, it stays under 40,000 in a sawtooth, and total input falls from 116.7 million to 12.9 million estimated tokens.](/blog/introducing-gobstopper/gob-sawtooth.png)

*Figure 8. One recorded Claude Code session replayed at a 45,000-token threshold (the default is 128,000). The shaded areas are what each request resends; with Gobstopper the area is about a ninth as large. At 128,000 the same session is rewritten 10 times instead of 50.*
Estimated tokens (4 characters per token), not billed · one recorded Claude Code session, 383 requests · estimate, replay, build f4db57e (request engine identical to v0.7.2) · calibration on.

A summary is never summarized. Each crossing rebuilds from the full history the client resends and discards the previous summary; Gobstopper then carries your words and the assistant's replies from turns earlier compactions summarized, up to 24,000 characters. In the logged part of the tail 0 arm (about 68 of 89 trials), compacted requests had a median of 31,500 estimated tokens, against a median of 55,000 before compaction.

![Diagram: a full-width bar labelled original request, and below it a shorter bar of six parts: head, summary, carry, and the last three turns. The rewrite bar is a little over half as long.](/blog/introducing-gobstopper/gob-anatomy.png)

*Figure 9. What a rewritten request carries. The bar lengths use the two medians from the logged part of the tail 0 arm: 55K estimated tokens before compaction and 31.5K after. The example lines are illustrative.*
Terminal-Bench 2.1, tail 0 arm, about 68 of 89 trials logged · Gobstopper v0.7.2 · September 27–28, 2026.

Two more pieces keep it honest. Calibration learns, per provider and model, how far the four-characters-per-token estimate runs below the provider's own count, and after five responses divides the threshold by that ratio, bounded 1.0 to 2.0; `--no-calibrate` turns it off. And the proxy fails open: if it cannot parse a request, hits an internal error, or the provider rejects a rewrite for any reason other than length, it sends the original bytes. A length rejection gets a harsher rewrite and a retry. Over the logged part of the run, about 2,100 rewritten or reused requests across both Gobstopper arms, no request fell back to its original bytes.

The summary rule is CliffCompaction's, from the paper by Trang Nguyen, Eulrang Cho, Bingqing Chen, and Tim Dettmers ([arXiv:2609.26779](https://arxiv.org/abs/2609.26779)), ported to Rust for the Anthropic Messages, OpenAI Responses, and OpenAI Chat Completions dialects. Gobstopper departs from the reference in seven ways, six of them on by default:

| Departure | Default | Restore the reference |
|---|---|---|
| Keeps older whole turns beyond the last three while they fit a tail budget | Off (tail 0 since v0.7.3) | `--keep-tail-percent 0` |
| Counts a run of assistant messages as one turn in every dialect | On | None; tool calls stay paired with results |
| Separate threshold for Anthropic requests that declare a 1M-token window | On | `--threshold-1m` equal to `--threshold` |
| Resends the original on a non-length rejection | On | None |
| Raises the threshold when the head alone approaches it | On | None |
| Carries earlier words, up to 24,000 characters | On | `--carry-max-chars 0` |
| Calibrates the threshold, bounded 1.0 to 2.0 | On | `--no-calibrate` |

The authors report, for their own proxy in a Terminal-Bench 2.1 run through Claude Code with GLM 5.3 Flash, 76.69% at a 45,000-token threshold against 73.03% for Claude Code's default auto-compaction and 70.97% for its auto-compaction at about 45,000 tokens. Those are their figures, on their setup. Our run cannot tell its arms apart, and its absolute rates are about 8 points lower, which we infer (not measured) comes from 18 to 19 timeouts per arm under emulation and host load. It had no arm matching their 70.97%. The [comparison with CliffCompaction](https://gobstopper.sh/compare/cliffcompaction) has the rest.

## Try it

Install from source and run one Claude Code session through a temporary proxy:

```sh
cargo install --git https://github.com/hraness/gobstopper gobstopper
gobstopper proxy run -- claude
```

To match the benchmark, add `--threshold 45000` before the `--`. The default, 128,000, compacts later, and in replays most recorded Claude Code sessions never reach it. On macOS, `gobstopper proxy install` starts the proxy at login on 127.0.0.1:8260, and `gobstopper proxy status` shows its counters. Codex, opencode, Crush, Aider, and Goose route through the same proxy; the [docs](/docs#compact-live-coding-agent-requests) have the setup for each. The proxy needs curl 8.3 or later and never writes session files.

## Saved sessions and the vault

Gobstopper also works on saved Claude Code and Codex session files, as an edit you can inspect and undo. It never touches a running session's files: it writes a separate compacted copy, and before it does, it stores the original bytes in a content-addressed archive on your machine.

```sh
gobstopper detect
gobstopper plan <session> --trigger 100000 --floor 30000
gobstopper eval <session>
gobstopper apply <session> --strategy elide
```

`plan` writes nothing and, with `--json`, gives a reason code such as `below_trigger` when it declines to act. `eval` compares the built-in rules on the same frozen copy. The simplest rule, `elide`, replaces stale tool outputs with a stub, oldest first, and leaves the last eight alone:

```text
[output elided by gobstopper: 512 bytes]
```

The archive lives under `~/.local/share/gobstopper/vault/`, and every read checks the stored bytes against their hashes. To find an error message a compaction removed, search one snapshot and read only the matching record; to go back to the whole session, restore a snapshot into a new fork:

```sh
gobstopper search-snapshot <snapshot-sha> --query 'exact error text' --json
gobstopper read-snapshot <snapshot-sha> --record 42 --max-bytes 4096 --json
gobstopper undo <session>
```

Search matches literal, case-sensitive text. `gobstopper mcp` exposes the same recall read-only to an agent, and returns archived text only when you start it with `--allow-transcript-content`; that text then goes to the agent and its model provider. If you use xcb, it already applies Gobstopper's elision policy to its prompts and keeps the original output in local history.

## What the numbers do not show

One trial per arm is too few to rank the arms on tasks solved, and it leaves the cost difference with no proxy inside the noise. A three-trial rerun is planned and will replace these figures. The run used GLM 5.3 Flash, not an Anthropic model, so it says nothing yet about Claude models under the proxy, and its dollars are one gateway's prices for one model. Anthropic prices cache reads lower and bills cache writes, so the same token cut is worth less there in percent. 21 of the 89 tail 0 trials may have run an earlier build, because the proxy's logs from before a restart were lost. And only the 45,000-token threshold was measured live.

Offline replays, which estimate rather than bill, show how the threshold changes the picture. Over 24 recorded sessions (12 Claude Code, 12 Codex), replay cut estimated input by 78% at 32,000 tokens and 61% at 128,000, with no tool call ever separated from its result. Three large sessions hold 476 million of the 665 million tokens, so the median session's cut at 32,000 is 46%, and at 128,000 the median Claude Code session is not cut at all.

![Line chart: estimated input cut across 24 recorded sessions. Tail 0 cuts 78% at 32K, 73% at 64K, 61% at 128K and 38% at 256K. Tail 40 is a little lower at every threshold.](/blog/introducing-gobstopper/gob-grid.png)

*Figure 10. Pooled cut in estimated input across 24 recorded sessions, by threshold.*
Estimates, not billed · 24 recorded sessions (12 Claude Code, 12 Codex), 665M tokens · main fdeb099 · September 26, 2026.

An earlier replay, on September 19, 2026, of the `compacted` file strategy over 729 archived sessions produced no plan for 637 of them and a 36.4% median projected cut for 73 high-context Codex tasks. The [benchmarks page](/benchmarks#terminal-bench-2026-09-28) has every study, its setup, and the aggregate data to download.

## Limits and what comes next

The roadmap describes Gobstopper as the context-compaction layer for the Hraness agent stack, a policy engine that runtimes such as xcb embed. Planned, not shipped: the three-trial Terminal-Bench rerun, a run with an Anthropic model, steering provider-native compaction rather than only triggering it, and compacting at natural breaks between tasks instead of at a flat token count.

Some core pieces are checked with more than tests. The archive's concurrency design has TLA+ models, each with deliberately broken variants that must fail; [How Gobstopper model-checks its archive against crashes](/blog/vault-models-that-fail-on-purpose) walks through them. The limits on an edit plan, such as at most 64 edits and at most one state card, are small Rust functions that the production code and Kani proofs share, and Lean proves list laws about transcripts; [What Kani and Lean prove about Gobstopper's compaction](/blog/proofs-for-the-admission-math) explains both. Each check covers one component at a stated scope. The TLA+ models check a finite design rather than the Rust code, Kani covers selected arithmetic, and the Lean laws are checked against the Rust code only on finite cases. None of them establishes whole-system correctness, behavior after a power loss, or that a provider will accept a copy.

The proxy cannot make an agent notice that something it needs was summarized away; it can only make the old middle cheaper to carry. A copy that passes Gobstopper's structural checks may still fail to resume in Claude Code or Codex, so test a copy before you rely on it. Released builds cannot ask a provider to compact a live session, and direct rewrites of provider files or stores are turned off.

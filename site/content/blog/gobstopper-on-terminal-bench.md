Reducing the context a coding agent sends can lower its input-token count while also removing something the next step needs. A useful evaluation measures both the requests and the completed tasks. Looking at token reduction alone misses that tradeoff.

Gobstopper’s Terminal-Bench study compares three setups for the same tasks: a proxy that keeps the last three turns verbatim, a proxy that keeps a larger recent tail, and no proxy. The result illustrates why token counts, task completion, and cost need separate interpretation.

## What Terminal-Bench showed

The study ran Terminal-Bench 2.1’s 89 tasks through Claude Code with GLM 5.3 Flash, one trial per task in each setup. The proxy used a 45,000-token threshold. Its `tail 0` setting kept the last three turns; `tail 40` also retained older whole turns within a larger budget.

The measured input was:

| Setup | Input tokens | Tasks solved |
| --- | ---: | ---: |
| Gobstopper, tail 0 | 84.3 million | 61 of 89 |
| No proxy | 118.6 million | 60 of 89 |
| Gobstopper, tail 40 | 118.5 million | 59 of 89 |

Tail 0 sent 29% fewer input tokens than no proxy. Almost all of the reduction came from cached context read again on later requests. New input and output were about equal between those setups.

![Input-token totals for the three setups, with cache reads forming most of each bar.](/blog/introducing-gobstopper/gob-tokens.png)

*Provider-reported input across 89 tasks, one trial per setup. The study used Gobstopper v0.7.2 and Claude Code 2.1.283 with GLM 5.3 Flash through Vercel AI Gateway on September 27–28, 2026. Its 45,000-token threshold differs from the 128,000 default; 21 of 89 tail-0 trials may have used an earlier build.*

The solved counts are too close to distinguish with this run. Their confidence intervals overlap, and 31 tasks changed outcome across the three setups. The data supports a lower observed input count for tail 0, rather than a claim that compaction improves task completion or preserves it in every workflow.

![Task-completion rates with overlapping confidence intervals for tail 0, tail 40, and no proxy.](/blog/introducing-gobstopper/gob-solved.png)

*Wilson 95% intervals for the same study. Each setup ran each task once.*

The [benchmark record](/benchmarks#terminal-bench-2026-09-28) keeps the complete setup, statistics, and downloadable aggregate data. The tasks ran as x86 images under emulation on one Mac, so these results describe that environment and model.

## A smaller token count is not the same as a smaller bill

For this model through this gateway, tail 0 cost $5.72 and no proxy cost $6.82. The estimated difference was about 16%, but its 95% interval ran from 32% lower to 2% higher. One run therefore did not establish a cost improvement over no proxy.

The price of cache reads changes how much a repeated token costs. GLM 5.3 Flash’s cache-read price in this study was a fifth of its new-input price. Another provider’s pricing can turn the same token reduction into a different dollar result. A subscription has a different billing model again.

When evaluating your own setup, keep new input, cache reads, cache writes, and output separate. Compare them with completed tasks and your provider’s prices, rather than multiplying every removed input token by the new-input rate.

## Why the recent tail matters

Keeping more history sounds like a conservative choice. It also makes each rewritten request larger. At the same threshold, tail 40 cost $7.97, or 39% more than tail 0 in provider-reported terms. That comparison’s interval excluded zero, but five tasks accounted for more than the entire net cost gap. The remaining tasks slightly offset it.

![Two rewritten requests: tail 40 retains a larger block of recent history than tail 0.](/blog/introducing-gobstopper/gob-tail.png)

*Tail 0 keeps the latest three turns. Tail 40 can keep additional older turns, leaving less space before the next threshold crossing.*

A larger tail can cause earlier repeat compaction and send more context between compactions. Offline replay is consistent with that explanation, but the live logs do not settle the cause: logging was incomplete for the tail-0 arm, and the setups encountered different host load.

Gobstopper uses tail 0 by default. `--keep-tail-percent` lets you retain more recent context when a task benefits from it. Evaluate that choice with the work you run, especially when later steps depend on exact earlier output.

## How it keeps the cache warm

A coding agent sends prior conversation content with each model request. If each step adds a similar amount, the accumulated input grows approximately with the square of the number of steps until compaction changes the history.

Gobstopper’s proxy keeps the opening instructions and recent turns verbatim, then builds a mechanical summary of the older middle. It reuses that rewritten prefix byte for byte between compactions, allowing the provider’s prompt cache to match it. At the next threshold crossing, it rebuilds from the original history the client sends.

![Request size rises steadily without compaction and follows a lower sawtooth pattern with Gobstopper.](/blog/introducing-gobstopper/gob-sawtooth.png)

*One recorded Claude Code session replayed across 383 requests at a 45,000-token threshold, with calibration enabled. Counts are four-characters-per-token estimates, rather than billed tokens; the replay uses the v0.7.2 request engine.*

The summary rule comes from CliffCompaction’s [paper](https://arxiv.org/abs/2609.26779). Gobstopper adds controls for carrying earlier context, retaining selected original tool results and images, and adapting to reported token usage. The [proxy reference](https://github.com/hraness/gobstopper/blob/main/docs/proxy.md) describes those controls; they are separate from the historical benchmark configuration.

## Try one session

After [installing Gobstopper](/docs#quick-start), start Claude Code through a temporary proxy:

```sh
gobstopper proxy run -- claude
```

To use the study’s threshold, run:

```sh
gobstopper proxy run --threshold 45000 -- claude
```

The default threshold is 128,000 estimated tokens, so shorter sessions may pass through without compaction. The proxy leaves session files unchanged. Inspect the counters when the session ends and check whether the agent completed the task and retained the details it needed.

For a saved session, Gobstopper can instead preview edits and write a separate smaller copy while archiving the original. That is a different workflow from rewriting live requests; the [saved-session reference](/docs#install--use) covers preview, search, and restore commands.

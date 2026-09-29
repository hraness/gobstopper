/**
 * The comparison rows shared by this page and the README section
 * "How Gobstopper compares with CliffCompaction". A test checks that the
 * README table carries the same cells, so edit both together.
 */
export const CLIFF_REPOSITORY = "https://github.com/nguyenvuthientrang/cliffcompaction";
export const CLIFF_PAPER = "https://arxiv.org/abs/2609.26779";
export const CLIFF_PYPI = "https://pypi.org/project/cliffcompaction/";
export const CLIFF_BLOG = "https://nguyenvuthientrang.github.io/cliffcompaction/";

export type ComparisonRow = Readonly<{ aspect: string; cliff: string; gobstopper: string }>;

export const comparisonRows: readonly ComparisonRow[] = [
  {
    aspect: "Where it runs",
    cliff: "A local HTTP proxy between the agent and the Anthropic or OpenAI API",
    gobstopper: "A local HTTP proxy between the agent and its model provider, plus a CLI over the session files Claude Code and Codex write",
  },
  {
    aspect: "Clients",
    cliff: "Any client of the Anthropic Messages, OpenAI Chat Completions, or OpenAI Responses API",
    gobstopper: "Any client of the same three dialects that accepts a custom provider address: Claude Code, Codex, opencode, Crush, Aider, Goose, and more",
  },
  {
    aspect: "What it changes",
    cliff: "Each outgoing request, transparently, while the session runs",
    gobstopper: "The proxy rewrites outgoing requests over the threshold; file commands publish a separate compacted copy and leave the source unchanged",
  },
  {
    aspect: "How it shrinks",
    cliff: "Drops tool results over 500 characters, signatures for tool calls, last three turns verbatim; never paraphrases",
    gobstopper: "The proxy applies the same summary rule and keeps the last three turns verbatim by default, and older whole turns within a tail budget if you set one; file strategies drop or stub stale tool results, and `structured` and `compacted` add a metadata state card; no built-in strategy paraphrases unless `GOBSTOPPER_DIGEST=apple` has an on-device model write the card",
  },
  {
    aspect: "Recompaction",
    cliff: "Rebuilt from the original history; the prior summary is discarded",
    gobstopper: "The proxy rebuilds from the original history, and each summary keeps the human's words and the assistant's visible replies from the turns earlier compactions summarized, up to 24,000 characters (since v0.6.0); `cliff` on a copy drops the same records as one pass over the source when both passes produce a plan; strategies that inject a state card carry it forward into the next copy",
  },
  {
    aspect: "What holds the originals",
    cliff: "The agent's own history and the files on disk; the proxy keeps only an in-memory cache of compacted prefixes",
    gobstopper: "For proxied requests, the agent's own transcript and an in-memory cache; for copies, a content-addressed vault with `search-snapshot` and `read-snapshot`",
  },
  {
    aspect: "Evidence published",
    cliff: "Terminal-Bench 2.0 and 2.1 (including a run through Claude Code), SWE-bench Verified, and KernelBench results in the paper, on Kimi, GLM, and GPT-5-mini models",
    gobstopper: "Offline replays of 729 archived sessions, replays of nine recorded sessions through the proxy, one dated afternoon of live proxy counters, literal retention probes, dated single-session trials, and one live Terminal-Bench 2.1 run (89 tasks, three arms, one trial each, September 27 and 28, 2026): resolution within noise of Claude Code alone, 29% fewer provider-reported input tokens at the default tail and a 45,000-token threshold",
  },
  {
    aspect: "Model needed",
    cliff: "None; the summary is mechanical",
    gobstopper: "None for the proxy or the built-in strategies; optional model scorers",
  },
] as const;

export type DepartureRow = Readonly<{ departure: string; byDefault: string; restore: string }>;

/** The seven ways `gobstopper proxy` departs from the reference engine. */
export const departureRows: readonly DepartureRow[] = [
  {
    departure: "Keeps older whole turns beyond the last three while they fit a tail budget",
    byDefault: "Off: tail 0 since v0.7.3 (40 before)",
    restore: "`--keep-tail-percent 0`",
  },
  {
    departure: "Counts a run of consecutive assistant messages as one turn in every dialect, not only Responses",
    byDefault: "On",
    restore: "None; a split would separate tool calls from their results",
  },
  {
    departure: "Uses a separate threshold for Anthropic requests that declare a 1M-token window",
    byDefault: "On: 256,000, or `--threshold` if higher",
    restore: "Set `--threshold-1m` equal to `--threshold`",
  },
  {
    departure: "Resends the original when the provider rejects a rewritten request for a reason other than length",
    byDefault: "On",
    restore: "None",
  },
  {
    departure: "Raises the threshold when the verbatim head alone approaches it",
    byDefault: "On",
    restore: "None",
  },
  {
    departure: "Carries the human's words and the assistant's visible replies from earlier summarized turns",
    byDefault: "On: up to 24,000 characters",
    restore: "`--carry-max-chars 0`",
  },
  {
    departure: "Divides the threshold by a learned ratio of provider-reported to estimated input tokens",
    byDefault: "On: bounded 1.0 to 2.0, after five responses",
    restore: "`--no-calibrate`",
  },
] as const;

export const comparisonQuestions = [
  {
    question: "Can I run CliffCompaction and Gobstopper together?",
    answer:
      "Run one proxy per client. Chaining CliffCompaction and `gobstopper proxy` would compact each other's output, and the two have not been tested together. Gobstopper's file commands work alongside either proxy because neither changes the session files.",
  },
  {
    question: "Does Gobstopper reproduce CliffCompaction's benchmark results?",
    answer:
      "No. The cost and Terminal-Bench figures on this page are the authors' measurements of their proxy. Gobstopper ran its own Terminal-Bench 2.1 comparison of `gobstopper proxy` against Claude Code with no proxy, on GLM 5.3 Flash at a 45,000-token threshold with one trial per arm: resolution was within noise and input tokens were 29% lower. It ran no CliffCompaction arm and no 45K native auto-compaction arm, so the two sets of figures are not a head-to-head.",
  },
  {
    question: "Where do the dropped tool results go?",
    answer:
      "Both proxies drop them from the request only; the agent can read the file or rerun the command, and Claude Code and Codex keep the full history in their own transcripts. For copies, Gobstopper stores the source and candidate bytes in a local vault, so `gobstopper search-snapshot` and `gobstopper read-snapshot` can return the archived record. Neither tool makes an agent notice that a fact is missing.",
  },
  {
    question: "Why does auto not pick cliff?",
    answer:
      "The default strategy for saved sessions compares file strategies by projected savings and preserved prefix against a floor. Cliff has no floor: its yield is whatever the size rule removes. Choose it with `--strategy cliff` or a preset, so you make that trade yourself. For running sessions, use `gobstopper proxy`.",
  },
] as const;

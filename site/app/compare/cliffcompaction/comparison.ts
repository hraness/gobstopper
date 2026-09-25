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
    gobstopper: "A CLI over the session files Claude Code, Codex, and Devin write",
  },
  {
    aspect: "What it changes",
    cliff: "Each outgoing request, transparently, while the session runs",
    gobstopper: "A separate compacted copy of a session you then resume; the source file is unchanged",
  },
  {
    aspect: "How it shrinks",
    cliff: "Drops tool results over 500 characters, signatures for tool calls, last three turns verbatim; never paraphrases",
    gobstopper: "Strategies that drop or stub stale tool results by rule; `structured` and `compacted` add a metadata state card; no built-in strategy paraphrases unless `GOBSTOPPER_DIGEST=apple` has an on-device model write the card",
  },
  {
    aspect: "Recompaction",
    cliff: "Rebuilt from the original history; the prior summary is discarded",
    gobstopper: "`cliff` on a copy drops the same records as one pass over the source when both passes produce a plan; strategies that inject a state card carry it forward into the next copy",
  },
  {
    aspect: "What holds the originals",
    cliff: "The agent's own history and the files on disk; the proxy keeps only an in-memory cache of compacted prefixes",
    gobstopper: "A content-addressed vault with `search-snapshot` and `read-snapshot` for the exact archived record",
  },
  {
    aspect: "Evidence published",
    cliff: "Terminal-Bench 2.0, SWE-bench Verified, and KernelBench results in the paper, on Kimi, GLM, and GPT-5-mini models",
    gobstopper: "Offline replays of 729 archived sessions, literal retention probes, and dated single-session trials; no task-success or billing claims",
  },
  {
    aspect: "Model needed",
    cliff: "None; the summary is mechanical",
    gobstopper: "None for built-in strategies; optional model scorers",
  },
] as const;

export const comparisonQuestions = [
  {
    question: "Can I run CliffCompaction and Gobstopper together?",
    answer:
      "They work at different layers, so nothing prevents pointing an agent at the proxy and inspecting or copying its session files with Gobstopper. The two tools have not been tested together, and Gobstopper does not proxy API requests. A Gobstopper copy resumed under the proxy would be compacted again by the proxy's own rule.",
  },
  {
    question: "Does the cliff strategy reproduce CliffCompaction's benchmark results?",
    answer:
      "No. The cost and Terminal-Bench figures are the authors' measurements of their proxy on the Kimi and GLM models they tested. Gobstopper's cliff strategy applies the same drop rule to a transcript copy, but Gobstopper has not run those benchmarks, and a smaller copy is not evidence of a lower bill or a successful continuation.",
  },
  {
    question: "Where do the dropped tool results go?",
    answer:
      "CliffCompaction drops them from the request; the agent can read the file or rerun the command. Gobstopper stores the exact source and candidate bytes in a local vault before it writes a copy, so `gobstopper search-snapshot` and `gobstopper read-snapshot` can return the archived record. Neither tool makes an agent notice that a fact is missing.",
  },
  {
    question: "Why does auto not pick cliff?",
    answer:
      "The default strategy compares file strategies by projected savings and preserved prefix against a floor. Cliff has no floor: its yield is whatever the size rule removes. Choose it with `--strategy cliff` or a preset so the trade is explicit.",
  },
] as const;

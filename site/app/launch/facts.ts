import type { LaunchFact, LaunchStatus } from "@hraness/design-kit/launch";

import { arm } from "../_lib/gobbench-data";
import { F } from "../_lib/gobbench-format";
import { publishedRelease } from "../publication";

if (publishedRelease === null) throw new Error("The launch post names the latest release; site/published-release.json has none.");

/** The status label, from the release record. */
export const LAUNCH_STATUS: LaunchStatus = `Latest release: v${publishedRelease.version}` as LaunchStatus;

/**
 * Every number the launch post, the social kit, and the mockup captions use.
 * Benchmark values come from the formatter the homepage and film read, so no
 * page can round differently. tests/launch.test.ts pins the rest to source.
 */
export const launchFacts = {
  status: {
    value: LAUNCH_STATUS,
    source: "site/published-release.json, the latest published tag and the CI run that verified it",
  },
  threshold: {
    value: "128,000",
    source: "crates/gobstopper-adapters/src/request/mod.rs DEFAULT_THRESHOLD_TOKENS",
  },
  keepRecent: {
    value: "three",
    source: "crates/gobstopper-adapters/src/request/mod.rs, the default keep_recent of 3",
  },
  stepNow: {
    value: "40",
    source: "Worked example from the Gobstopper introduction: request 40 of a session",
  },
  stepsBefore: {
    value: "39",
    source: "Worked example: request 40 resends the 39 steps before it",
  },
  benchTasks: {
    value: String(arm("tail0").n_tasks),
    source: "site/public/benchmarks/2026-09-28/terminal-bench-results.json arms[].n_tasks",
  },
  benchSolvedWith: {
    value: F.solved.tail0,
    source: "terminal-bench-results.json, Gobstopper, tail 0 resolved",
  },
  benchSolvedWithout: {
    value: F.solved.no_proxy,
    source: "terminal-bench-results.json, Claude Code, no proxy resolved",
  },
  benchInputFewer: {
    value: F.inputFewer,
    source: "terminal-bench-results.json pairs tail0 vs no_proxy token_ratio_minus1.total_input",
  },
  benchThreshold: {
    value: "45,000",
    source: "STYLE.md: the Terminal-Bench run used a 45,000-token threshold; sawtooth-series.json run threshold 45000",
  },
} as const satisfies Record<string, LaunchFact>;

export type LaunchFactKey = keyof typeof launchFacts;

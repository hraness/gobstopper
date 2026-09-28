// The September 28, 2026 launch data: Terminal-Bench 2.1 through Claude Code, the
// 24-session replay grid and one real per-request sawtooth series. The three files
// are byte copies of the privacy-checked data package and are also the public
// downloads on /benchmarks. Nothing else supplies launch numbers.
import tb from "../../public/benchmarks/2026-09-28/terminal-bench-results.json";
import grid from "../../public/benchmarks/2026-09-28/replay-grid.json";
import saw from "../../public/benchmarks/2026-09-28/sawtooth-series.json";

export const terminalBench = tb;
export const replayGrid = grid;
export const sawtooth = saw;

export type ArmId = "tail0" | "tail40" | "no_proxy";

/** Display order for every chart and table: the new default, the baseline, the old default. */
export const ARM_ORDER: readonly ArmId[] = ["tail0", "no_proxy", "tail40"];

export const ARM_LABEL: Record<ArmId, string> = {
  tail0: "Gobstopper, tail 0",
  tail40: "Gobstopper, tail 40 (old default)",
  no_proxy: "Claude Code, no proxy",
};

export const ARM_VAR: Record<ArmId, string> = {
  tail0: "var(--gob-arm-tail0)",
  tail40: "var(--gob-arm-tail40)",
  no_proxy: "var(--gob-arm-noproxy)",
};

export type Arm = (typeof terminalBench.arms)[number];
export type Pair = (typeof terminalBench.pairs)[number];

export function arm(id: ArmId): Arm {
  const found = terminalBench.arms.find((entry) => entry.arm === id);
  if (found === undefined) throw new Error(`terminal-bench-results.json has no arm ${id}.`);
  return found;
}

/** The paired comparison stored as (a, b); ratios are a over b. */
export function pair(a: ArmId, b: ArmId): Pair {
  const found = terminalBench.pairs.find((entry) => entry.a === a && entry.b === b);
  if (found === undefined) throw new Error(`terminal-bench-results.json has no pair ${a}/${b}.`);
  return found;
}

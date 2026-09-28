// One formatter for every launch number. The site, the stills and the film all read
// their display strings from F, so no surface can round differently. Pure TypeScript:
// media/render.ts bundles this file, so it imports nothing but the data module.
import { arm, pair, replayGrid, sawtooth, terminalBench, type ArmId } from "./gobbench-data";

const MINUS = "−";
const EN_DASH = "–";

/** Wilson score interval for k successes in n trials, as fractions. */
export function wilson(k: number, n: number, z = 1.959963984540054): [number, number] {
  if (!(n > 0) || k < 0 || k > n) throw new RangeError(`wilson(${k}, ${n}) needs 0 <= k <= n and n > 0.`);
  const p = k / n;
  const z2 = z * z;
  const denominator = 1 + z2 / n;
  const centre = (p + z2 / (2 * n)) / denominator;
  const half = (z * Math.sqrt((p * (1 - p)) / n + z2 / (4 * n * n))) / denominator;
  return [Math.max(0, centre - half), Math.min(1, centre + half)];
}

/** 0.6854 -> "68.5%". */
export function pct(x: number, digits = 1): string {
  return `${(x * 100).toFixed(digits)}%`;
}

/** Fractions -> "58.3–77.2" (percent points, en dash, no % sign). */
export function range(lo: number, hi: number): string {
  return `${(lo * 100).toFixed(1)}${EN_DASH}${(hi * 100).toFixed(1)}`;
}

/** 84305420 -> "84.3M". */
export function millions(tokens: number, digits = 1): string {
  return `${(tokens / 1_000_000).toFixed(digits)}M`;
}

/** 595109 -> "595K". */
export function thousands(tokens: number): string {
  return `${(tokens / 1000).toFixed(0)}K`;
}

/** -0.2889 -> "29%", the rounded magnitude for "29% fewer". */
export function fewer(ratioMinus1: number): string {
  return `${Math.abs(ratioMinus1 * 100).toFixed(0)}%`;
}

/** +0.393 -> "+39%", -0.161 -> "−16%" (U+2212). A value that rounds to zero has no sign. */
export function signed(ratioMinus1: number, digits = 0): string {
  const magnitude = Math.abs(ratioMinus1 * 100).toFixed(digits);
  if (Number(magnitude) === 0) return `${magnitude}%`;
  return `${ratioMinus1 > 0 ? "+" : MINUS}${magnitude}%`;
}

/** 5.7204, 2 -> "$5.72"; -0.1235, 2 -> "−$0.12". */
export function usd(x: number, digits: number): string {
  const magnitude = Math.abs(x).toFixed(digits);
  return `${x < 0 && Number(magnitude) !== 0 ? MINUS : ""}$${magnitude}`;
}

type PerArm = Readonly<Record<ArmId, string>>;

function perArm(value: (id: ArmId) => string): PerArm {
  return { tail0: value("tail0"), no_proxy: value("no_proxy"), tail40: value("tail40") };
}

function interval(bounds: readonly number[], digits: number): string {
  const [lo, hi] = bounds;
  if (lo === undefined || hi === undefined) throw new Error("A 95% interval needs two bounds.");
  return `${signed(lo, digits)} to ${signed(hi, digits)}`;
}

function replayCut(threshold: number): string {
  const row = replayGrid.pooled.find((entry) => entry.threshold === threshold && entry.keep_tail_percent === 0);
  if (row === undefined) throw new Error(`replay-grid.json has no pooled tail 0 row at ${threshold}.`);
  return pct(row.pooled_input_cut, 0);
}

const sawRun = sawtooth.runs.find((run) => run.threshold === 45000 && run.keep_tail_percent === 0);
if (sawRun === undefined) throw new Error("sawtooth-series.json has no 45,000-token tail 0 run.");
const sawWithProxy = sawRun.cumulative_est_input_tokens[1];
if (sawWithProxy === undefined) throw new Error("The sawtooth run needs its cumulative total with the proxy.");

const cost = (id: ArmId): number => arm(id).cost_usd.total;
const gap = terminalBench.cost_concentration.find((entry) => entry.minuend === "tail40" && entry.subtrahend === "tail0");
if (gap === undefined) throw new Error("terminal-bench-results.json has no tail 40 minus tail 0 cost concentration.");
/**
 * The C-gap rows: extra cost of tail 40 over tail 0 for the five costliest tasks, then
 * the other tasks together (the total gap minus the five). Shared by the site chart
 * and the blog still.
 */
export type GapRow = { readonly task: string; readonly deltaUsd: number };
const topGap = gap.top_tasks.map((entry) => ({ task: entry.task, deltaUsd: entry.tail40 - entry.tail0 }));
const topGapSum = topGap.reduce((sum, row) => sum + row.deltaUsd, 0);
export const GAP_ROWS: readonly GapRow[] = [
  ...topGap,
  { task: `other ${arm("tail0").n_tasks - topGap.length} tasks`, deltaUsd: gap.total_gap_usd - topGapSum },
];

const [largestFrom, largestTo] = replayGrid.largest_session_at_32k_tail0.largest_request_est_tokens;
if (largestFrom === undefined || largestTo === undefined) throw new Error("The largest replayed session needs both sizes.");

export const F = {
  solved: perArm((id) => String(arm(id).resolved)),
  rate: perArm((id) => pct(arm(id).resolved / arm(id).n_tasks)),
  ci: perArm((id) => range(...wilson(arm(id).resolved, arm(id).n_tasks))),
  input: perArm((id) => millions(arm(id).tokens_all_trials.total_input)),
  cache: perArm((id) => millions(arm(id).tokens_all_trials.cache_read)),
  uncached: perArm((id) => millions(arm(id).tokens_all_trials.uncached_input)),
  output: perArm((id) => millions(arm(id).tokens_all_trials.output, 2)),
  cost: perArm((id) => usd(cost(id), 2)),
  perTask: perArm((id) => usd(arm(id).cost_usd.per_task_mean, 3)),
  inputFewer: fewer(pair("tail0", "no_proxy").token_ratio_minus1.total_input),
  cacheFewer: fewer(pair("tail0", "no_proxy").token_ratio_minus1.cache_read),
  costLower: fewer(pair("tail0", "no_proxy").total_cost_ratio_minus1),
  costLowerCI: interval(pair("tail0", "no_proxy").total_cost_ratio_minus1_boot95, 0),
  oldVsNew: signed(cost("tail40") / cost("tail0") - 1),
  oldVsNone: signed(cost("tail40") / cost("no_proxy") - 1),
  // The interval is stored for tail 0 over tail 40, so it is quoted beside newVsOld (the
  // same direction); oldVsNew is the same comparison turned round and has no interval here.
  newVsOld: signed(pair("tail0", "tail40").total_cost_ratio_minus1),
  oldVsNewCI: interval(pair("tail0", "tail40").total_cost_ratio_minus1_boot95, 1),
  // Unsigned magnitudes for prose such as "cost 39% more" or "cost 28% less", where
  // the word carries the direction and a sign would read twice.
  oldVsNewAbs: fewer(cost("tail40") / cost("tail0") - 1),
  oldVsNoneAbs: fewer(cost("tail40") / cost("no_proxy") - 1),
  newVsOldAbs: fewer(pair("tail0", "tail40").total_cost_ratio_minus1),
  churn: {
    all: String(terminalBench.churn.resolved_by_all),
    none: String(terminalBench.churn.resolved_by_none),
    split: String(terminalBench.churn.split),
  },
  gapTotal: usd(gap.total_gap_usd, 2),
  // The package's top_share_of_gap is 1.055, already rounded at the half; the unrounded
  // share from the same rows is 1.0549, so it is recomputed here (tests check they agree).
  gapTopShare: pct(topGapSum / gap.total_gap_usd, 0),
  replay: { t32: replayCut(32000), t64: replayCut(64000), t128: replayCut(128000), t256: replayCut(256000) },
  replayLargest: { from: thousands(largestFrom), to: thousands(largestTo) },
  saw: {
    requests: String(sawtooth.without_proxy.length),
    peak: thousands(Math.max(...sawtooth.without_proxy)),
    cumFrom: millions(sawtooth.without_proxy.reduce((sum, tokens) => sum + tokens, 0)),
    cumTo: millions(sawWithProxy),
    cut: pct(sawRun.cumulative_cut, 0),
    compactions: String(sawRun.compactions),
  },
} as const;

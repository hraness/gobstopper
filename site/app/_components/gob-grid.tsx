import { replayGrid } from "../_lib/gobbench-data";
import { pct } from "../_lib/gobbench-format";
import type { Tick } from "./gob-axis";
import { GobFigure, GobLegend, GobTable, P_GRID, at } from "./gob-figure";
import { GobPlot } from "./gob-plot";

const THRESHOLDS = [32000, 64000, 128000, 256000] as const;
const TAILS = [0, 25, 40] as const;
const Y_TICKS: readonly Tick[] = [0, 25, 50, 75, 100].map((value) => ({ value, label: `${value}%` }));

function cut(threshold: number, tail: number): number {
  const row = replayGrid.pooled.find((entry) => entry.threshold === threshold && entry.keep_tail_percent === tail);
  if (row === undefined) throw new Error(`replay-grid.json has no pooled row at ${threshold}, tail ${tail}.`);
  return row.pooled_input_cut;
}

const xAt = (index: number): number => 0.06 + (index / (THRESHOLDS.length - 1)) * 0.88;
const X_TICKS = THRESHOLDS.map((threshold, index) => ({ value: threshold, label: `${threshold / 1000}K`, at: xAt(index) }));

function path(tail: number): string {
  return THRESHOLDS.map((threshold, index) => {
    const x = Math.round(xAt(index) * 10000) / 10;
    const y = Math.round((1 - cut(threshold, tail)) * 10000) / 10;
    return `${index === 0 ? "M" : "L"}${x},${y}`;
  }).join(" ");
}

const LINES = [
  { tail: 0, arm: "tail0", label: "tail 0" },
  { tail: 40, arm: "tail40", label: "tail 40" },
] as const;

/** C-grid: pooled estimated-input cut across 24 sessions, by threshold, tail 0 vs tail 40. */
export function GobGrid() {
  const cuts0 = THRESHOLDS.map((threshold) => pct(cut(threshold, 0), 0));
  const unpaired = replayGrid.pairing_violations_total.proxy_introduced;
  const replays = replayGrid.pairing_violations_total.cells;
  return (
    <GobFigure
      alt={`Line chart: estimated input cut across ${replayGrid.corpus.sessions} recorded sessions. Tail 0 cuts ${cuts0[0]} at 32K, ${cuts0[1]} at 64K, ${cuts0[2]} at 128K and ${cuts0[3]} at 256K. Tail 40 is a little lower at every threshold.`}
      caption={`Pooled cut in estimated input across ${replayGrid.corpus.sessions} recorded sessions, by threshold.`}
      id="grid"
      kind="chart"
      legend={
        <GobLegend
          items={[
            { label: "Gobstopper, tail 0", swatch: "marker", arm: "tail0" },
            { label: "Gobstopper, tail 40", swatch: "marker", arm: "tail40" },
          ]}
        />
      }
      notes={
        <p className="gob-figure__note">
          {unpaired} unpaired tool calls in {replays} replays. Three large sessions hold 476M of the 665M tokens, so a typical
          session&apos;s cut at 32K is about 46%. At 128K most Claude Code sessions never cross the threshold.
        </p>
      }
      provenance={P_GRID}
      table={
        <GobTable
          caption="Pooled cut in estimated input by threshold and tail"
          head={["Threshold", ...TAILS.map((tail) => `Tail ${tail}`)]}
          rows={THRESHOLDS.map((threshold) => [`${threshold / 1000}K`, ...TAILS.map((tail) => pct(cut(threshold, tail), 1))])}
        />
      }
      title="Lower thresholds cut more"
    >
      <GobPlot
        overlay={
          <>
            {/* Tail 40 first, so tail 0 (haloed) paints on top where the two nearly meet. */}
            {[...LINES].reverse().flatMap((line) => THRESHOLDS.map((threshold, index) => (
              <span
                aria-hidden="true"
                className="gob-marker gob-plot__point"
                data-arm={line.arm}
                key={`${line.arm}-${threshold}`}
                style={{ left: at(xAt(index)), bottom: at(cut(threshold, line.tail)) }}
              />
            )))}
            {LINES.flatMap((line) => THRESHOLDS.map((threshold, index) => (
              <span
                aria-hidden="true"
                className="gob-plot__value"
                data-arm={line.arm}
                key={`v-${line.arm}-${threshold}`}
                // Tail 0 is labelled above its marker, tail 40 below its own.
                style={{ left: at(xAt(index)), bottom: `calc(${at(cut(threshold, line.tail))} ${line.tail === 0 ? "+" : "-"} 0.6rem)` }}
              >
                {pct(cut(threshold, line.tail), 0)}
              </span>
            )))}
          </>
        }
        xTicks={X_TICKS}
        yMax={100}
        yTicks={Y_TICKS}
      >
        {LINES.map((line) => <path className="gob-line" data-arm={line.arm} d={path(line.tail)} key={line.arm} />)}
      </GobPlot>
      <p className="gob-plot__xlabel" aria-hidden="true">threshold, estimated tokens · cut in estimated input</p>
    </GobFigure>
  );
}

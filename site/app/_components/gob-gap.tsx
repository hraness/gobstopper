import type { CSSProperties } from "react";

import { F, GAP_ROWS } from "../_lib/gobbench-format";
import { GobRules, GobXAxis, type Tick } from "./gob-axis";
import { GobFigure, GobTable, P_TB_FULL, at } from "./gob-figure";

// Half a dollar of room left of zero, so the one saving is labelled on its own side.
const DOMAIN = [-0.5, 1.5] as const;
const TICKS: readonly Tick[] = [
  { value: 0, label: "$0" },
  { value: 0.5, label: "+$0.50" },
  { value: 1, label: "+$1.00" },
  { value: 1.5, label: "+$1.50", show: "wide" },
];

function money(delta: number): string {
  const rounded = Math.round(delta * 100) / 100;
  return `${rounded < 0 ? "−" : "+"}$${Math.abs(rounded).toFixed(2)}`;
}

function spokenMoney(delta: number, first: boolean): string {
  const rounded = Math.abs(Math.round(delta * 100) / 100).toFixed(2);
  return `${delta < 0 ? "minus" : "plus"} ${rounded}${first ? " dollars" : ""}`;
}

const place = (value: number): number => (value - DOMAIN[0]) / (DOMAIN[1] - DOMAIN[0]);
const ZERO = place(0);

const ALT = `Bar chart of extra cost for tail 40 over tail 0 per task: ${GAP_ROWS.map((row, index) => {
  const last = index === GAP_ROWS.length - 1;
  return `${last ? "and the " : ""}${row.task}${last ? " together" : ""} ${spokenMoney(row.deltaUsd, index === 0)}`;
}).join(", ")}.`;

/** C-gap: diverging bars, tail 40 minus tail 0 per task, provider-reported dollars. */
export function GobGap() {
  return (
    <GobFigure
      alt={ALT}
      caption={`Provider-reported cost difference, tail 40 minus tail 0, per task. Five of 89 tasks account for ${F.gapTopShare} of the ${F.gapTotal} gap; the other 84 net slightly negative. This is why the one significant cost result should not be over-read.`}
      id="gap"
      kind="chart"
      provenance={P_TB_FULL}
      table={
        <GobTable
          caption="Extra provider-reported cost of tail 40 over tail 0, per task"
          head={["Task", "Tail 40 minus tail 0"]}
          rows={GAP_ROWS.map((row) => [row.task, money(row.deltaUsd)])}
        />
      }
      title="Five tasks carried the old default's extra cost"
    >
      <div className="gob-gap">
        {GAP_ROWS.map((row) => {
          const negative = row.deltaUsd < 0;
          const end = place(row.deltaUsd);
          return (
            <div className="gob-gap-row" data-sign={negative ? "minus" : "plus"} key={row.task}>
              <p className="gob-gap-row__label">{row.task}</p>
              <div className="gob-gap-row__track">
                <GobRules domain={DOMAIN} ticks={TICKS} />
                <span
                  className="gob-gap-row__bar"
                  style={negative
                    ? { left: at(end), width: at(ZERO - end) }
                    : { left: at(ZERO), width: at(end - ZERO) }}
                />
                <span
                  className="gob-gap-row__value"
                  data-side={negative ? "start" : undefined}
                  // A saving is labelled left of its bar; on narrow tracks the room left of zero
                  // is too small, so it sits right of the zero line instead (see globals.css).
                  style={negative
                    ? { "--gob-zero": at(ZERO), "--gob-end": at(end) } as CSSProperties
                    : { left: `calc(${at(end)} + 0.4rem)` }}
                >
                  {money(row.deltaUsd)}
                </span>
              </div>
            </div>
          );
        })}
        <div className="gob-gap-row gob-gap-row--axis">
          <span className="gob-gap-row__label" />
          <GobXAxis domain={DOMAIN} label="provider-reported dollars, tail 40 minus tail 0" ticks={TICKS} />
        </div>
      </div>
    </GobFigure>
  );
}

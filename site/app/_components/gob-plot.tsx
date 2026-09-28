import type { ReactNode } from "react";

import { at } from "./gob-figure";
import type { Tick } from "./gob-axis";

/**
 * A line-chart frame: y tick labels in a gutter, faint horizontal rules, a 1000×1000 SVG
 * stretched over the plot area (strokes stay crisp with non-scaling-stroke), and HTML
 * overlays positioned in percent so text never scales with the drawing.
 */
export function GobPlot({
  children, overlay, xTicks, yMax, yTicks,
}: {
  readonly children: ReactNode;
  readonly overlay?: ReactNode;
  readonly xTicks: readonly (Tick & { readonly at: number })[];
  readonly yMax: number;
  readonly yTicks: readonly Tick[];
}) {
  return (
    <div className="gob-plot">
      <div aria-hidden="true" className="gob-plot__y">
        {yTicks.map((tick) => (
          <span className="gob-plot__ytick" data-show={tick.show} key={`${tick.label}-${tick.show ?? "all"}`} style={{ bottom: at(tick.value / yMax) }}>
            {tick.label}
          </span>
        ))}
      </div>
      <div className="gob-plot__area">
        {yTicks.map((tick) => (
          <span
            aria-hidden="true"
            className="gob-plot__rule"
            data-show={tick.show}
            data-zero={tick.value === 0 ? "" : undefined}
            key={`rule-${tick.label}-${tick.show ?? "all"}`}
            style={{ bottom: at(tick.value / yMax) }}
          />
        ))}
        <svg aria-hidden="true" className="gob-plot__svg" focusable="false" preserveAspectRatio="none" viewBox="0 0 1000 1000">
          {children}
        </svg>
        {overlay}
      </div>
      <span aria-hidden="true" />
      <div aria-hidden="true" className="gob-plot__x">
        {xTicks.map((tick) => (
          <span
            className="gob-tick"
            data-edge={tick.at <= 0.001 ? "start" : tick.at >= 0.999 ? "end" : undefined}
            data-show={tick.show}
            key={tick.label}
            style={{ left: at(tick.at) }}
          >
            {tick.label}
          </span>
        ))}
      </div>
    </div>
  );
}

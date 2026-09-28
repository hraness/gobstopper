import { at } from "./gob-figure";

/** A tick: its value on the scale, its label, and the widths it shows at. */
export type Tick = { readonly value: number; readonly label: string; readonly show?: "wide" | "narrow" };

/** Faint vertical rules at each tick, drawn inside a bar track (HTML, not SVG). */
export function GobRules({ domain, ticks }: { readonly domain: readonly [number, number]; readonly ticks: readonly Tick[] }) {
  const [lo, hi] = domain;
  return (
    <span aria-hidden="true" className="gob-rules">
      {ticks.map((tick) => (
        <span
          className="gob-rules__rule"
          data-show={tick.show}
          data-zero={tick.value === 0 ? "" : undefined}
          key={`${tick.label}-${tick.show ?? "all"}`}
          style={{ left: at((tick.value - lo) / (hi - lo)) }}
        />
      ))}
    </span>
  );
}

/** The tick labels under a horizontal scale, centred on their rules. */
export function GobXAxis({
  domain, label, ticks,
}: {
  readonly domain: readonly [number, number];
  readonly label?: string;
  readonly ticks: readonly Tick[];
}) {
  const [lo, hi] = domain;
  return (
    <div aria-hidden="true" className="gob-xaxis">
      <div className="gob-xaxis__ticks">
        {ticks.map((tick) => {
          const position = (tick.value - lo) / (hi - lo);
          return (
            <span
              className="gob-tick"
              data-edge={position <= 0.001 ? "start" : position >= 0.999 ? "end" : undefined}
              data-show={tick.show}
              key={`${tick.label}-${tick.show ?? "all"}`}
              style={{ left: at(position) }}
            >
              {tick.label}
            </span>
          );
        })}
      </div>
      {label === undefined ? null : <p className="gob-xaxis__label">{label}</p>}
    </div>
  );
}

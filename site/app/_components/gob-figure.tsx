import type { CSSProperties, ReactNode } from "react";

import { ARM_LABEL, type ArmId } from "../_lib/gobbench-data";

/** The provenance lines every launch figure prints (SPEC §3). */
export const P_TB =
  "Terminal-Bench 2.1 · 89 tasks · one trial per arm · Claude Code 2.1.283 with GLM 5.3 Flash via Vercel AI Gateway · Gobstopper v0.7.2, 45,000-token threshold (default 128,000) · September 27–28, 2026";
export const P_TB_FULL = `${P_TB} · 21 of 89 tail-0 trials may have run an earlier build`;
export const P_SAW =
  "Estimated tokens (about 4 characters per token, plus 20,000 assumed), not billed · one recorded Claude Code session, 383 requests · replay, build f4db57e (the v0.7.2 request engine), calibration on";
export const P_GRID =
  "Estimates, not billed · 24 recorded sessions (12 Claude Code, 12 Codex), 665M tokens · main fdeb099 · September 26, 2026";

/** "84.3M" -> "84.3 million", "491K" -> "491,000" for text alternatives. */
export function spoken(value: string): string {
  const match = /^([\d.]+)([MK])$/u.exec(value);
  if (match === null) return value;
  const [, number, unit] = match;
  if (unit === "M") return `${number} million`;
  return (Number(number) * 1000).toLocaleString("en-US");
}

/** Percent position for inline styles; figures position marks with percentages only. */
export function at(fraction: number): string {
  const clamped = Math.min(1, Math.max(0, fraction));
  return `${Math.round(clamped * 10000) / 100}%`;
}

export type GobFigureProps = {
  readonly id: string;
  readonly title: string;
  /** Hidden when the surrounding section heading already says it. */
  readonly titleHidden?: boolean;
  readonly hero?: string;
  readonly subtitle?: ReactNode;
  readonly legend?: ReactNode;
  readonly alt: string;
  readonly caption: ReactNode;
  readonly provenance?: string;
  /** Text shown under the plot, before the caption (notes, annotations as a list). */
  readonly notes?: ReactNode;
  readonly aside?: ReactNode;
  readonly table?: ReactNode;
  readonly variant?: string;
  readonly kind: "chart" | "diagram";
  readonly children: ReactNode;
};

/**
 * The frame every launch figure shares: a visible title, the plot as one image with a
 * written alternative, a caption with provenance, and the numbers in a table behind
 * "Show the numbers". Server-rendered; no client script.
 */
export function GobFigure({
  alt, aside, caption, children, hero, id, kind, legend, notes, provenance, subtitle, table, title, titleHidden, variant,
}: GobFigureProps) {
  const altId = `fig-${id}-alt`;
  return (
    <figure className="gob-figure" data-kind={kind} data-variant={variant} id={`fig-${id}`}>
      <header className="gob-figure__head">
        {hero === undefined ? null : <p className="gob-figure__hero">{hero}</p>}
        <p className={titleHidden === true ? "gob-figure__title gob-visually-hidden" : "gob-figure__title"}>{title}</p>
        {subtitle === undefined ? null : <p className="gob-figure__subtitle">{subtitle}</p>}
      </header>
      {legend}
      <div aria-labelledby={altId} className="gob-figure__plot" role="img">
        {children}
      </div>
      {/* Read once, through aria-labelledby; hidden from the reading order so it is not read twice. */}
      <p aria-hidden="true" className="gob-visually-hidden" id={altId}>{alt}</p>
      {notes}
      {aside}
      <figcaption className="gob-figure__caption">
        {caption}
        {provenance === undefined ? null : <span className="gob-figure__prov">{provenance}</span>}
      </figcaption>
      {table === undefined ? null : (
        <details className="gob-figure__table">
          <summary>Show the numbers</summary>
          <div className="gob-table-scroll">{table}</div>
        </details>
      )}
    </figure>
  );
}

/** A plain data table for "Show the numbers"; never the marketing data table. */
export function GobTable({
  caption, head, rows,
}: {
  readonly caption: string;
  readonly head: readonly string[];
  readonly rows: readonly (readonly ReactNode[])[];
}) {
  return (
    <table className="gob-table">
      <caption>{caption}</caption>
      <thead>
        <tr>{head.map((cell, index) => <th key={index} scope="col">{cell}</th>)}</tr>
      </thead>
      <tbody>
        {rows.map((row, rowIndex) => (
          <tr key={rowIndex}>
            {row.map((cell, index) => (index === 0
              ? <th key={index} scope="row">{cell}</th>
              : <td key={index}>{cell}</td>))}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** The arm's secondary marker: circle, square or diamond (SPEC §1.1). */
export function GobMarker({ arm, style }: { readonly arm: ArmId; readonly style?: CSSProperties }) {
  return <span aria-hidden="true" className="gob-marker" data-arm={arm} style={style} />;
}

export type LegendItem = {
  readonly label: string;
  readonly swatch: "fill" | "pale" | "line" | "marker" | "step" | "slab";
  readonly arm?: ArmId;
  readonly kind?: string;
};

export function GobLegend({ items }: { readonly items: readonly LegendItem[] }) {
  return (
    <ul className="gob-legend">
      {items.map((item) => (
        <li key={item.label}>
          {item.swatch === "marker" && item.arm !== undefined ? (
            <GobMarker arm={item.arm} />
          ) : (
            <span aria-hidden="true" className="gob-swatch" data-arm={item.arm} data-kind={item.kind} data-swatch={item.swatch} />
          )}
          {item.label}
        </li>
      ))}
    </ul>
  );
}

/** An arm name with its marker, exactly as ARM_LABEL spells it. */
export function GobArmName({ arm }: { readonly arm: ArmId }) {
  return (
    <span className="gob-arm-name">
      <GobMarker arm={arm} />
      <span>{ARM_LABEL[arm]}</span>
    </span>
  );
}

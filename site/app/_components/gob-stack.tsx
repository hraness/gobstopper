import type { ReactNode } from "react";

import { SLAB, stackHeight, type Slab, type SlabKind } from "../_lib/gob-geometry";
import { GobRing } from "./gob-ring";

/** A run of equal slabs: a label on its first slab names the whole run. */
export type SlabRun = { readonly kind: SlabKind; readonly count: number; readonly label?: string };

export function runs(slabs: readonly Slab[]): SlabRun[] {
  const out: SlabRun[] = [];
  for (const slab of slabs) {
    const last = out.at(-1);
    if (last !== undefined && last.kind === slab.kind && slab.label === undefined) {
      out[out.length - 1] = { ...last, count: last.count + 1 };
    } else {
      out.push(slab.label === undefined ? { kind: slab.kind, count: 1 } : { kind: slab.kind, count: 1, label: slab.label });
    }
  }
  return out;
}

/** CSS length for n slab units; --gob-u is one slab's height. */
export function units(n: number): string {
  return `calc(var(--gob-u) * ${Math.round(n * 1000) / 1000})`;
}

/** Where a lid sits: `lidAt` counted in slab pitches (slab plus gap) from the floor. */
export function lidHeight(lidAt: number): number {
  return lidAt * (SLAB.h + SLAB.gap);
}

function Run({ run, labels }: { readonly run: SlabRun; readonly labels: boolean }) {
  return (
    <div className="gob-run" data-kind={run.kind}>
      <div className="gob-run__slabs">
        {Array.from({ length: run.count }, (_, index) => <span className="gob-slab" data-kind={run.kind} key={index} />)}
      </div>
      {!labels || run.label === undefined ? null : (
        <span className="gob-run__label">
          {run.kind === "summary" ? <GobRing /> : null}
          <span>{run.label}</span>
        </span>
      )}
    </div>
  );
}

/** A stack of slabs, floor first (drawn bottom up), with labels beside the runs they name. */
export function GobStack({ labels = true, slabs }: { readonly labels?: boolean; readonly slabs: readonly Slab[] }) {
  return (
    <div className="gob-stack" data-labels={labels ? "" : undefined} style={{ height: units(stackHeight(slabs.length)) }}>
      {runs(slabs).map((run, index) => <Run key={index} labels={labels} run={run} />)}
    </div>
  );
}

/** The same labels as a key, shown under a stack when the plot is too narrow for them. */
export function GobStackKey({ slabs }: { readonly slabs: readonly Slab[] }) {
  const labelled = runs(slabs).filter((run) => run.label !== undefined).reverse();
  if (labelled.length === 0) return null;
  return (
    <ul className="gob-stack-key">
      {labelled.map((run) => (
        <li key={run.label}>
          <span aria-hidden="true" className="gob-slab gob-slab--chip" data-kind={run.kind} />
          {run.label}
        </li>
      ))}
    </ul>
  );
}

export type StackColumn = {
  readonly title: string;
  readonly slabs: readonly Slab[];
  /** Text placed above the lid over this column, pointing down at it. */
  readonly note?: ReactNode;
};

/**
 * Two or more stacks standing on one floor under one lid (the threshold rule, a solid
 * ink line labelled at its right end). Column titles and keys sit under the floor.
 */
export function GobStacks({
  columns, lidAt, lidLabel = "threshold",
}: {
  readonly columns: readonly StackColumn[];
  readonly lidAt: number;
  readonly lidLabel?: string;
}) {
  const lid = lidHeight(lidAt);
  const tallest = Math.max(lid, ...columns.map((column) => stackHeight(column.slabs.length)));
  return (
    <div className="gob-stacks" style={{ gridTemplateColumns: `repeat(${columns.length}, minmax(0, 1fr))` }}>
      <div className="gob-stacks__plot" style={{ gridColumn: `1 / span ${columns.length}`, gridRow: 2, height: units(tallest + 0.5) }}>
        {columns.map((column, index) => (
          <div className="gob-stacks__col" data-col={index} key={column.title} style={{ left: `${(index / columns.length) * 100}%`, width: `${100 / columns.length}%` }}>
            <GobStack slabs={column.slabs} />
          </div>
        ))}
        <div className="gob-lid" style={{ bottom: units(lid) }}>
          <span className="gob-lid__label">{lidLabel}</span>
        </div>
      </div>
      {columns.some((column) => column.note !== undefined)
        ? columns.map((column) => (
          <p className="gob-stacks__note" key={`note-${column.title}`} style={{ gridRow: 1 }}>
            {column.note ?? null}
          </p>
        ))
        : null}
      {columns.map((column, index) => (
        <div className="gob-stacks__foot" data-col={index} key={`foot-${column.title}`} style={{ gridRow: 3 }}>
          <p className="gob-stacks__title">{column.title}</p>
          <GobStackKey slabs={column.slabs} />
        </div>
      ))}
    </div>
  );
}

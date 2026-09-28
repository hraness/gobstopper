// Shared geometry for the launch diagrams and charts. Pure functions, no DOM: every
// coordinate is a unit or a fraction 0–1. The site multiplies by 100 to get percent,
// the media scenes multiply by pixels, so a slab or a tick sits in the same place on
// every surface. Pure TypeScript with no imports: media/render.ts bundles this file.

/** One slab is one unit tall; the gap between slabs is a fraction of a unit. */
export const SLAB = { h: 1, gap: 0.14 } as const;

/** The brand mark: three concentric circles in a 26-unit box. */
export const RING = { box: 26, r: [13, 8, 5] } as const;

export type SlabKind = "task" | "turn" | "summary" | "kept" | "recent" | "dropped";

/** A label on the first slab of a run of equal kinds names the whole run. */
export type Slab = { readonly kind: SlabKind; readonly label?: string };

function run(kind: SlabKind, count: number, label?: string): Slab[] {
  return Array.from({ length: count }, (_, index) => (index === 0 && label !== undefined ? { kind, label } : { kind }));
}

const task = (label = "your task"): Slab => ({ kind: "task", label });
const summary: Slab = { kind: "summary", label: "summary of the middle" };

/** Column i (from 0) holds i + 1 slabs: your task, then i turns. */
export function resendColumns(turns = 5): Slab[][] {
  if (!Number.isInteger(turns) || turns < 1) throw new RangeError(`resendColumns(${turns}) needs a positive whole number.`);
  return Array.from({ length: turns }, (_, column) => [task(), ...run("turn", column)]);
}

/** D-fuse: nine slabs rise through the lid; after compaction five sit under it. */
export const FUSE = {
  before: [task(), ...run("turn", 8)],
  after: [task("your task, word for word"), summary, ...run("recent", 3, "last 3 turns, word for word")],
  lidAt: 6.5,
} as const satisfies { before: readonly Slab[]; after: readonly Slab[]; lidAt: number };

export type AnatomyBlock = { readonly kind: SlabKind; readonly w: number; readonly label?: string };

/**
 * D-anatomy: the rewrite as a share of the original request. The widths sum to 0.57,
 * the tail-0 arm's median fresh compaction (55K to 31.5K estimated tokens).
 */
export const ANATOMY = {
  original: 1,
  blocks: [
    { kind: "task", w: 0.10, label: "Head" },
    { kind: "summary", w: 0.17, label: "Summary" },
    { kind: "kept", w: 0.08, label: "Carry" },
    { kind: "recent", w: 0.073 },
    { kind: "recent", w: 0.073 },
    { kind: "recent", w: 0.074 },
  ],
} as const satisfies { original: number; blocks: readonly AnatomyBlock[] };

/** D-tail: both arms just after a rewrite. Tail 40 also keeps two older turns. */
export const TAIL = {
  // A fifth of a pitch above the tail-40 stack, so the lid never touches its top slab.
  lidAt: 7.2,
  tail0: [task(), summary, ...run("recent", 3, "last 3 turns")],
  tail40: [task(), summary, ...run("kept", 2, "older turns kept"), ...run("recent", 3, "last 3 turns")],
} as const satisfies { lidAt: number; tail0: readonly Slab[]; tail40: readonly Slab[] };

/** Height of a stack of n slabs in slab units, gaps included. */
export function stackHeight(count: number): number {
  return count <= 0 ? 0 : count * SLAB.h + (count - 1) * SLAB.gap;
}

/** Maps [d0, d1] onto [r0, r1]. */
export function linear(d0: number, d1: number, r0: number, r1: number): (v: number) => number {
  if (d0 === d1) throw new RangeError("linear() needs a domain with two distinct ends.");
  const slope = (r1 - r0) / (d1 - d0);
  return (v) => r0 + (v - d0) * slope;
}

const BOX = 1000;

function coordinate(value: number): string {
  const rounded = Math.round(value * 10) / 10;
  return Object.is(rounded, -0) ? "0" : String(rounded);
}

function points(ys: readonly number[], yMax: number): Array<readonly [string, string]> {
  if (ys.length < 2) throw new RangeError("A path needs at least two values.");
  if (!(yMax > 0)) throw new RangeError("A path needs a positive yMax.");
  const x = linear(0, ys.length - 1, 0, BOX);
  const y = linear(0, yMax, BOX, 0);
  return ys.map((value, index) => [coordinate(x(index)), coordinate(y(value))] as const);
}

/** A polyline in a 1000×1000 box, y inverted so 0 sits on the baseline. */
export function linePath(ys: readonly number[], yMax: number): string {
  return points(ys, yMax).map(([x, y], index) => `${index === 0 ? "M" : "L"}${x},${y}`).join(" ");
}

/** The same line closed down to the baseline, for a wash. */
export function areaPath(ys: readonly number[], yMax: number): string {
  return `${linePath(ys, yMax)} L${BOX},${BOX} L0,${BOX} Z`;
}

/** A step line: each value holds until the next index, then jumps. */
export function stepPath(ys: readonly number[], yMax: number): string {
  const all = points(ys, yMax);
  const [first, ...rest] = all;
  let path = `M${first![0]},${first![1]}`;
  let previousY = first![1];
  for (const [x, y] of rest) {
    path += ` H${x}`;
    if (y !== previousY) path += ` V${y}`;
    previousY = y;
  }
  return path;
}

const NICE = [1, 2, 2.5, 3, 4, 5, 6, 8, 10] as const;

/** `count` evenly spaced round ticks from 0 whose last tick reaches at least `max`. */
export function ticks(max: number, count: number): number[] {
  if (!(max > 0) || !Number.isInteger(count) || count < 2) throw new RangeError(`ticks(${max}, ${count}) needs max > 0 and count >= 2.`);
  const raw = max / (count - 1);
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const mantissa = raw / magnitude;
  const nice = NICE.find((candidate) => candidate >= mantissa - 1e-9) ?? 10;
  const step = nice * magnitude;
  return Array.from({ length: count }, (_, index) => Number((index * step).toPrecision(12)));
}

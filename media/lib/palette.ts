// The two palettes for rendered media. Stills use Paper; the film, its poster, the film
// card and the social card use Night. This is the only place these hex values live: the
// site uses its own CSS variables and never imports this file.

export type PaletteName = "paper" | "night";

export interface Palette {
  readonly background: string;
  readonly panel: string;
  readonly ink: string;
  readonly ink2: string;
  readonly rule: string;
  /** The threshold rule. */
  readonly lid: string;
  /** Gobstopper, tail 0. */
  readonly tail0: string;
  /** Gobstopper, tail 40 (old default). */
  readonly tail40: string;
  /** Claude Code, no proxy. */
  readonly noproxy: string;
  /** The summary slab, diagrams only. */
  readonly summary: string;
  readonly slab: string;
  readonly slabEdge: string;
  /** The brand mark keeps its own colour in both palettes. */
  readonly mark: string;
}

export const PAPER: Palette = {
  background: "#f8f7f4",
  panel: "#fffefa",
  ink: "#1c1917",
  ink2: "#6c665f",
  rule: "#dfdcd6",
  lid: "#1c1917",
  tail0: "#2256bb",
  tail40: "#c26300",
  noproxy: "#00906f",
  summary: "#7858a3",
  slab: "#e4e1db",
  slabEdge: "#6c665f",
  mark: "#2474d4",
};

export const NIGHT: Palette = {
  background: "#1a1b26",
  panel: "#16161e",
  ink: "#c0caf5",
  ink2: "#a9b1d6",
  rule: "#2f3549",
  lid: "#c0caf5",
  tail0: "#4e75d2",
  tail40: "#d0792a",
  noproxy: "#199c7e",
  summary: "#9d7cd8",
  slab: "#2f3549",
  slabEdge: "#a9b1d6",
  mark: "#2474d4",
};

export const PALETTES: Readonly<Record<PaletteName, Palette>> = { paper: PAPER, night: NIGHT };

/** CSS custom property name for each palette role, set on :root by stage.boot(). */
export const PALETTE_VARS: Readonly<Record<keyof Palette, string>> = {
  background: "--bg",
  panel: "--panel",
  ink: "--ink",
  ink2: "--ink-2",
  rule: "--rule",
  lid: "--lid",
  tail0: "--tail0",
  tail40: "--tail40",
  noproxy: "--noproxy",
  summary: "--summary",
  slab: "--slab",
  slabEdge: "--slab-edge",
  mark: "--mark",
};

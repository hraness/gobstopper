// In-page runtime for every Gobstopper media scene (stills and film shots).
//
// render.ts bundles this file with site/app/_lib/gob-geometry.ts and
// site/app/_lib/gobbench-format.ts into one IIFE and inlines it into each scene, where it
// is available as the global `Gob` (see lib/bundle.ts). A scene calls
// `Gob.boot({ palette })` from a classic inline script and draws in the returned stage.
//
// Determinism: every visual is a pure function of the render clock. boot() registers its
// readiness promise and its single frame callback synchronously, before the first frame;
// timeline() builds paused Web Animations that the frame callback seeks to
// `frame.timeMs + timeOffsetSeconds`. Nothing here reads Math.random or wall-clock time.
//
// The host seeks every document animation to the frame time before frame callbacks run, so
// timeline() needs no trackAnimation() call (which is closed once readiness settlement
// begins); the stage's frame callback then applies the scene's time offset on top.

import { RING, type SlabKind } from "../../site/app/_lib/gob-geometry";
import { PALETTES, PALETTE_VARS, type Palette, type PaletteName } from "./palette";

/** The page-facing Slopcamera overlay API, as far as the stage uses it. */
interface OverlayFrame { frame: number; timeMs: number; deltaMs: number; progress: number; width: number; height: number }
interface Overlay {
  readonly width: number;
  readonly height: number;
  readonly durationMs: number;
  readonly fps: number;
  readonly seed: number;
  readonly parameters: Readonly<Record<string, unknown>>;
  asset(name: string): string;
  ready(promise: PromiseLike<unknown>): Promise<unknown>;
  onFrame(callback: (frame: OverlayFrame) => void | Promise<void>): () => void;
  trackAnimation<T>(animation: T): T;
}
declare const SlopcameraOverlay: Overlay;

/** Declared scene resources. render.ts declares exactly these names. */
export const RESOURCES = [
  { name: "nebula-book", path: "brand/fonts/NebulaSans-Book.woff2", urlPath: "fonts/NebulaSans-Book.woff2", mediaType: "font/woff2" },
  { name: "nebula-medium", path: "brand/fonts/NebulaSans-Medium.woff2", urlPath: "fonts/NebulaSans-Medium.woff2", mediaType: "font/woff2" },
  { name: "nebula-semibold", path: "brand/fonts/NebulaSans-Semibold.woff2", urlPath: "fonts/NebulaSans-Semibold.woff2", mediaType: "font/woff2" },
  { name: "geist-mono", path: "brand/fonts/GeistMono-wght.woff2", urlPath: "fonts/GeistMono-wght.woff2", mediaType: "font/woff2" },
  { name: "mark", path: "brand/mark.svg", urlPath: "brand/mark.svg", mediaType: "image/svg+xml" },
] as const;

export const FONT_TEXT = `"Nebula Sans", sans-serif`;
export const FONT_MONO = `"Geist Mono", monospace`;

/** Default eases and durations (seconds). One ease per segment, never one per timeline. */
export const EASE = {
  enter: "cubic-bezier(.2,.8,.2,1)",
  exit: "cubic-bezier(.4,0,1,1)",
  fuse: "cubic-bezier(.65,0,.35,1)",
  linear: "linear",
} as const;
export const DUR = { enter: 0.4, exit: 0.25, fuse: 0.6 } as const;

export type TextRole = "title" | "beat" | "hero" | "label" | "small" | "prov" | "mono";
export type TypeScale = "film" | "still";

type RoleStyle = { size: number; line: number; weight: number; mono?: boolean; ink2?: boolean; tracking?: number };

/**
 * Type scale in CSS px. Film: 1920×1080 canvas (SPEC §1.3). Still: 1600×900 canvas shown
 * at roughly 720–830 px wide on GitHub and the blog and 360 px on a phone, so every role
 * is larger than the film scale to stay legible after that reduction.
 */
export const TYPE: Readonly<Record<TypeScale, Readonly<Record<TextRole, RoleStyle>>>> = {
  film: {
    title: { size: 30, line: 1.2, weight: 600 },
    beat: { size: 64, line: 1.08, weight: 600, tracking: -0.01 },
    hero: { size: 160, line: 1, weight: 600, tracking: -0.02 },
    label: { size: 30, line: 1.2, weight: 400 },
    small: { size: 24, line: 1.25, weight: 400 },
    prov: { size: 22, line: 1.3, weight: 400, ink2: true },
    mono: { size: 30, line: 1.2, weight: 450, mono: true },
  },
  still: {
    title: { size: 46, line: 1.12, weight: 600, tracking: -0.008 },
    beat: { size: 46, line: 1.12, weight: 600, tracking: -0.008 },
    hero: { size: 132, line: 1, weight: 600, tracking: -0.02 },
    label: { size: 34, line: 1.25, weight: 400 },
    small: { size: 29, line: 1.3, weight: 400 },
    prov: { size: 24, line: 1.4, weight: 400, ink2: true },
    mono: { size: 29, line: 1.35, weight: 450, mono: true },
  },
};

export type Box = { x: number; y: number; w?: number; h?: number };
export type Align = "left" | "center" | "right";
export type SlabTone = "tail" | "anatomy";

export interface Segment {
  el: Element;
  from: Keyframe;
  to: Keyframe;
  /** Seconds on the scene clock (frame time plus timeOffsetSeconds). */
  t0: number;
  t1: number;
  ease?: string;
}

export interface StageParams {
  F: Record<string, unknown>;
  data: Record<string, unknown>;
  timeOffsetSeconds?: number;
  [key: string]: unknown;
}

export interface StillFrame {
  /** The plot area between the header and the footer. */
  plot: Required<Box>;
  header: HTMLElement;
  footer: HTMLElement | null;
}

export interface Stage {
  readonly params: StageParams;
  readonly palette: Palette;
  readonly paletteName: PaletteName;
  readonly scale: TypeScale;
  /** The canvas-sized CSS box every helper draws into. */
  readonly root: HTMLElement;
  readonly width: number;
  readonly height: number;
  /** Seeded, deterministic value in [0, 1) for a key; replaces Math.random. */
  jitter(key: string): number;
  el<K extends keyof HTMLElementTagNameMap>(tag: K, css?: Partial<CSSStyleDeclaration> | string, parent?: Element): HTMLElementTagNameMap[K];
  slab(kind: SlabKind, box: Required<Box>, label?: string, opts?: { tone?: SlabTone; radius?: number; parent?: Element }): HTMLElement;
  lid(y: number, label: string, opts?: { x0?: number; x1?: number; parent?: Element }): HTMLElement;
  ring(x: number, y: number, size?: number, opts?: { color?: string; parent?: Element; stroke?: number }): SVGElement;
  mark(x: number, y: number, size: number, parent?: Element): HTMLImageElement;
  text(role: TextRole, s: string, box: Box, opts?: { align?: Align; color?: string; weight?: number; parent?: Element; nowrap?: boolean }): HTMLElement;
  lowerThird(s: string): HTMLElement;
  timeline(segments: Segment[]): void;
  /** Marks the scene built; the host renders frames only after this. scene() calls it. */
  done(): void;
  /** Fails the render with an error. */
  fail(error: unknown): void;
  /** Stills: border, title, optional subtitle, mark and provenance. Returns the plot box. */
  still(opts: { title: string; subtitle?: string; provenance?: string; legend?: HTMLElement }): StillFrame;
}

function hash32(input: string, seed: number): number {
  // FNV-1a, then a murmur finaliser: stable across engines.
  let h = (0x811c9dc5 ^ seed) >>> 0;
  for (let i = 0; i < input.length; i++) {
    h ^= input.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  h ^= h >>> 16; h = Math.imul(h, 0x85ebca6b) >>> 0;
  h ^= h >>> 13; h = Math.imul(h, 0xc2b2ae35) >>> 0;
  h ^= h >>> 16;
  return h >>> 0;
}

function applyCss(node: HTMLElement | SVGElement, css: Partial<CSSStyleDeclaration> | string | undefined): void {
  if (css === undefined) return;
  if (typeof css === "string") node.style.cssText += css;
  else Object.assign(node.style, css);
}

function baseStyles(scale: TypeScale): string {
  const roles = TYPE[scale];
  const rules = (Object.keys(roles) as TextRole[]).map((role) => {
    const r = roles[role];
    return `.gob-t-${role}{font-family:${r.mono ? FONT_MONO : FONT_TEXT};font-size:${r.size}px;line-height:${r.line};`
      + `font-weight:${r.weight};color:var(${r.ink2 ? "--ink-2" : "--ink"});`
      + `${r.tracking ? `letter-spacing:${r.tracking}em;` : ""}${r.mono ? "font-variant-ligatures:none;" : ""}}`;
  }).join("\n");
  return `
html,body{margin:0;width:100%;height:100%;overflow:hidden;background:transparent!important}
*,*::before,*::after{box-sizing:border-box}
#gob-stage{position:absolute;left:0;top:0;overflow:hidden;background:var(--bg);color:var(--ink);
  font-family:${FONT_TEXT};font-kerning:normal;font-feature-settings:"kern" 1;text-rendering:geometricPrecision;
  -webkit-font-smoothing:antialiased;font-variant-numeric:tabular-nums}
.gob-abs{position:absolute}
.gob-t{position:absolute;margin:0;text-wrap:pretty}
.gob-t-hero,.gob-t-beat,.gob-t-title{text-wrap:balance;font-variant-numeric:proportional-nums}
${rules}
.gob-slab{position:absolute;display:flex;align-items:center;overflow:hidden;white-space:nowrap}
.gob-slab>span{padding:0 0.7em;font-weight:500}
`;
}

/**
 * Start a scene. Registers readiness (fonts) and the frame callback synchronously, then
 * resolves once the fonts are loaded. Call it once, from the scene's inline script.
 */
export function boot(opts: { palette: PaletteName; scale?: TypeScale }): Promise<Stage> {
  const api = SlopcameraOverlay;
  const paletteName = opts.palette;
  const palette = PALETTES[paletteName];
  if (palette === undefined) throw new RangeError(`Unknown palette ${String(paletteName)}`);
  const scale: TypeScale = opts.scale ?? (api.width === 1920 ? "film" : "still");
  const params = api.parameters as unknown as StageParams;
  const offsetMs = Math.round((typeof params.timeOffsetSeconds === "number" ? params.timeOffsetSeconds : 0) * 1000);

  const rootStyle = document.documentElement.style;
  for (const key of Object.keys(PALETTE_VARS) as (keyof Palette)[]) rootStyle.setProperty(PALETTE_VARS[key], palette[key]);
  rootStyle.setProperty("--on-fill", paletteName === "paper" ? "#fffefa" : "#ffffff");
  rootStyle.setProperty("color-scheme", paletteName === "paper" ? "light" : "dark");

  const style = document.createElement("style");
  style.textContent = baseStyles(scale);
  document.head.appendChild(style);

  const faces = [
    new FontFace("Nebula Sans", `url("${api.asset("nebula-book")}") format("woff2")`, { weight: "400", style: "normal" }),
    new FontFace("Nebula Sans", `url("${api.asset("nebula-medium")}") format("woff2")`, { weight: "500", style: "normal" }),
    new FontFace("Nebula Sans", `url("${api.asset("nebula-semibold")}") format("woff2")`, { weight: "600", style: "normal" }),
    new FontFace("Geist Mono", `url("${api.asset("geist-mono")}") format("woff2")`, { weight: "100 900", style: "normal" }),
  ];
  for (const face of faces) document.fonts.add(face);
  const markUrl = api.asset("mark");
  const markImage = new Image();
  markImage.src = markUrl;
  const assets = Promise.all([...faces.map((face) => face.load()), markImage.decode()]).then(() => document.fonts.ready);
  // The host closes registration at the first frame and then waits for readiness, so the
  // scene must finish building inside readiness: `built` settles only on stage.done().
  let finish: () => void = () => undefined;
  let fail: (error: unknown) => void = () => undefined;
  const built = new Promise<void>((resolve, reject) => { finish = resolve; fail = reject; });
  api.ready(assets.then(() => built));

  const animations: Animation[] = [];
  api.onFrame((frame) => {
    const t = frame.timeMs + offsetMs;
    for (const animation of animations) animation.currentTime = t;
  });

  const root = document.createElement("div");
  root.id = "gob-stage";
  root.style.width = `${api.width}px`;
  root.style.height = `${api.height}px`;
  document.body.appendChild(root);

  const el: Stage["el"] = (tag, css, parent) => {
    const node = document.createElement(tag);
    applyCss(node, css);
    (parent ?? root).appendChild(node);
    return node;
  };

  const place = (node: HTMLElement, box: Box) => {
    node.style.left = `${box.x}px`;
    node.style.top = `${box.y}px`;
    if (box.w !== undefined) node.style.width = `${box.w}px`;
    if (box.h !== undefined) node.style.height = `${box.h}px`;
  };

  const text: Stage["text"] = (role, s, box, o = {}) => {
    const node = el("p", undefined, o.parent);
    node.className = `gob-t gob-t-${role}`;
    node.textContent = s;
    place(node, box);
    if (o.align) node.style.textAlign = o.align;
    if (o.color) node.style.color = o.color;
    if (o.weight) node.style.fontWeight = String(o.weight);
    if (o.nowrap) node.style.whiteSpace = "nowrap";
    return node;
  };

  const slab: Stage["slab"] = (kind, box, label, o = {}) => {
    const node = el("div", undefined, o.parent);
    node.className = `gob-slab gob-slab-${kind}`;
    place(node, box);
    node.style.borderRadius = `${o.radius ?? (scale === "film" ? 12 : 10)}px`;
    const edge = (w: number, c: string) => `inset 0 0 0 ${w}px ${c}`;
    let fill = "var(--slab)";
    let ink = "var(--ink)";
    let shadow = edge(1.5, "color-mix(in oklab, var(--slab-edge) 45%, transparent)");
    switch (kind) {
      case "task": shadow = edge(2, "var(--ink)"); break;
      case "summary": fill = "var(--summary)"; ink = "var(--on-fill)"; shadow = "none"; break;
      case "recent": fill = "var(--tail0)"; ink = "var(--on-fill)"; shadow = "none"; break;
      case "kept":
        if (o.tone === "anatomy") shadow = edge(2, "var(--summary)");
        else { fill = "var(--tail40)"; ink = "var(--on-fill)"; shadow = "none"; }
        break;
      case "dropped": node.style.opacity = "0.4"; break;
      case "turn": break;
    }
    node.style.background = fill;
    node.style.boxShadow = shadow;
    node.style.color = ink;
    if (label !== undefined) {
      const span = document.createElement("span");
      span.className = "gob-t-small";
      span.style.color = ink;
      span.style.fontWeight = "500";
      span.textContent = label;
      node.appendChild(span);
    }
    return node;
  };

  const lid: Stage["lid"] = (y, label, o = {}) => {
    const x0 = o.x0 ?? 0;
    const x1 = o.x1 ?? api.width;
    const group = el("div", `position:absolute;left:${x0}px;top:${y - 1}px;width:${x1 - x0}px;height:2px;background:var(--lid)`, o.parent);
    group.className = "gob-lid";
    const tag = document.createElement("p");
    tag.className = "gob-t gob-t-small";
    tag.textContent = label;
    tag.style.cssText += "right:0;bottom:12px;white-space:nowrap;font-weight:500";
    group.appendChild(tag);
    return group;
  };

  const ring: Stage["ring"] = (x, y, size = 28, o = {}) => {
    const ns = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(ns, "svg");
    svg.setAttribute("viewBox", `0 0 ${RING.box} ${RING.box}`);
    svg.setAttribute("width", String(size));
    svg.setAttribute("height", String(size));
    svg.setAttribute("aria-hidden", "true");
    svg.style.cssText = `position:absolute;left:${x - size / 2}px;top:${y - size / 2}px;overflow:visible;color:${o.color ?? "var(--ink)"}`;
    const stroke = o.stroke ?? 1.6;
    for (const r of RING.r) {
      const circle = document.createElementNS(ns, "circle");
      circle.setAttribute("cx", String(RING.box / 2));
      circle.setAttribute("cy", String(RING.box / 2));
      circle.setAttribute("r", String(r - stroke / 2));
      circle.setAttribute("fill", "none");
      circle.setAttribute("stroke", "currentColor");
      circle.setAttribute("stroke-width", String(stroke));
      svg.appendChild(circle);
    }
    (o.parent ?? root).appendChild(svg);
    return svg;
  };

  const mark: Stage["mark"] = (x, y, size, parent) => {
    const img = el("img", `position:absolute;left:${x}px;top:${y}px;width:${size}px;height:${size}px`, parent);
    img.src = markUrl;
    img.alt = "";
    return img;
  };

  const lowerThird: Stage["lowerThird"] = (s) => {
    const node = text("prov", s, { x: 120, y: api.height - 64 }, { nowrap: true });
    node.style.top = "auto";
    node.style.bottom = "64px";
    return node;
  };

  const timeline: Stage["timeline"] = (segments) => {
    // Web Animations composite later-created effects on top. A later segment's backwards
    // fill would hide an earlier one, so only each element's first segment fills backwards.
    const sorted = [...segments].sort((a, b) => a.t0 - b.t0);
    const seen = new Set<Element>();
    for (const seg of sorted) {
      if (!(seg.t1 > seg.t0)) throw new RangeError(`timeline segment needs t1 > t0 (got ${seg.t0}..${seg.t1})`);
      const first = !seen.has(seg.el);
      seen.add(seg.el);
      const animation = seg.el.animate([seg.from, seg.to], {
        delay: seg.t0 * 1000,
        duration: (seg.t1 - seg.t0) * 1000,
        easing: seg.ease ?? EASE.enter,
        fill: first ? "both" : "forwards",
      });
      animation.pause();
      animation.currentTime = offsetMs;
      animations.push(animation);
    }
  };

  const still: Stage["still"] = (o) => {
    const W = api.width;
    const H = api.height;
    const padX = 72;
    const padTop = 60;
    const padBottom = 52;
    el("div", `position:absolute;inset:0;box-shadow:inset 0 0 0 1px var(--rule);pointer-events:none;z-index:5`);
    const header = el("div", `position:absolute;left:${padX}px;top:${padTop}px;width:${W - padX * 2}px`);
    const markSize = 44;
    mark(W - padX - markSize, padTop + 3, markSize);
    const titleNode = text("title", o.title, { x: 0, y: 0, w: W - padX * 2 - markSize - 40 }, { parent: header });
    titleNode.style.position = "relative";
    if (o.subtitle) {
      const sub = text("label", o.subtitle, { x: 0, y: 0, w: W - padX * 2 - markSize - 40 }, { parent: header, color: "var(--ink-2)" });
      sub.style.position = "relative";
      sub.style.marginTop = "10px";
    }
    if (o.legend) {
      header.appendChild(o.legend);
      o.legend.style.position = "relative";
      o.legend.style.marginTop = "22px";
    }
    let footer: HTMLElement | null = null;
    let footerTop = H - padBottom;
    if (o.provenance) {
      footer = el("div", `position:absolute;left:${padX}px;bottom:${padBottom}px;width:${W - padX * 2}px;padding-top:18px;border-top:1px solid var(--rule)`);
      const prov = text("prov", o.provenance, { x: 0, y: 0 }, { parent: footer });
      prov.style.position = "relative";
      footerTop = H - padBottom - footer.getBoundingClientRect().height;
    }
    const headerBottom = padTop + header.getBoundingClientRect().height;
    const plot = { x: padX, y: Math.round(headerBottom + 44), w: W - padX * 2, h: Math.round(footerTop - 40 - (headerBottom + 44)) };
    return { plot, header, footer };
  };

  const stage: Stage = {
    params, palette, paletteName, scale, root,
    width: api.width,
    height: api.height,
    jitter: (key) => hash32(key, api.seed) / 0x1_0000_0000,
    el, slab, lid, ring, mark, text, lowerThird, timeline, still,
    done: () => finish(),
    fail: (error) => fail(error),
  };
  return assets.then(() => stage, (error: unknown) => { fail(error); throw error; });
}

/**
 * The usual scene entry point: boot, build, then mark the scene ready. A thrown build
 * error fails the render instead of leaving the host waiting for readiness.
 */
export function scene(opts: { palette: PaletteName; scale?: TypeScale }, build: (stage: Stage) => void | Promise<void>): void {
  void boot(opts).then(async (stage) => {
    try {
      await build(stage);
      stage.done();
    } catch (error) {
      stage.fail(error);
    }
  });
}

/** A legend row of swatches. Each item is a label and a CSS background for its chip. */
export function legend(items: readonly { label: string; fill: string; edge?: string; shape?: "chip" | "line" }[], gap = 36): HTMLElement {
  const row = document.createElement("div");
  row.style.cssText = `display:flex;flex-wrap:wrap;gap:12px ${gap}px;align-items:center`;
  for (const item of items) {
    const entry = document.createElement("div");
    entry.style.cssText = "display:flex;align-items:center;gap:14px";
    const chip = document.createElement("span");
    chip.style.cssText = item.shape === "line"
      ? `display:inline-block;width:36px;height:0;border-top:${item.fill}`
      : `display:inline-block;width:30px;height:22px;border-radius:5px;background:${item.fill};${item.edge ? `box-shadow:inset 0 0 0 2px ${item.edge}` : ""}`;
    const label = document.createElement("span");
    label.className = "gob-t-small";
    label.style.color = "var(--ink-2)";
    label.textContent = item.label;
    entry.append(chip, label);
    row.appendChild(entry);
  }
  return row;
}

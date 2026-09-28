// The one script render.ts inlines into every scene, as the global `Gob`. It carries the
// stage runtime, the palettes, the shared geometry, the provenance lines and the launch
// formatter, so a scene draws the same slabs and prints the same strings as the site.
import * as geometry from "../../site/app/_lib/gob-geometry";
import { ARM_LABEL, ARM_ORDER, type ArmId } from "../../site/app/_lib/gobbench-data";
import { F, GAP_ROWS, pct, range, wilson } from "../../site/app/_lib/gobbench-format";
import { PALETTES } from "./palette";
import { PROV } from "./provenance";
import { ARM_STYLE, DUR, EASE, FONT_MONO, FONT_TEXT, TYPE, boot, hatch, legend, marker, scene, svgLayer, svgNode } from "./stage";

const Gob = Object.freeze({
  boot, scene, legend, svgLayer, svgNode, hatch, marker, ARM_STYLE, EASE, DUR, TYPE, FONT_TEXT, FONT_MONO, PALETTES,
  geometry, F, GAP_ROWS, ARM_LABEL, ARM_ORDER, PROV, pct, range, wilson,
});

export type GobRuntime = typeof Gob;
export type { ArmId };

Object.defineProperty(globalThis, "Gob", { value: Gob, enumerable: true, configurable: false, writable: false });

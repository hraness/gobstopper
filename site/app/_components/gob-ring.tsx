import { RING } from "../_lib/gob-geometry";

/**
 * The ring glyph: the brand mark's three concentric circles, stroked in currentColor.
 * It marks a compaction point wherever one is drawn. Decorative; the figure's text
 * alternative carries the meaning.
 */
export function GobRing({ className }: { readonly className?: string }) {
  const centre = RING.box / 2;
  return (
    <svg
      aria-hidden="true"
      className={className === undefined ? "gob-ring" : `gob-ring ${className}`}
      focusable="false"
      viewBox={`0 0 ${RING.box} ${RING.box}`}
    >
      {RING.r.map((radius) => (
        // The outer circle sits on the box edge; inset by half the stroke so it is not clipped.
        <circle cx={centre} cy={centre} key={radius} r={radius === RING.box / 2 ? radius - 1.25 : radius} />
      ))}
    </svg>
  );
}

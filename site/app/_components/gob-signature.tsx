import type { CSSProperties } from "react";

/**
 * The site's one signature moment: a long transcript squeezing into a short
 * one while the full history slides into a local vault. Pure SVG and CSS; the
 * final frame is the default and motion runs only when the reader allows it.
 * The drawing is aria-hidden and the figcaption carries its meaning.
 */

const KEPT = [
  { y: 12, h: 10, kind: "system" },
  { y: 26, h: 14, kind: "task" },
] as const;

/** Twelve older turns that become one summary. Heights vary like real tool output. */
const OLD = [18, 10, 24, 8, 16, 28, 12, 10, 20, 8, 14, 22] as const;
const RECENT = [14, 10, 16] as const;

const WIDTH = 420;
const COL_W = 110;
const LEFT_X = 16;
const RIGHT_X = 294;
const GAP = 4;

function stack(heights: readonly number[], start: number): { y: number; h: number }[] {
  let y = start;
  return heights.map((h) => {
    const bar = { y, h };
    y += h + GAP;
    return bar;
  });
}

const leftOld = stack(OLD, KEPT[1].y + KEPT[1].h + GAP);
const lastOld = leftOld[leftOld.length - 1] ?? { y: 0, h: 0 };
const leftRecent = stack(RECENT, lastOld.y + lastOld.h + GAP);
const lastLeft = leftRecent[leftRecent.length - 1] ?? { y: 0, h: 0 };
const leftBottom = lastLeft.y + lastLeft.h;

const SUMMARY_Y = KEPT[1].y + KEPT[1].h + GAP;
const SUMMARY_H = 22;
const rightRecent = stack(RECENT, SUMMARY_Y + SUMMARY_H + GAP);
const lastRight = rightRecent[rightRecent.length - 1] ?? { y: 0, h: 0 };
const rightBottom = lastRight.y + lastRight.h;

const THRESHOLD_Y = 162;
const VAULT_X = RIGHT_X - 12;
const VAULT_W = COL_W + 24;
const VAULT_Y = rightBottom + 48;
const VAULT_H = 64;
const HEIGHT = leftBottom + 26;
const ARROW_Y = 90;

export const GOB_SIGNATURE_ASPECT = `${WIDTH} / ${HEIGHT}`;

export function GobCompactionSignature() {
  return (
    <figure className="gob-sig" data-hraness-signature="compaction" style={{ "--gob-sig-aspect": GOB_SIGNATURE_ASPECT } as CSSProperties}>
      <svg
        aria-hidden="true"
        className="gob-sig__canvas"
        focusable="false"
        viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
      >
        {/* Left: the whole conversation as the agent would resend it */}
        <g className="gob-sig__long">
          {KEPT.map((bar) => <rect key={bar.kind} className="gob-sig__bar gob-sig__bar--kept" height={bar.h} rx="2" width={COL_W} x={LEFT_X} y={bar.y} />)}
          {leftOld.map((bar, index) => (
            <rect
              key={bar.y}
              className="gob-sig__bar gob-sig__bar--old"
              height={bar.h}
              rx="2"
              style={{ "--gob-i": index, "--gob-dy": `${SUMMARY_Y - bar.y}px` } as CSSProperties}
              width={COL_W}
              x={LEFT_X}
              y={bar.y}
            />
          ))}
          {leftRecent.map((bar) => <rect key={bar.y} className="gob-sig__bar gob-sig__bar--kept" height={bar.h} rx="2" width={COL_W} x={LEFT_X} y={bar.y} />)}
          <text className="gob-sig__label" x={LEFT_X} y={leftBottom + 18}>{`${OLD.length + RECENT.length + KEPT.length} turns resent`}</text>
        </g>

        {/* Threshold across the left column */}
        <line className="gob-sig__threshold" x1={LEFT_X - 8} x2={LEFT_X + COL_W + 8} y1={THRESHOLD_Y} y2={THRESHOLD_Y} />
        <text className="gob-sig__label gob-sig__label--threshold" x={LEFT_X + COL_W + 14} y={THRESHOLD_Y + 5}>threshold</text>

        {/* Arrow */}
        <g className="gob-sig__arrow">
          <line x1={LEFT_X + COL_W + 28} x2={RIGHT_X - 30} y1={ARROW_Y} y2={ARROW_Y} />
          <path d={`M${RIGHT_X - 37} ${ARROW_Y - 6} L${RIGHT_X - 29} ${ARROW_Y} L${RIGHT_X - 37} ${ARROW_Y + 6}`} />
          <text className="gob-sig__label" textAnchor="middle" x={(LEFT_X + COL_W + 28 + RIGHT_X - 30) / 2} y={ARROW_Y - 10}>compact</text>
        </g>

        {/* Right: what the model receives */}
        <g className="gob-sig__short">
          {KEPT.map((bar) => <rect key={bar.kind} className="gob-sig__bar gob-sig__bar--kept" height={bar.h} rx="2" width={COL_W} x={RIGHT_X} y={bar.y} />)}
          <rect className="gob-sig__bar gob-sig__bar--summary" height={SUMMARY_H} rx="2" width={COL_W} x={RIGHT_X} y={SUMMARY_Y} />
          <text className="gob-sig__label gob-sig__label--inside" x={RIGHT_X + 8} y={SUMMARY_Y + 15}>summary</text>
          {rightRecent.map((bar) => <rect key={bar.y} className="gob-sig__bar gob-sig__bar--kept" height={bar.h} rx="2" width={COL_W} x={RIGHT_X} y={bar.y} />)}
          <text className="gob-sig__label" x={RIGHT_X} y={rightBottom + 18}>{`${RECENT.length + KEPT.length} turns + summary`}</text>
        </g>

        {/* Vault: the full history, kept locally */}
        <g className="gob-sig__vault">
          <path className="gob-sig__vault-arrow" d={`M${LEFT_X + COL_W + 8} 250 C ${LEFT_X + COL_W + 90} 250, ${VAULT_X - 60} ${VAULT_Y + VAULT_H / 2}, ${VAULT_X - 4} ${VAULT_Y + VAULT_H / 2}`} />
          <rect className="gob-sig__vault-box" height={VAULT_H} rx="6" width={VAULT_W} x={VAULT_X} y={VAULT_Y} />
          {leftOld.slice(0, 6).map((bar, index) => (
            <rect
              key={bar.y}
              className="gob-sig__bar gob-sig__bar--archived"
              height={6}
              rx="1.5"
              style={{ "--gob-i": index } as CSSProperties}
              width={COL_W - 4}
              x={RIGHT_X + 2}
              y={VAULT_Y + 9 + index * 9}
            />
          ))}
          <text className="gob-sig__label" x={VAULT_X} y={VAULT_Y + VAULT_H + 18}>
            <tspan x={VAULT_X}>full history</tspan>
            <tspan dy="17" x={VAULT_X}>in a local vault</tspan>
          </text>
        </g>
      </svg>
      <figcaption className="gob-sig__caption">
        Past the threshold, Gobstopper keeps the system prompt, your first task, and the last three turns word for word, turns the older turns into one summary, and writes the whole conversation to a local vault your agents can search.
      </figcaption>
    </figure>
  );
}

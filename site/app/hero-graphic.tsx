"use client";

import { useEffect, useRef, useState } from "react";

// Illustrative sawtooth: context occupancy per turn under two policies.
// The provider compacts once, late, near the top of the window; a 250k/40k
// Gobstopper policy compacts at every crossing of the trigger line.
const TURNS = 28;
const RISE = 34; // tokens per turn, in thousands
const FLOOR = 40;
const TRIGGER = 250;
const CEILING = 1000;
const PROVIDER_DROP_AT = 26.5;
const PROVIDER_AFTER = 90;
const PERIOD = (TRIGGER - FLOOR) / RISE;

function gobstopperAt(turn: number) {
  return FLOOR + (turn % PERIOD) * RISE;
}

function providerAt(turn: number) {
  if (turn < PROVIDER_DROP_AT) return Math.min(920, FLOOR + turn * RISE);
  return PROVIDER_AFTER + (turn - PROVIDER_DROP_AT) * RISE;
}

const W = 720;
const H = 252;
const PAD_L = 46;
const PAD_R = 14;
const PAD_T = 16;
const PAD_B = 30;
const plotW = W - PAD_L - PAD_R;
const plotH = H - PAD_T - PAD_B;

const xFor = (turn: number) => PAD_L + (turn / TURNS) * plotW;
const yFor = (k: number) => PAD_T + plotH - (k / CEILING) * plotH;

function gobstopperPath() {
  const points: string[] = [`M ${xFor(0)} ${yFor(FLOOR)}`];
  let ctx = FLOOR;
  for (let t = 0; t <= TURNS; t += 0.5) {
    const next = FLOOR + (t % PERIOD) * RISE;
    if (next < ctx) {
      // Compaction: drop vertically to the floor at this turn.
      points.push(`L ${xFor(t)} ${yFor(FLOOR)}`);
    }
    ctx = next;
    points.push(`L ${xFor(t)} ${yFor(ctx)}`);
  }
  return points.join(" ");
}

function providerPath() {
  const points: string[] = [`M ${xFor(0)} ${yFor(providerAt(0))}`];
  for (let t = 0.5; t <= TURNS; t += 0.5) {
    const prev = providerAt(t - 0.5);
    const next = providerAt(t);
    if (next < prev) points.push(`L ${xFor(t)} ${yFor(next)}`);
    points.push(`L ${xFor(t)} ${yFor(next)}`);
  }
  return points.join(" ");
}

// Compaction-event ticks: each Gobstopper drop.
const dropTurns: number[] = [];
for (let t = PERIOD; t <= TURNS; t += PERIOD) dropTurns.push(t);
const dropLabels = ["elide ×43", "compact", "elide ×38", "compact"];

const GOB_PATH = gobstopperPath();
const PROVIDER_PATH = providerPath();

/**
 * The hero's context-occupancy sawtooth. A playhead sweeps across the chart;
 * pointer position scrubs it and the readout reports both policies' context
 * at that turn. Idle motion resumes a few seconds after the pointer rests.
 */
export function HeroGraphic() {
  const [frac, setFrac] = useState(0.62);
  const lastPointer = useRef(-1e4);
  const frame = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    let raf = 0;
    const tick = () => {
      if (performance.now() - lastPointer.current > 3200) {
        const t = performance.now() / 1000;
        setFrac(0.5 + 0.46 * Math.sin(t * 0.21));
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);

  const onMove = (event: React.PointerEvent) => {
    const rect = frame.current?.getBoundingClientRect();
    if (rect === undefined || rect.width === 0) return;
    lastPointer.current = performance.now();
    setFrac(Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width)));
  };

  const turn = frac * TURNS;
  const gob = gobstopperAt(turn);
  const pro = providerAt(turn);
  const px = PAD_L + frac * plotW;

  return (
    <div className="gob-sawtooth" ref={frame} onPointerMove={onMove}>
      <div className="gob-sawtooth-head">
        <span className="gob-sawtooth-title">context occupancy · 1M-token window</span>
        <span className="gob-sawtooth-readout" aria-hidden="true">
          turn {Math.round(turn)} · gobstopper ~{Math.round(gob)}k · provider ~{Math.round(pro)}k
        </span>
      </div>
      <svg className="gob-sawtooth-chart" viewBox={`0 0 ${W} ${H}`} role="img" aria-label="Context occupancy per turn: the provider compacts once near the top of the window; Gobstopper compacts at each 250k trigger crossing.">
        {/* Window guides */}
        {[0, 250, 500, 750, 1000].map((k) => (
          <g key={k}>
            <line className="gob-sawtooth-grid" x1={PAD_L} x2={W - PAD_R} y1={yFor(k)} y2={yFor(k)} />
            <text className="gob-sawtooth-tick" x={PAD_L - 8} y={yFor(k) + 4} textAnchor="end">{k === 0 ? "0" : `${k}k`}</text>
          </g>
        ))}
        {/* Trigger line and floor band */}
        <rect className="gob-sawtooth-floor" x={PAD_L} y={yFor(FLOOR)} width={plotW} height={yFor(0) - yFor(FLOOR)} />
        <line className="gob-sawtooth-trigger" x1={PAD_L} x2={W - PAD_R} y1={yFor(TRIGGER)} y2={yFor(TRIGGER)} />
        <text className="gob-sawtooth-axislabel" x={W - PAD_R - 4} y={yFor(TRIGGER) - 6} textAnchor="end">trigger {TRIGGER}k</text>
        <text className="gob-sawtooth-axislabel" x={PAD_L + 6} y={yFor(FLOOR) - 4}>floor {FLOOR}k</text>
        {/* Series */}
        <path className="gob-sawtooth-provider" d={PROVIDER_PATH} />
        <path className="gob-sawtooth-series" d={GOB_PATH} />
        {/* Compaction events */}
        {dropTurns.map((t, i) => (
          <g key={t} className="gob-sawtooth-event">
            <path d={`M ${xFor(t) - 5} ${yFor(FLOOR) + 12} L ${xFor(t)} ${yFor(FLOOR) + 5} L ${xFor(t) + 5} ${yFor(FLOOR) + 12} Z`} />
            {dropLabels[i] !== undefined && (
              <text x={xFor(t)} y={yFor(FLOOR) + 26} textAnchor="middle">{dropLabels[i]}</text>
            )}
          </g>
        ))}
        {/* Playhead */}
        <line className="gob-sawtooth-playhead" x1={px} x2={px} y1={PAD_T} y2={yFor(0)} />
        <circle className="gob-sawtooth-dot gob-sawtooth-dot--gob" cx={px} cy={yFor(gob)} r={3.5} />
        <circle className="gob-sawtooth-dot gob-sawtooth-dot--pro" cx={px} cy={yFor(pro)} r={3.5} />
        <text className="gob-sawtooth-axislabel" x={W - PAD_R - 4} y={H - 6} textAnchor="end">turns →</text>
      </svg>
      <div className="gob-sawtooth-legend">
        <span><i className="gob-sawtooth-swatch gob-sawtooth-swatch--pro" />provider: one late compaction</span>
        <span><i className="gob-sawtooth-swatch gob-sawtooth-swatch--gob" />gobstopper: every crossing, early</span>
      </div>
    </div>
  );
}

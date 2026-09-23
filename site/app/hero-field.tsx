"use client";

import { useEffect, useRef, type CSSProperties } from "react";

type FieldNote = {
  id: string;
  type: "session" | "snapshot" | "elision" | "digest" | "threshold" | "event" | "verify" | "provider" | "undo" | "strategy";
  title: string;
  body: string;
  x: number;
  y: number;
  rotate: number;
  width: number;
  drift: [number, number];
  seconds: number;
  delay: number;
  tags?: string[];
  bloom?: boolean;
};

const notes: FieldNote[] = [
  { id: "claude", type: "session", title: "claude session 034…", body: "Live, provider-served. ~333k context — native compaction left it alone.", x: 12, y: 24, rotate: 1.4, width: 184, drift: [10, 14], seconds: 38, delay: -21 },
  { id: "codex", type: "session", title: "codex session", body: "Idle. Its own JSONL dialect, parsed into the same record model.", x: 47, y: 10, rotate: -1.2, width: 178, drift: [11, 10], seconds: 45, delay: -14 },
  { id: "trigger", type: "threshold", title: "trigger 250k", body: "A plan stages below the line; a validated copy publishes on the cross.", x: 80, y: 13, rotate: 1.7, width: 182, drift: [13, 9], seconds: 44, delay: -30 },
  { id: "snapshot", type: "snapshot", title: "vault snap_7d3e", body: "Content-addressed. Taken before a single byte is rewritten.", x: 9, y: 56, rotate: -1.6, width: 186, drift: [9, 12], seconds: 41, delay: -8 },
  { id: "elide", type: "elision", title: "elide ×43", body: "Stale tool outputs masked in place — no record deleted, linkage intact.", x: 33, y: 40, rotate: -0.8, width: 196, drift: [10, 12], seconds: 40, delay: -5, bloom: true },
  { id: "digest", type: "digest", title: "digest injected", body: "A compact state card carries the conversation forward.", x: 61, y: 49, rotate: 0.9, width: 180, drift: [8, 13], seconds: 47, delay: -18 },
  { id: "verify", type: "verify", title: "verify clean", body: "Parent chains, ordinals, and structure checked after every rewrite.", x: 87, y: 60, rotate: -1.1, width: 190, drift: [14, 8], seconds: 39, delay: -2 },
  { id: "provider", type: "provider", title: "provider /compact", body: "On a running session the provider keeps authority — Gobstopper asks, never edits under it.", x: 91, y: 34, rotate: 0.6, width: 188, drift: [8, 11], seconds: 48, delay: -26 },
  { id: "undo", type: "undo", title: "$ gobstopper undo", body: "restores byte-identical bytes\ninto a fresh fork", x: 17, y: 80, rotate: -2, width: 198, drift: [12, 9], seconds: 35, delay: -15 },
  { id: "event", type: "event", title: "compaction-events-v1", body: "Every mutation emits one record: snapshot refs, edits, retention.", x: 48, y: 84, rotate: 1.5, width: 196, drift: [9, 11], seconds: 44, delay: -27 },
  { id: "strategy", type: "strategy", title: "strategy auto", body: "Picks by transcript shape — elide, structured, scored, or provider delegation.", x: 75, y: 87, rotate: -2.2, width: 196, drift: [8, 14], seconds: 40, delay: -11 },
];

const edges = [
  { from: "trigger", to: "claude", label: "watches", pulse: true },
  { from: "claude", to: "snapshot", label: "snapshots first" },
  { from: "snapshot", to: "elide", label: "guards" },
  { from: "elide", to: "digest", label: "carries state", pulse: true },
  { from: "digest", to: "verify", label: "checks" },
  { from: "provider", to: "claude", label: "delegates" },
  { from: "codex", to: "elide", label: "candidate" },
  { from: "digest", to: "event", label: "emits" },
  { from: "undo", to: "snapshot", label: "restores" },
  { from: "strategy", to: "elide", label: "selects", pulse: true },
  { from: "undo", to: "event", label: "replays" },
  { from: "verify", to: "provider", label: "returns to" },
];

const noteById = new Map(notes.map((note) => [note.id, note]));

function edgePath(from: FieldNote, to: FieldNote) {
  const midX = (from.x + to.x) / 2;
  const bend = Math.min(10, 0.5 * Math.abs(from.y - to.y) + 5);
  const topY = Math.min(from.y, to.y) - bend;
  return `M ${from.x} ${from.y} Q ${midX} ${topY} ${to.x} ${to.y}`;
}

function edgeMid(from: FieldNote, to: FieldNote) {
  const midX = (from.x + to.x) / 2;
  const bend = Math.min(10, 0.5 * Math.abs(from.y - to.y) + 5);
  const topY = Math.min(from.y, to.y) - bend;
  return [(from.x + 2 * midX + to.x) / 4, (from.y + 2 * topY + to.y) / 4] as const;
}

/**
 * Ambient compaction field behind the hero: drifting session/snapshot cards
 * and labeled edges. Pointer proximity sharpens each card through --prox;
 * when the pointer rests, a slow probe keeps wandering the reveal. A press
 * sends a burst of clarity out from the point.
 */
export function HeroField() {
  const field = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const root = field.current;
    if (root === null) return;
    const targets = Array.from(root.querySelectorAll<HTMLElement>("[data-prox]"));
    if (targets.length === 0) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;

    const centers = new Map<HTMLElement, [number, number]>();
    const measure = () => {
      centers.clear();
      for (const el of targets) {
        const rect = el.getBoundingClientRect();
        centers.set(el, [rect.left + rect.width / 2, rect.top + rect.height / 2]);
      }
    };
    measure();

    let pointerX = -1e4, pointerY = -1e4;
    let burstX = -1e4, burstY = -1e4, burst = 0;
    let lastPointerAt = -1e4;
    let raf = 0;

    const apply = () => {
      raf = 0;
      const now = performance.now();
      // Idle: a slow probe roams the field so the reveal never sits still.
      if (now - lastPointerAt > 3200) {
        const rect = root.getBoundingClientRect();
        const t = now / 1000;
        pointerX = rect.left + rect.width * (0.5 + 0.4 * Math.sin(t * 0.29) * Math.cos(t * 0.11));
        pointerY = rect.top + rect.height * (0.5 + 0.34 * Math.sin(t * 0.21 + 1.9) * Math.cos(t * 0.07));
        schedule();
      }
      if (burst > 0) {
        burst = Math.max(0, burst - 0.022);
        schedule();
      }
      for (const el of targets) {
        const c = centers.get(el);
        if (c === undefined) continue;
        const near = Math.max(0, 1 - Math.hypot(c[0] - pointerX, c[1] - pointerY) / 340);
        const ring = burst * Math.max(0, 1 - Math.hypot(c[0] - burstX, c[1] - burstY) / 520);
        el.style.setProperty("--prox", Math.min(1, near + ring).toFixed(3));
      }
    };
    const schedule = () => { if (raf === 0) raf = requestAnimationFrame(apply); };
    const onMove = (event: PointerEvent) => {
      pointerX = event.clientX;
      pointerY = event.clientY;
      lastPointerAt = performance.now();
      schedule();
    };
    const onDown = (event: PointerEvent) => {
      burstX = event.clientX;
      burstY = event.clientY;
      burst = 1;
      schedule();
    };
    const onLeave = () => {
      pointerX = -1e4;
      pointerY = -1e4;
      lastPointerAt = performance.now();
      schedule();
    };

    window.addEventListener("pointermove", onMove, { passive: true });
    window.addEventListener("pointerdown", onDown, { passive: true });
    window.addEventListener("scroll", measure, { capture: true, passive: true });
    window.addEventListener("resize", measure);
    document.documentElement.addEventListener("pointerleave", onLeave);
    window.addEventListener("blur", onLeave);
    schedule();

    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerdown", onDown);
      window.removeEventListener("scroll", measure, { capture: true });
      window.removeEventListener("resize", measure);
      document.documentElement.removeEventListener("pointerleave", onLeave);
      window.removeEventListener("blur", onLeave);
      if (raf !== 0) cancelAnimationFrame(raf);
    };
  }, []);

  return (
    <div aria-hidden="true" className="gob-field" ref={field}>
      <svg className="gob-edges" preserveAspectRatio="none" viewBox="0 0 100 100">
        {edges.map((edge) => {
          const from = noteById.get(edge.from);
          const to = noteById.get(edge.to);
          if (from === undefined || to === undefined) return null;
          return <path key={`${edge.from}-${edge.to}`} className={edge.pulse === true ? "gob-edge gob-edge--pulse" : "gob-edge"} d={edgePath(from, to)} data-prox="" />;
        })}
      </svg>
      {edges.map((edge) => {
        const from = noteById.get(edge.from);
        const to = noteById.get(edge.to);
        if (from === undefined || to === undefined) return null;
        const [x, y] = edgeMid(from, to);
        return <span key={`label-${edge.from}-${edge.to}`} className="gob-edge-label" data-prox="" style={{ left: `${x}%`, top: `${y}%` }}>{edge.label}</span>;
      })}
      {notes.map((note) => (
        <article
          key={note.id}
          className={note.bloom === true ? "gob-field-note gob-field-note--bloom" : "gob-field-note"}
          data-prox=""
          data-type={note.type}
          style={{
            "--x": `${note.x}%`, "--y": `${note.y}%`, "--w": `${note.width}px`, "--r": `${note.rotate}deg`,
            "--dx": `${note.drift[0]}px`, "--dy": `${note.drift[1]}px`, "--s": `${note.seconds}s`, "--d": `${note.delay}s`, "--bd": `${note.delay * 0.7}s`,
          } as CSSProperties}
        >
          <span className="gob-field-note-type">{note.type}</span>
          <h3 className="gob-field-note-title">{note.title}</h3>
          <p className="gob-field-note-body">{note.body}</p>
          {note.tags !== undefined && <div className="gob-field-note-tags">{note.tags.map((tag) => <span key={tag}>#{tag}</span>)}</div>}
        </article>
      ))}
    </div>
  );
}

import type { CSSProperties } from "react";

type FieldNote = {
  id: string;
  type: "session" | "snapshot" | "elision" | "digest" | "threshold" | "event" | "verify" | "provider" | "undo" | "strategy";
  title: string;
  body: string;
  x: number;
  y: number;
  rotate: number;
  width: number;
  tags?: string[];
};

const notes: FieldNote[] = [
  { id: "claude", type: "session", title: "claude session 034…", body: "Live, provider-served. ~333k context — native compaction left it alone.", x: 12, y: 24, rotate: 1.4, width: 184 },
  { id: "codex", type: "session", title: "codex session", body: "Idle. Its own JSONL dialect, parsed into the same record model.", x: 47, y: 10, rotate: -1.2, width: 178 },
  { id: "trigger", type: "threshold", title: "trigger 250k", body: "A plan stages below the line; a validated copy publishes on the cross.", x: 80, y: 13, rotate: 1.7, width: 182 },
  { id: "snapshot", type: "snapshot", title: "vault snap_7d3e", body: "Content-addressed. Taken before a single byte is rewritten.", x: 9, y: 56, rotate: -1.6, width: 186 },
  { id: "elide", type: "elision", title: "elide ×43", body: "Stale tool outputs masked in place — no record deleted, linkage intact.", x: 33, y: 40, rotate: -0.8, width: 196 },
  { id: "digest", type: "digest", title: "digest injected", body: "A compact state card carries the conversation forward.", x: 61, y: 49, rotate: 0.9, width: 180 },
  { id: "verify", type: "verify", title: "verify clean", body: "Parent chains, ordinals, and structure checked after every rewrite.", x: 87, y: 60, rotate: -1.1, width: 190 },
  { id: "provider", type: "provider", title: "provider /compact", body: "Native compaction delegates session changes to the provider through its own controls.", x: 91, y: 34, rotate: 0.6, width: 188 },
  { id: "undo", type: "undo", title: "$ gobstopper undo", body: "prepares archived content\nunder a fresh session identity", x: 17, y: 80, rotate: -2, width: 198 },
  { id: "event", type: "event", title: "compaction-events-v1", body: "Best-effort events: snapshot refs and available measurements.", x: 48, y: 84, rotate: 1.5, width: 196 },
  { id: "strategy", type: "strategy", title: "strategy auto", body: "Picks by transcript shape — elide, structured, scored, or provider delegation.", x: 75, y: 87, rotate: -2.2, width: 196 },
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

/** Product-owned routing artwork. The shared HeroBackdrop owns pointer light,
 * proximity, offscreen suspension, and reduced-motion behavior. */
export function HeroField() {
  return (
    <div aria-hidden="true" className="gob-field">
      <svg className="gob-edges" preserveAspectRatio="none" viewBox="0 0 100 100">
        {edges.map((edge) => {
          const from = noteById.get(edge.from);
          const to = noteById.get(edge.to);
          if (from === undefined || to === undefined) return null;
          return <path key={`${edge.from}-${edge.to}`} className={edge.pulse === true ? "gob-edge gob-edge--pulse" : "gob-edge"} d={edgePath(from, to)} data-hraness-hero-item="" />;
        })}
      </svg>
      {edges.map((edge) => {
        const from = noteById.get(edge.from);
        const to = noteById.get(edge.to);
        if (from === undefined || to === undefined) return null;
        const [x, y] = edgeMid(from, to);
        return <span key={`label-${edge.from}-${edge.to}`} className="gob-edge-label" data-hraness-hero-item="" style={{ left: `${x}%`, top: `${y}%` }}>{edge.label}</span>;
      })}
      {notes.map((note) => (
        <article
          key={note.id}
          className="gob-field-note"
          data-hraness-hero-item=""
          data-type={note.type}
          style={{
            "--x": `${note.x}%`, "--y": `${note.y}%`, "--w": `${note.width}px`, "--r": `${note.rotate}deg`,
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

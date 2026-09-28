import { ANATOMY } from "../_lib/gob-geometry";
import { GobFigure } from "./gob-figure";
import { GobRing } from "./gob-ring";

const RECENT_W = ANATOMY.blocks.filter((block) => block.kind === "recent").reduce((sum, block) => sum + block.w, 0);
const LABELLED = ANATOMY.blocks.flatMap((block) => ("label" in block ? [{ kind: block.kind, w: block.w, label: block.label }] : []));
const TOTAL = ANATOMY.blocks.reduce((sum, block) => sum + block.w, 0);

const ROW_TEXT: Record<string, string> = {
  Head: "Head: system prompt and your first message, word for word",
  Summary: "Summary",
  Carry: "Carry: earlier words and replies, up to 24,000 characters",
};

/** Rows for the key and the phone list: the three labelled blocks, then the last 3 turns as one. */
const ROWS = [
  ...LABELLED.map((block) => ({ kind: block.kind, w: block.w, text: ROW_TEXT[block.label] ?? block.label, recent: false })),
  { kind: "recent" as const, w: RECENT_W, text: "Last 3 turns, word for word", recent: true },
];

function share(fraction: number): string {
  return `${Math.round((fraction / ANATOMY.original) * 10000) / 100}%`;
}

/** D-anatomy: the rewritten request, block by block, against the original. */
export function GobAnatomy() {
  return (
    <GobFigure
      alt={`Diagram: a full-width bar labelled original request, and below it a shorter bar of six parts: head, summary, carry, and the last three turns. The rewrite is about ${Math.round(TOTAL * 100)}% of the original.`}
      caption="At its median in the benchmark, a rewrite was 57% of the original request's size (55K to 31.5K estimated tokens). The example lines are illustrative."
      id="anatomy"
      kind="diagram"
      title="Inside a rewritten request"
    >
      <div className="gob-anatomy">
        <p className="gob-anatomy__label">original request</p>
        <div className="gob-anatomy__ghost" />
        <p className="gob-anatomy__label">rewritten request</p>
        <div className="gob-anatomy__strip" style={{ width: share(TOTAL) }}>
          {ANATOMY.blocks.map((block, index) => (
            <span className="gob-anatomy__block" data-kind={block.kind} key={index} style={{ flexGrow: block.w, flexBasis: 0 }}>
              {block.kind === "summary" ? <GobRing /> : null}
            </span>
          ))}
        </div>
        <ul className="gob-anatomy__rows">
          {ROWS.map((row) => (
            <li data-kind={row.kind} key={row.text}>
              <span className="gob-anatomy__row-text">{row.text}</span>
              <span className="gob-anatomy__row-bar">
                <span className="gob-anatomy__block" data-kind={row.kind} data-recent={row.recent ? "" : undefined} style={{ width: share(row.w) }} />
              </span>
            </li>
          ))}
        </ul>
        <pre className="gob-anatomy__summary">
          <code>{"user: fix the failing test\n[Bash] cargo test -p app (output over 500 characters left out)\nassistant: the test passes now"}</code>
        </pre>
        <p className="gob-anatomy__foot">Tool results over 500 characters are left out. The files are still on disk, and the command can be rerun.</p>
      </div>
    </GobFigure>
  );
}

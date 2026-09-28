import { TAIL } from "../_lib/gob-geometry";
import { F } from "../_lib/gobbench-format";
import { GobFigure, P_TB_FULL } from "./gob-figure";
import { GobStacks } from "./gob-stack";

/** D-tail: two post-rewrite stacks under one lid; the old default leaves less room. */
export function GobTail() {
  return (
    <GobFigure
      alt="Diagram: two stacks after a rewrite. Tail 0 keeps your task, a summary and the last three turns. Tail 40 also keeps older turns, so it sits closer to the threshold and is rewritten again sooner."
      caption={`In the benchmark, tail 0 cost ${F.newVsOldAbs} less in total than tail 40 in provider-reported terms, at a 45,000-token threshold (paired 95% interval ${F.oldVsNewCI}). It is one trial per task, and five tasks drive the gap. v0.7.3 makes tail 0 the default.`}
      id="tail"
      kind="diagram"
      provenance={P_TB_FULL}
      title="Keeping more old turns meant more rewrites and more tokens"
    >
      <GobStacks
        columns={[
          { title: "Gobstopper, tail 0 (the new default)", slabs: TAIL.tail0 },
          {
            title: "Gobstopper, tail 40 (old default)",
            slabs: TAIL.tail40,
            note: <><span aria-hidden="true" className="gob-arrow">↓</span> less room left, so the next rewrite comes sooner</>,
          },
        ]}
        lidAt={TAIL.lidAt}
      />
    </GobFigure>
  );
}

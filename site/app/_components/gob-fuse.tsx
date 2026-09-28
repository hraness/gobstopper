import { FUSE } from "../_lib/gob-geometry";
import { GobFigure } from "./gob-figure";
import { GobStacks } from "./gob-stack";

/** D-fuse: past the threshold, the middle of the session becomes one summary. */
export function GobFuse() {
  return (
    <GobFigure
      alt="Diagram: a tall stack of nine blocks crosses a threshold line. Beside it, with Gobstopper, five blocks sit under the line: your task, one summary block, and the last three turns."
      caption="Gobstopper keeps the start and the last three turns word for word and replaces the middle with a mechanical summary. No model writes it. Your files and your saved session are not changed."
      id="fuse"
      kind="diagram"
      title="When a request passes the threshold, the middle becomes one summary"
    >
      <GobStacks
        columns={[
          { title: "Without Gobstopper", slabs: FUSE.before },
          { title: "With Gobstopper", slabs: FUSE.after },
        ]}
        lidAt={FUSE.lidAt}
      />
    </GobFigure>
  );
}

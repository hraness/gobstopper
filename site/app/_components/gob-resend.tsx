import { resendColumns, stackHeight } from "../_lib/gob-geometry";
import { GobFigure, GobLegend } from "./gob-figure";
import { GobStack, units } from "./gob-stack";

const COLUMNS = resendColumns(5);
const TALLEST = stackHeight(COLUMNS.at(-1)!.length);

/** D-resend: every step of a coding agent resends the whole session. */
export function GobResend() {
  return (
    <GobFigure
      alt="Diagram: five columns of stacked blocks, one per step. Each column is one block taller than the last, because every request resends all earlier steps."
      caption="A coding agent has no memory between steps, so each request carries your first message and every file and command output since. By step 40 it resends 39 steps it has already sent."
      id="resend"
      kind="diagram"
      legend={
        <GobLegend
          items={[
            { label: "your task", swatch: "slab", kind: "task" },
            { label: "one step of chat and tool output", swatch: "slab", kind: "turn" },
          ]}
        />
      }
      title="Every step resends the whole session"
    >
      <div className="gob-resend" style={{ height: units(TALLEST + 1.9) }}>
        <div className="gob-resend__cols">
          {COLUMNS.map((column, index) => (
            <div className="gob-resend__col" key={index}>
              <GobStack slabs={column.map(({ kind }) => ({ kind }))} />
            </div>
          ))}
        </div>
        <div className="gob-resend__brace" style={{ bottom: units(TALLEST + 0.35) }}>
          <span className="gob-resend__brace-label">everything, again</span>
        </div>
      </div>
      <div className="gob-resend__steps" aria-hidden="true">
        {COLUMNS.map((_, index) => <span key={index}>Step {index + 1}</span>)}
      </div>
      <p className="gob-resend__under">every stacked slab is sent, and billed, again</p>
    </GobFigure>
  );
}

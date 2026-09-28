import { F } from "../_lib/gobbench-format";
import { terminalBench } from "../_lib/gobbench-data";
import { GobFigure, GobTable, P_TB_FULL } from "./gob-figure";

const RUNS = [
  { kind: "all", count: terminalBench.churn.resolved_by_all, label: "solved by all three arms" },
  { kind: "split", count: terminalBench.churn.split, label: "changed between arms" },
  { kind: "none", count: terminalBench.churn.resolved_by_none, label: "solved by none" },
] as const;

/** C-churn: 89 cells in three runs; a third of tasks flip between single trials. */
export function GobChurn() {
  const total = RUNS.reduce((sum, run) => sum + run.count, 0);
  return (
    <GobFigure
      alt={`Bar of ${total} tasks: ${F.churn.all} solved by all three arms, ${F.churn.split} solved by some arms but not others, ${F.churn.none} solved by none.`}
      caption={`With one trial per arm, ${F.churn.split} of ${total} tasks were solved by some arms and not others. A single trial of a task can land either way, so a one- or two-task difference in solved counts is within single-trial noise.`}
      id="churn"
      kind="chart"
      provenance={P_TB_FULL}
      table={
        <GobTable
          caption={`Outcome of each of ${total} tasks across the three arms`}
          head={["Outcome", "Tasks"]}
          rows={RUNS.map((run) => [run.label, String(run.count)])}
        />
      }
      title={`${F.churn.split} of 89 tasks changed outcome between arms`}
    >
      <div className="gob-churn">
        <div className="gob-churn__bar">
          {RUNS.map((run) => (
            <div className="gob-churn__run" data-kind={run.kind} key={run.kind} style={{ flexGrow: run.count, flexBasis: 0 }}>
              <div className="gob-churn__cells">
                {Array.from({ length: run.count }, (_, index) => <span className="gob-churn__cell" key={index} />)}
              </div>
            </div>
          ))}
        </div>
        <ul className="gob-churn__key">
          {RUNS.map((run) => (
            <li key={run.kind}>
              <span aria-hidden="true" className="gob-churn__cell" data-kind={run.kind} />
              <strong>{run.count}</strong> {run.label}
            </li>
          ))}
        </ul>
      </div>
    </GobFigure>
  );
}

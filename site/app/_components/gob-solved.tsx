import { ARM_LABEL, ARM_ORDER, arm, pair, terminalBench, type ArmId } from "../_lib/gobbench-data";
import { F, wilson } from "../_lib/gobbench-format";
import { GobRules, GobXAxis, type Tick } from "./gob-axis";
import { GobArmName, GobFigure, GobTable, P_TB_FULL, at } from "./gob-figure";

const DOMAIN = [50, 80] as const;
const TICKS: readonly Tick[] = [50, 60, 70, 80].map((value) => ({ value, label: `${value}%` }));

function position(rate: number): string {
  return at((rate * 100 - DOMAIN[0]) / (DOMAIN[1] - DOMAIN[0]));
}

const ALT = `Dot plot of tasks solved out of 89 with 95% intervals: ${ARM_ORDER.map(
  (id) => `${ARM_LABEL[id]}, ${F.solved[id]} (${F.ci[id].replace("–", " to ")}%)`,
).join("; ")}. The intervals overlap almost completely.`;

function Row({ id }: { readonly id: ArmId }) {
  const entry = arm(id);
  const [lo, hi] = wilson(entry.resolved, entry.n_tasks);
  const rate = entry.resolved / entry.n_tasks;
  return (
    <div className="gob-dot-row" data-arm={id}>
      <p className="gob-dot-row__label"><GobArmName arm={id} /></p>
      <div className="gob-dot-row__track">
        <GobRules domain={DOMAIN} ticks={TICKS} />
        <span className="gob-interval" data-arm={id} style={{ left: position(lo), right: `calc(100% - ${position(hi)})` }} />
        <span aria-hidden="true" className="gob-marker gob-dot-row__dot" data-arm={id} style={{ left: position(rate) }} />
      </div>
      <p className="gob-dot-row__value">{F.solved[id]} of 89 · {F.ci[id]}%</p>
    </div>
  );
}

/** C-solved: share of tasks resolved per arm with Wilson intervals, on a 50–80% axis. */
export function GobSolved() {
  const paper = terminalBench.paper_reference.reported;
  const [cliff, native200, native45] = paper;
  return (
    <GobFigure
      alt={ALT}
      aside={
        <aside className="gob-paper" aria-label="The CliffCompaction paper's own numbers">
          <p className="gob-paper__label">The paper&apos;s own numbers, not run by us</p>
          <p>
            CliffCompaction&apos;s authors report {cliff!.resolved_pct}% for their proxy at 45K, {native200!.resolved_pct}% for
            Claude Code&apos;s 200K default and {native45!.resolved_pct}% for its 45K auto-compaction, on their own setup
            (<a href="https://arxiv.org/abs/2609.26779">arXiv:2609.26779</a>). Not comparable to the rows above.
          </p>
        </aside>
      }
      caption="Share of 89 tasks resolved, with Wilson 95% intervals. One trial per arm."
      id="solved"
      kind="chart"
      notes={
        <p className="gob-figure__note">
          The intervals overlap almost completely: no measurable difference (McNemar p = {pair("tail0", "no_proxy").mcnemar_exact_p.toFixed(1)} against
          no proxy). {F.churn.split} of 89 tasks changed outcome between arms, the noise level of one trial.
        </p>
      }
      provenance={P_TB_FULL}
      table={
        <GobTable
          caption="Tasks solved of 89, with Wilson 95% intervals"
          head={["Arm", "Solved of 89", "Rate", "95% interval"]}
          rows={ARM_ORDER.map((id) => [ARM_LABEL[id], F.solved[id], F.rate[id], `${F.ci[id]}%`])}
        />
      }
      title="Solved about as often"
    >
      <div className="gob-dots">
        {ARM_ORDER.map((id) => <Row id={id} key={id} />)}
        <div className="gob-dot-row gob-dot-row--axis">
          <span className="gob-dot-row__label" />
          <GobXAxis domain={DOMAIN} label="share of 89 tasks solved (axis starts at 50%)" ticks={TICKS} />
          <span className="gob-dot-row__value" />
        </div>
      </div>
    </GobFigure>
  );
}

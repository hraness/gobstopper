import { ARM_LABEL, ARM_ORDER, arm, type ArmId } from "../_lib/gobbench-data";
import { F } from "../_lib/gobbench-format";
import { GobRules, GobXAxis, type Tick } from "./gob-axis";
import { GobArmName, GobFigure, GobLegend, GobTable, P_TB, P_TB_FULL, at, spoken } from "./gob-figure";

const MAX = 120_000_000;
const DOMAIN = [0, MAX] as const;
const TICKS: readonly Tick[] = [
  { value: 0, label: "0" },
  { value: 40_000_000, label: "40M", show: "wide" },
  { value: 60_000_000, label: "60M", show: "narrow" },
  { value: 80_000_000, label: "80M", show: "wide" },
  { value: 120_000_000, label: "120M" },
];

function tokens(id: ArmId) {
  return arm(id).tokens_all_trials;
}

const ALT = `Bar chart of total input tokens over 89 Terminal-Bench tasks. ${ARM_ORDER.map(
  (id) => `${ARM_LABEL[id]}: ${spoken(F.input[id])}, ${F.solved[id]} solved.`,
).join(" ")} Cache reads make up most of each bar.`;

function Row({ id }: { readonly id: ArmId }) {
  const t = tokens(id);
  const total = t.cache_read + t.uncached_input;
  const share = t.total_input / MAX;
  return (
    <div className="gob-bar-row" data-arm={id}>
      <p className="gob-bar-row__label">
        <GobArmName arm={id} />
        <span className="gob-bar-row__meta"><span aria-hidden="true" className="gob-bar-row__sep">· </span>{F.solved[id]} of 89 solved</span>
      </p>
      <div className="gob-bar-row__track">
        <GobRules domain={DOMAIN} ticks={TICKS} />
        <span className="gob-bar" style={{ width: at(share) }}>
          <span className="gob-bar__seg" data-arm={id} data-part="cache" style={{ flexGrow: t.cache_read / total }} />
          <span className="gob-bar__seg" data-arm={id} data-part="new" style={{ flexGrow: t.uncached_input / total }} />
          <span className="gob-bar__value">{F.input[id]}</span>
        </span>
      </div>
    </div>
  );
}

/** C-tokens: total input per arm, cache reads and new input, from zero. No dollars on the homepage. */
export function GobTokens({ variant }: { readonly variant: "home" | "full" }) {
  const home = variant === "home";
  return (
    <GobFigure
      alt={ALT}
      caption={`Total input over 89 tasks. Almost all of the difference is cache reads, the context resent on every step: ${F.cache.tail0} vs ${F.cache.no_proxy}. New input and output were about equal.`}
      hero={home ? `${F.inputFewer} fewer input tokens` : undefined}
      id={home ? "tokens" : "tokens-full"}
      kind="chart"
      legend={
        <GobLegend
          items={[
            { label: "cache reads (context re-sent)", swatch: "fill" },
            { label: "new input (not cached)", swatch: "pale" },
          ]}
        />
      }
      notes={home ? undefined : (
        <p className="gob-figure__note">
          Provider-reported cost for this model: {F.perTask.tail0}, {F.perTask.no_proxy} and {F.perTask.tail40} per task.
          The {F.costLower} gap to no proxy is not statistically significant (95% interval {F.costLowerCI}).
        </p>
      )}
      provenance={home ? P_TB : P_TB_FULL}
      subtitle={home ? "Gobstopper at its default tail vs Claude Code, no proxy · same tasks solved within single-trial noise" : undefined}
      table={
        <GobTable
          caption="Total input tokens over 89 tasks, provider-reported"
          head={["Arm", "Solved of 89", "Total input", "Cache reads", "New input"]}
          rows={ARM_ORDER.map((id) => [ARM_LABEL[id], F.solved[id], F.input[id], F.cache[id], F.uncached[id]])}
        />
      }
      title={`Same tasks solved, ${F.inputFewer} fewer tokens sent`}
      titleHidden={home}
      variant={variant}
    >
      <div className="gob-bars">
        {ARM_ORDER.map((id) => <Row id={id} key={id} />)}
        <div className="gob-bar-row gob-bar-row--axis">
          <span className="gob-bar-row__label" />
          <GobXAxis domain={DOMAIN} label="input tokens, all 89 tasks" ticks={TICKS} />
        </div>
      </div>
    </GobFigure>
  );
}

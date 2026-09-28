import { areaPath, linePath, stepPath } from "../_lib/gob-geometry";
import { sawtooth } from "../_lib/gobbench-data";
import { F } from "../_lib/gobbench-format";
import type { Tick } from "./gob-axis";
import { GobFigure, GobLegend, GobTable, P_SAW, at, spoken } from "./gob-figure";
import { GobPlot } from "./gob-plot";
import { GobRing } from "./gob-ring";

const Y_MAX = 500_000;
const Y_TICKS: readonly Tick[] = [0, 100_000, 200_000, 300_000, 400_000, 500_000].map((value) => ({
  value,
  label: value === 0 ? "0" : `${value / 1000}K`,
  show: value === 0 || value === 500_000 ? undefined : "wide",
}));
const Y_TICKS_ALL: readonly Tick[] = [...Y_TICKS, { value: 250_000, label: "250K", show: "narrow" }];

const run = sawtooth.runs.find((entry) => entry.threshold === 45000 && entry.keep_tail_percent === 0)!;
const other = sawtooth.runs.find((entry) => entry.threshold === 128000 && entry.keep_tail_percent === 0)!;
const N = sawtooth.without_proxy.length;
const X_TICKS = [1, 100, 200, 300, N].map((request) => ({ value: request, label: String(request), at: (request - 1) / (N - 1) }));
const ZOOM_MAX = 50_000;
const ZOOM_TICKS: readonly Tick[] = [0, 25_000, 50_000].map((value) => ({ value, label: value === 0 ? "0" : `${value / 1000}K` }));
const peakIndex = sawtooth.without_proxy.indexOf(Math.max(...sawtooth.without_proxy));

const ANNOTATIONS = [
  `no proxy: up to ${F.saw.peak} per request`,
  `${F.saw.compactions} rewrites`,
  `total over the session: ${F.saw.cumFrom} to ${F.saw.cumTo} estimated tokens (${F.saw.cut} less)`,
] as const;

const HOME_CAPTION =
  "One recorded Claude Code session replayed at a 45,000-token threshold (the default is 128,000). The shaded areas are what each request resends; with Gobstopper the area is about a ninth as large.";

/**
 * The rewrite rings under a plot. The main plot gets all of them; the narrow zoomed
 * panel keeps every fifth, so 50 rewrites stay countable marks instead of a smear.
 */
function RingStrip({ where }: { readonly where: "main" | "zoom" }) {
  const marks = where === "main" ? run.compaction_requests : run.compaction_requests.filter((_, n) => n % 5 === 0);
  return (
    <div aria-hidden="true" className="gob-ring-strip" data-where={where}>
      <span className="gob-ring-strip__gutter">{where === "main" ? "rewrites" : ""}</span>
      <span className="gob-ring-strip__track">
        {marks.map((index) => (
          <span className="gob-ring-strip__mark" key={index} style={{ left: at(index / (N - 1)) }}>
            <GobRing />
          </span>
        ))}
      </span>
    </div>
  );
}

/** C-sawtooth: one real session, request size per request, with and without the proxy. */
export function GobSawtooth({ variant }: { readonly variant: "home" | "full" }) {
  const full = variant === "full";
  const withMax = Math.max(...run.with_proxy);
  const ceiling = Math.ceil(withMax / 10_000) * 10_000;
  return (
    <GobFigure
      alt={`Line chart of estimated tokens per request over ${F.saw.requests} requests of one session. Without the proxy, request size climbs steadily to about ${spoken(F.saw.peak)}. With Gobstopper at a 45,000-token threshold, it stays under ${ceiling.toLocaleString("en-US")} in a sawtooth, and total input falls from ${spoken(F.saw.cumFrom)} to ${spoken(F.saw.cumTo)} estimated tokens.`}
      caption={full ? `${HOME_CAPTION} At 128,000 the same session is rewritten ${other.compactions} times instead of ${run.compactions}.` : HOME_CAPTION}
      id={full ? "sawtooth-full" : "sawtooth"}
      kind="chart"
      legend={
        <GobLegend
          items={[
            { label: "Claude Code, no proxy", swatch: "line", arm: "no_proxy" },
            { label: "Gobstopper, tail 0", swatch: "line", arm: "tail0" },
            { label: "threshold: 45K set, lowered by calibration", swatch: "step" },
          ]}
        />
      }
      notes={
        <ol className="gob-annotations">
          {ANNOTATIONS.map((text, index) => <li data-n={index + 1} key={text}>{text}</li>)}
        </ol>
      }
      provenance={P_SAW}
      table={
        <GobTable
          caption="One recorded session, estimated tokens"
          head={["Measure", "Claude Code, no proxy", "Gobstopper, tail 0"]}
          rows={[
            ["Requests", F.saw.requests, F.saw.requests],
            ["Largest request", F.saw.peak, `${Math.round(withMax / 1000)}K`],
            ["Total over the session", F.saw.cumFrom, F.saw.cumTo],
            ["Rewrites", "0", F.saw.compactions],
          ]}
        />
      }
      title="A sawtooth, not a ramp"
      variant={variant}
    >
      <GobPlot
        overlay={
          <>
            <span className="gob-plot__note" data-n="1" style={{ right: at(1 - peakIndex / (N - 1)), bottom: `calc(${at(sawtooth.without_proxy[peakIndex]! / Y_MAX)} + 0.35rem)` }}>
              {ANNOTATIONS[0]}
            </span>
            <span className="gob-plot__note" data-n="2" style={{ left: "38%", bottom: `calc(${at(withMax / Y_MAX)} + 0.35rem)` }}>
              {ANNOTATIONS[1]}
            </span>
            <span className="gob-plot__note gob-plot__note--block" data-n="3" style={{ left: "1.5%", top: "4%" }}>
              {ANNOTATIONS[2]}
            </span>
          </>
        }
        xTicks={X_TICKS}
        yMax={Y_MAX}
        yTicks={Y_TICKS_ALL}
      >
        <path className="gob-wash" data-arm="no_proxy" d={areaPath(sawtooth.without_proxy, Y_MAX)} />
        <path className="gob-wash" data-arm="tail0" d={areaPath(run.with_proxy, Y_MAX)} />
        <path className="gob-line" data-arm="no_proxy" d={linePath(sawtooth.without_proxy, Y_MAX)} />
        <path className="gob-step" d={stepPath(run.applied_threshold_est_tokens, Y_MAX)} />
        <path className="gob-line" data-arm="tail0" d={linePath(run.with_proxy, Y_MAX)} />
      </GobPlot>
      <RingStrip where="main" />
      <p className="gob-plot__xlabel" aria-hidden="true">request number · estimated tokens per request</p>
      {/* Narrow figures squash the 45K band into a few pixels; a zoomed panel shows the sawtooth itself. */}
      <div aria-hidden="true" className="gob-saw-zoom">
        <p className="gob-saw-zoom__title">
          Gobstopper, tail 0, zoomed to 0–50K · <GobRing /> every fifth rewrite
        </p>
        <GobPlot xTicks={X_TICKS} yMax={ZOOM_MAX} yTicks={ZOOM_TICKS}>
          <path className="gob-wash" data-arm="tail0" d={areaPath(run.with_proxy, ZOOM_MAX)} />
          <path className="gob-step" d={stepPath(run.applied_threshold_est_tokens, ZOOM_MAX)} />
          <path className="gob-line" data-arm="tail0" d={linePath(run.with_proxy, ZOOM_MAX)} />
        </GobPlot>
        <RingStrip where="zoom" />
      </div>
    </GobFigure>
  );
}

import { MarketingDataTable } from "@hraness/design-kit/react/server";

import { ARM_LABEL } from "../_lib/gobbench-data";
import { F } from "../_lib/gobbench-format";

/** Terminal-Bench 2.1 setup, kept beside the result as the kit asks. */
export const TB_META = "89 tasks · one trial per arm · Claude Code 2.1.283 with GLM 5.3 Flash via Vercel AI Gateway · Gobstopper v0.7.2 at a 45,000-token threshold · September 27–28, 2026";

/** The home page's benchmark figure: tokens and tasks solved, Gobstopper against Claude Code's built-in compaction. */
export function GobBenchTable() {
  return (
    <MarketingDataTable
      caption="Terminal-Bench 2.1, with and without Gobstopper"
      columns={[
        { label: "Setup" },
        { label: "Tasks solved of 89", numeric: true },
        { label: "Input tokens", numeric: true },
        { label: "Cache reads", numeric: true },
        { label: "New input", numeric: true },
      ]}
      meta={TB_META}
      rows={[
        [ARM_LABEL.tail0, F.solved.tail0, { content: F.input.tail0, tone: "positive" }, F.cache.tail0, F.uncached.tail0],
        [ARM_LABEL.no_proxy, F.solved.no_proxy, F.input.no_proxy, F.cache.no_proxy, F.uncached.no_proxy],
      ]}
    />
  );
}

import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { renderToStaticMarkup } from "react-dom/server";

import { IntroducingBody } from "../app/blog/introducing/body";
import {
  GobElideShowcase,
  GobInstall,
  GobMeter,
  GobMeterShowcase,
  GobProxyStart,
  GobVaultSteps,
} from "../app/_mockups/gob-mockups";
import { cli, elide, SNAPSHOT } from "../app/_mockups/fixtures";
import { COMPACTION_INDEXES, METER_POINTS, METER_RUN, meterSeries } from "../app/_mockups/meter-data";
import { launchBeats } from "../app/launch/beats";
import { launchFacts } from "../app/launch/facts";

const site = join(import.meta.dir, "..");

test("the meter replays the published series exactly", async () => {
  const series = JSON.parse(await readFile(join(site, "public/benchmarks/2026-09-28/sawtooth-series.json"), "utf8")) as {
    without_proxy: number[];
    runs: { label: string; with_proxy: number[]; compaction_requests: number[]; applied_threshold_est_tokens: number[]; threshold: number }[];
  };
  const run = series.runs.find((entry) => entry.label === METER_RUN)!;
  expect(run.threshold).toBe(Number(launchFacts.threshold.value.replaceAll(",", "")));
  expect(meterSeries.map((row) => row.without)).toEqual(series.without_proxy);
  expect(meterSeries.map((row) => row.with)).toEqual(run.with_proxy);
  expect(meterSeries.map((row) => row.threshold)).toEqual(run.applied_threshold_est_tokens);
  expect(COMPACTION_INDEXES).toEqual(run.compaction_requests);
  for (const point of METER_POINTS) expect(point.request).toBeLessThanOrEqual(meterSeries.length);
});

test("the CLI fixtures agree with each other", async () => {
  const generator = await readFile(join(site, "app/_mockups/fixture-session/generate.py"), "utf8");
  expect(generator).toContain(JSON.stringify(elide.prompt));
  expect(generator).toContain(JSON.stringify(elide.finish));
  expect([...cli.apply.slice(0, cli.plan.length)] as string[]).toEqual([...cli.plan]);
  expect(cli.apply.join("\n")).toContain(`recovery snapshot: ${SNAPSHOT}`);
  expect(cli.search.join("\n")).toContain(`"snapshot_sha256": "${SNAPSHOT}"`);
  expect(cli.undo[0]).toContain(SNAPSHOT.slice(0, 16));
  expect(cli.plan.join("\n")).toContain(`eliding ${elide.elided} of ${elide.total} stale tool outputs`);
  for (const row of elide.rows) {
    if (row.after !== null) expect(row.after).toBe(`[output elided by gobstopper: ${row.bytes} bytes]`);
  }
  const start = cli.proxyRun[0]!;
  expect(start).toContain(`threshold ${launchFacts.threshold.value.replaceAll(",", "")} tokens`);
  expect(start).toContain("keep_recent 3");
});

test("every mockup has an accessible name and keeps sample account paths private", () => {
  const surfaces = [
    <GobMeter key="m" mode="off" />,
    <GobMeterShowcase key="s" />,
    <GobElideShowcase key="e" />,
    <GobVaultSteps key="v" />,
    <GobProxyStart key="p" />,
    <GobInstall key="i" />,
  ];
  for (const surface of surfaces) {
    const html = renderToStaticMarkup(surface);
    const names: string[] = [];
    new HTMLRewriter().on('[role="img"]', { element(element) {
      names.push(element.getAttribute("aria-label") ?? "");
    } }).transform(html);
    expect(names.length).toBeGreaterThan(0);
    for (const name of names) expect(name.trim().length).toBeGreaterThan(0);
    expect(html).not.toMatch(/\/Users\/(?!sam\b)[a-z]+/u);
  }
});

test("the launch post shows one visual per beat, in order", () => {
  const html = renderToStaticMarkup(<IntroducingBody />);
  let at = -1;
  for (const beat of launchBeats) {
    const next = html.indexOf(`id="beat-${beat.id}"`);
    expect(next).toBeGreaterThan(at);
    at = next;
  }
  expect(html.split("<figure").length - 1).toBeGreaterThanOrEqual(launchBeats.length);
});

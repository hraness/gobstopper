import { expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

import { assertLaunchKit } from "@hraness/design-kit/launch";

import { launchBeats, launchKitOptions, socialKit } from "../app/launch/beats";
import { LAUNCH_STATUS, launchFacts } from "../app/launch/facts";
import { renderSocialKitMarkdown } from "../app/launch/social-kit-markdown";
import { publishedRelease } from "../app/publication";

const root = join(import.meta.dir, "../..");
const read = (path: string) => readFile(join(root, path), "utf8");

// Tests pin facts to their records, not the prose around them.

test("the launch kit passes the shared checks", () => {
  expect(() => assertLaunchKit(launchBeats, socialKit, launchKitOptions)).not.toThrow();
});

test("the status comes from the release record", () => {
  expect(String(LAUNCH_STATUS)).toBe(`Latest release: v${publishedRelease!.version}`);
  const status = launchBeats.find((beat) => beat.id === "status")!;
  expect(status.post).toContain(LAUNCH_STATUS);
});

test("request defaults match the proxy source", async () => {
  const source = await read("crates/gobstopper-adapters/src/request/mod.rs");
  const threshold = /pub const DEFAULT_THRESHOLD_TOKENS: u64 = ([0-9_]+);/u.exec(source)![1]!.replaceAll("_", "");
  expect(launchFacts.threshold.value.replaceAll(",", "")).toBe(threshold);
  expect(source).toMatch(/threshold_tokens: DEFAULT_THRESHOLD_TOKENS,\s*keep_recent: 3,/u);
  expect(launchFacts.keepRecent.value).toBe("three");
});

test("benchmark facts match the published results", async () => {
  const results = JSON.parse(await read("site/public/benchmarks/2026-09-28/terminal-bench-results.json")) as {
    arms: { arm: string; n_tasks: number; resolved: number }[];
  };
  const arm = (id: string) => results.arms.find((entry) => entry.arm === id)!;
  expect(launchFacts.benchTasks.value).toBe(String(arm("tail0").n_tasks));
  expect(launchFacts.benchSolvedWith.value).toBe(String(arm("tail0").resolved));
  expect(launchFacts.benchSolvedWithout.value).toBe(String(arm("no_proxy").resolved));
  const series = JSON.parse(await read("site/public/benchmarks/2026-09-28/sawtooth-series.json")) as { runs: { label: string; threshold: number }[] };
  expect(series.runs.find((run) => run.label === "Gobstopper, tail 0, 45K threshold")!.threshold).toBe(45_000);
  expect(launchFacts.benchThreshold.value).toBe("45,000");
});

test("kb/launch/social-kit.md is generated from the beats", async () => {
  expect(await read("kb/launch/social-kit.md")).toBe(renderSocialKitMarkdown());
});

test("no social channel outside the kit", async () => {
  const kit = await read("kb/launch/social-kit.md");
  expect(kit).not.toMatch(/mastodon/iu);
});

/**
 * Gobstopper's launch film: long sessions resend old output, the reveal, what
 * the proxy keeps and trims, one recorded benchmark, the vault, and the
 * agent-install end card. Numbers come from site/app/launch/facts.ts and the
 * benchmark formatter the homepage uses.
 */
import { join } from "node:path";

import { launchFacts } from "../../site/app/launch/facts.ts";
import { F } from "../../site/app/_lib/gobbench-format.ts";
import { arm } from "../../site/app/_lib/gobbench-data.ts";
import { publishedRelease } from "../../site/app/publication.ts";
import { defineStory } from "./story.ts";

const repo = join(import.meta.dir, "../..");
const tasks = arm("tail0").n_tasks;

export default () => defineStory({
  id: "gobstopper",
  brand: {
    wordmark: "Gobstopper",
    mark: join(repo, "site/public/marks/gobstopper.svg"),
    markAspect: 1,
    // Read with site-palette.ts from https://gobstopper.sh in dark mode, 2026-10-04 (tokyo-night palette).
    palette: { values: {
      background: "#1a1b26", foreground: "#c0caf5", muted: "#a9b1d6", surface: "#16161e",
      surfaceRaised: "#24283b", primary: "#7aa2f7", primarySoft: "#262b3f", primaryForeground: "#1a1b26",
    } },
    designKit: join(repo, "site/node_modules/@hraness/design-kit"),
  },
  acts: [
    {
      kind: "terminal", headline: "On every step, your coding agent resends the whole session.", accents: ["whole", "session."],
      title: "Your agent's requests",
      // The worked example from the introduction: request N resends the N - 1 steps before it.
      lines: [-2, -1, 0].map((offset) => {
        const step = Number(launchFacts.stepNow.value) + offset;
        return { out: `request ${step}  resends ${step - 1} earlier steps`, tone: offset === 0 ? "accent" as const : "muted" as const };
      }),
      seconds: 4.6,
    },
    {
      kind: "scatter", headline: "That includes output that mattered once.", accents: ["mattered", "once."],
      cards: [
        { app: "Test log", glyph: "T", color: "#9ece6a", lines: ["From a run that", "has since passed"] },
        { app: "File listing", glyph: "F", color: "#e0af68", lines: ["From before", "a refactor"] },
        { app: "Stack trace", glyph: "S", color: "#f7768e", lines: ["For a bug", "already fixed"] },
      ],
    },
    { kind: "reveal", tagline: "Context compaction you can undo." },
    {
      kind: "before-after", headline: "Gobstopper trims the old printouts before they reach the model.", accents: ["trims"],
      before: { label: "Old tool output", items: ["Test log from a passing run", "File listing before the refactor", "Stack trace for a fixed bug"] },
      after: {
        label: "What the model gets", title: "The session, smaller",
        rows: [{ label: "Kept", value: "The start" }, { label: "Kept", value: `The last ${launchFacts.keepRecent.value} steps` }],
        note: `Compaction starts at ${launchFacts.threshold.value} tokens.`,
      },
      frame: "A proxy on your computer",
    },
    {
      kind: "stats", headline: "In one recorded benchmark, it solved about as many tasks and sent fewer tokens.", accents: ["fewer", "tokens."],
      items: [
        { value: F.solved.tail0, label: `of ${tasks} tasks solved with Gobstopper` },
        { value: F.solved.no_proxy, label: `of ${tasks} without it` },
        { value: F.inputFewer, label: "fewer input tokens sent" },
      ],
      note: `Terminal-Bench 2.1, one trial per arm, one model, ${launchFacts.benchThreshold.value}-token threshold.`,
    },
    {
      kind: "cards", headline: "Your session files are never edited.", accents: ["never"],
      items: [
        { tag: "Vault", title: "The original bytes are stored on your machine first" },
        { tag: "Restore", title: "Search for what was left out, or restore the whole session" },
        { tag: "No model", title: "No model writes the summary" },
      ],
    },
  ],
  end: {
    lead: "Ask your agent:", prompt: "Install Gobstopper from gobstopper.sh",
    terms: `Free and open source · Latest release v${publishedRelease!.version}`, url: "gobstopper.sh",
  },
  formats: ["wide", "square", "portrait"],
});

import {
  assertLaunchKit,
  buildSocialKit,
  resolveLaunchBeats,
  type LaunchBeat,
  type LaunchKitOptions,
  type LaunchMessaging,
  type LaunchRelease,
  type SocialKit,
} from "@hraness/design-kit/launch";
import { product } from "@hraness/design-kit/portfolio";

import { SITE_ORIGIN } from "../_lib/site";
import { LAUNCH_STATUS, launchFacts } from "./facts";

/** Where the diagrams the companion post already ships live. */
const FIGURES = "/blog/introducing-gobstopper";

/** The technical companion: mechanism, benchmark setup, intervals, and limits. */
export const COMPANION_PATH = "/blog/gobstopper-on-terminal-bench";

/**
 * The beats of "Introducing Gobstopper". Each one is a short section on the
 * page and one post in the launch threads, so each must read on its own.
 * Numbers are {placeholders} filled from ./facts; the design kit rejects a
 * beat that types a digit, apart from the names in NUMERAL_NAMES.
 */
const authoredBeats: readonly LaunchBeat[] = [
  {
    id: "what",
    part: "what",
    headline: "Gobstopper saves tokens while preserving context",
    post: "Gobstopper replaces the compaction built into Claude Code and Codex. It proxies API requests, compacts more often, and keeps the full history in a local database your agents can search. Free and open source.",
    visual: { kind: "diagram", src: `${FIGURES}/gob-fuse.png` },
    alt: "A tall stack of blocks crosses a threshold line; with Gobstopper, the task, a summary and recent turns fit below.",
  },
  {
    id: "resend",
    part: "does",
    headline: "Earlier context travels with later requests",
    post: "A coding agent carries earlier context into later model requests: test logs, file listings, and stack traces for bugs you already fixed. Without trimming, that repeated context grows as the session continues.",
    visual: { kind: "diagram", src: `${FIGURES}/gob-resend.png` },
    alt: "Columns of stacked blocks show earlier context accumulating in later requests before compaction.",

  },
  {
    id: "proxy",
    part: "does",
    headline: "Put it between your agent and the model",
    post: "Run your agent through gobstopper proxy. Past {threshold} tokens, it keeps your task and the last {keepRecent} turns word for word and summarizes the older turns, which squeezes a long conversation into the context window. Session files stay unchanged.",
    visual: { kind: "mockup", id: "meter", state: { mode: "on" } },
    alt: "Request size in one recorded session, as an illustration: it climbs to a threshold and drops back each time it is summarized.",
    facts: ["threshold", "keepRecent"],
    detailHref: `${COMPANION_PATH}#how-it-keeps-the-cache-warm`,
  },
  {
    id: "elide",
    part: "does",
    headline: "Shrink a saved session, and see the cut first",
    post: "Got a saved Claude Code or Codex session that grew too big? Preview what would go, then write a smaller copy. Old tool output becomes a one-line note that says how big it was. Your messages stay word for word in the smaller copy.",
    visual: { kind: "mockup", id: "elide", state: { mode: "copy" } },
    alt: "A saved session in an illustration with a made-up project: old tool outputs become one-line notes, recent ones stay whole.",
  },
  {
    id: "undo",
    part: "does",
    headline: "The full history stays in a local database",
    post: "Gobstopper writes the full history to a database on your machine. You and your agents can search it for a detail a summary left out, or restore the original into a separate session copy.",
    visual: { kind: "mockup", id: "vault", state: {} },
    alt: "Terminal steps in an illustration: find a session, preview a cut, write a copy, search the saved original, and undo.",
    detailHref: "/docs#install--use",
  },
  {
    id: "local",
    part: "how",
    headline: "It runs on your machine, with no extra model call",
    post: "The proxy runs locally and builds the summary with fixed rules, not by asking a model. It needs no Gobstopper account and leaves the session files on your disk unchanged.",
    visual: { kind: "mockup", id: "proxy", state: {} },
    alt: "A terminal in an illustration: gobstopper proxy starts with its default threshold and launches the coding agent through it.",
  },
  {
    id: "bench",
    part: "who",
    headline: "Tested on real terminal tasks",
    post: "On Terminal-Bench 2.1, Claude Code through the proxy used {benchInputFewer} fewer input tokens than with its built-in compaction, and solved {benchSolvedWith} of {benchTasks} tasks, against {benchSolvedWithout}. Built for people who run long agent sessions every day.",
    visual: { kind: "diagram", src: `${FIGURES}/gob-tokens.png` },
    alt: "Bar chart of total input tokens on Terminal-Bench with and without Gobstopper, mostly cache reads.",
    socialPost: "On Terminal-Bench 2.1, Claude Code through the proxy with a {benchThreshold}-token threshold used {benchInputFewer} fewer input tokens than with its built-in compaction, and solved {benchSolvedWith} of {benchTasks} tasks, against {benchSolvedWithout}. Built for people who run long agent sessions every day.",
    facts: ["benchSolvedWith", "benchTasks", "benchSolvedWithout", "benchInputFewer", "benchThreshold"],
    detailHref: `${COMPANION_PATH}#what-terminal-bench-showed`,
  },
  {
    id: "vision",
    part: "vision",
    headline: "Long sessions should not cost more with every step",
    post: "The goal: an agent that can work for hours without dragging its whole history along, where cutting context is always something you can check and take back.",
    visual: { kind: "mockup", id: "meter", state: { mode: "off" } },
    alt: "The same recorded session without a proxy, as an illustration: the request size only ever grows.",
  },
  {
    id: "limits",
    part: "limits",
    headline: "Read the result with its setup",
    post: "That was one trial per setup with a non-Anthropic model, so the gap in tasks solved is within noise. It used a {benchThreshold}-token threshold, not the {threshold} default, and most tokens saved were cached rereads.",
    visual: { kind: "diagram", src: `${FIGURES}/gob-solved.png` },
    alt: "Dot plot of tasks solved with wide, overlapping intervals for Gobstopper and for Claude Code with no proxy.",
    facts: ["benchThreshold", "threshold"],
    detailHref: `${COMPANION_PATH}#what-terminal-bench-showed`,
  },
  {
    id: "status",
    part: "status",
    headline: "Install Gobstopper",
    post: "Free and open source, with install scripts for Apple silicon Macs, Linux and Windows. Ask your agent to install Gobstopper from gobstopper.sh. On Windows the proxy works; the saved-session tools need a Mac or Linux.",
    facts: ["status"],
    socialPost: "{status}. Free and open source, with install scripts for Apple silicon Macs, Linux and Windows.",
    visual: { kind: "mockup", id: "install", state: {} },
    alt: "The one-line install script in an illustration, then gobstopper --version printing the current release.",

  },
];

/** Product names that contain digits and are not launch figures. */
export const NUMERAL_NAMES: readonly string[] = ["Terminal-Bench 2.1"];

export const launchBeats: readonly LaunchBeat[] = resolveLaunchBeats(authoredBeats, launchFacts, {
  allowNumerals: NUMERAL_NAMES,
});

export const LAUNCH_POST_PATH = "/blog/introducing-gobstopper";
export const LAUNCH_POST_URL = `${SITE_ORIGIN}${LAUNCH_POST_PATH}`;

const registry = product("gobstopper").messaging;

/** The product's messaging record from the portfolio registry. */
export const launchMessaging: LaunchMessaging = {
  names: { name: registry.names.name },
  tagline: registry.tagline,
  meta: registry.meta,
};

export const launchRelease: LaunchRelease = {
  status: LAUNCH_STATUS,
  tags: ["Developer Tools", "Open Source", "Artificial Intelligence"],
};

/** The release is public: the install scripts and tagged binaries are live. */
export const launchKitOptions: LaunchKitOptions = {
  status: LAUNCH_STATUS,
  publicInstall: true,
  tagline: launchMessaging.tagline,
  canonicalUrl: LAUNCH_POST_URL,
  // Comparisons live on /compare pages, not in social posts.
  forbiddenNames: ["CliffCompaction", "LLMLingua", "Headroom"],
};

export const socialKit: SocialKit = buildSocialKit(launchBeats, launchMessaging, launchRelease, LAUNCH_POST_URL);
assertLaunchKit(launchBeats, socialKit, launchKitOptions);

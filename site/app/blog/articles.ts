import {
  articleProvenanceFromAdmission,
  isArticleIndexable,
  type ArticleAdmission,
  type ArticleIsoDate,
  type ArticleSourceRecord,
} from "@hraness/design-kit";

import { gobFilm, type GobFilm } from "../_data/gob-film";
import { gobFilmHtml } from "../_lib/gobbench-film-html";
import { publishedRelease } from "../publication";
import { FILM_TOKEN } from "./film-token";
import { introducingHtml } from "./introducing/html";
import { postBodies } from "./posts.generated";

export const BLOG_PATH = "/blog";
export const BLOG_FEED_PATH = "/blog/feed.xml";
export const BLOG_TITLE = "Gobstopper blog";
export const BLOG_DESCRIPTION =
  "Understand context compaction, interpret token measurements, and explore the checks behind Gobstopper’s transcript edits and local archive.";

const REVIEWER = "Codex editorial review (AI)";
const REVIEWED_ON: ArticleIsoDate = "2026-09-30";
const REASSESS_ON: ArticleIsoDate = "2026-11-10";
// The introduction was rewritten around the Terminal-Bench study and reviewed again.
const INTRO_REVIEWED_ON: ArticleIsoDate = "2026-09-30";

function repo(path: string, name = "gobstopper"): string {
  return `https://github.com/hraness/${name}/blob/main/${path}`;
}

function source(title: string, url: string, checkedOn: ArticleIsoDate = "2026-09-24"): ArticleSourceRecord {
  return { title, url, checkedOn };
}

/** A post written in Markdown under content/blog. */
export type MarkdownSlug = keyof typeof postBodies;
/** The launch post, built from the beats in app/launch/beats.ts. */
export const INTRODUCING_SLUG = "introducing-gobstopper";
export type PostSlug = MarkdownSlug | typeof INTRODUCING_SLUG;

export type BlogPost = Readonly<{
  slug: PostSlug;
  /** "beats" renders the launch beats and their mockups; "markdown" renders content/blog/<slug>.md. */
  body: "beats" | "markdown";
  title: string;
  dek: string;
  /** The dek shortened to fit two lines on the share card. */
  shareLine: string;
  /** A shorter card headline when the title does not fit two lines at the share card's standard size. */
  shareHeadline?: string;
  eyebrow: string;
  published: ArticleIsoDate;
  updated?: ArticleIsoDate;
  keywords: readonly string[];
  admission: ArticleAdmission;
}>;

/** The launch beats were written, and their sources rechecked, on this date. */
const BEATS_REVIEWED_ON: ArticleIsoDate = "2026-09-30";

const introducing: BlogPost = {
  slug: INTRODUCING_SLUG,
  body: "beats",
  title: "Introducing Gobstopper",
  dek: "Gobstopper is a free, open-source tool that keeps long Claude Code and Codex sessions small, and keeps the original so you can get any of it back.",
  shareLine: "Keeps long Claude Code and Codex sessions small, and keeps the original.",
  eyebrow: "Release",
  published: "2026-09-24",
  updated: "2026-09-30",
  keywords: ["Gobstopper", "context compaction", "coding agents", "Claude Code", "Codex", "token usage", "Terminal-Bench"],
  admission: {
    href: "/blog/introducing-gobstopper",
    lifecycle: "indexable",
    readerJob: "Decide whether to try Gobstopper, in the time it takes to read a short thread.",
    nonObviousAnswer: "Earlier context accumulates in model requests. The proxy summarizes older turns while retaining the recent work; saved-session edits use separate copies with the original archived for recovery.",
    originalContribution: "The launch announcement as ten short beats that each make one claim and show one visual: a diagram from the benchmark, or a code-built illustration driven by real gobstopper output and one recorded session replayed offline. Every number comes from the launch facts module; the depth lives in the Terminal-Bench companion post each beat links to.",
    hostFit: "Gobstopper is the product this post introduces, published on its own site.",
    nearestUrls: [
      { url: "/blog/gobstopper-on-terminal-bench", distinction: "That post reads the benchmark, the mechanism, and the limits in full; this one says what the product does in ten beats and links there." },
      { url: "/", distinction: "The homepage is the install page and feature list; this post is the announcement, written to be cut into social posts." },
      { url: "/docs", distinction: "The docs are the full command reference; this post shows only the steps a first reader needs." },
    ],
    sources: [
      source("Launch facts and the record each comes from", repo("site/app/launch/facts.ts"), BEATS_REVIEWED_ON),
      source("Terminal-Bench 2.1 study, September 27 and 28, 2026", "https://gobstopper.sh/benchmarks#terminal-bench-2026-09-28", BEATS_REVIEWED_ON),
      source("Terminal-Bench aggregate results (JSON)", "https://gobstopper.sh/benchmarks/2026-09-28/terminal-bench-results.json", BEATS_REVIEWED_ON),
      source("Gobstopper README: proxy, saved sessions, platforms, install", repo("README.md"), BEATS_REVIEWED_ON),
      source("gobstopper proxy reference: head, summary, and recent turns", repo("docs/proxy.md"), BEATS_REVIEWED_ON),
      source("Mockup fixtures: real CLI output on a synthetic session", repo("site/app/_mockups/fixtures.ts"), BEATS_REVIEWED_ON),
      source("Default elision stub text", repo("crates/gobstopper-core/src/strategy/elide.rs"), BEATS_REVIEWED_ON),
      source("Gobstopper public writing rules", repo("STYLE.md"), BEATS_REVIEWED_ON),
    ],
    observations: [
      "The copy now distinguishes requests sent through the proxy from the separate saved-session edit/archive workflow.",
      "The repeated-context explanation avoids claiming the agent has no state, and the install beat presents the action without a release-status label."
    ],
    scores: {"readerUtility": 2, "originalEvidence": 1, "factualConfidence": 2, "hostFit": 2, "voiceIntegrity": 2, "maintenanceValue": 2},
    owner: "Hraness",
    drafting: "ai-from-source",
    review: { reviewer: REVIEWER, reviewerType: "ai", reviewedOn: REVIEWED_ON },
    humanReview: null,
    reassessOn: REASSESS_ON,
    harmIfWrong: "A reader could take one single-trial run as proof that the proxy never costs tasks, or assume the benchmark's 45,000-token threshold is the default.",
    refreshTriggers: [
      "A new gobstopper release tag (the status beat reads the release record)",
      "A default threshold or kept-turns change (the proxy beat reads the facts module)",
      "A multi-trial or Anthropic-model Terminal-Bench run",
      "A platform change for the proxy or the saved-session tools",
      "Change to the CLI output shown in the mockups: detect, plan, apply, search-snapshot, undo, proxy run",
      "The launch film or its cuts changing",
    ],
  },
};

const terminalBench: BlogPost = {
  slug: "gobstopper-on-terminal-bench",
  body: "markdown",
  title: "What a compaction benchmark can tell you",
  dek: "Gobstopper’s Terminal-Bench study shows why input tokens, task completion, and cost need separate interpretation when you evaluate compaction.",
  shareLine: "Read token counts, task completion, and cost separately.",
  shareHeadline: "Terminal-Bench 2.1",
  eyebrow: "Benchmark",
  published: "2026-09-24",
  updated: "2026-09-30",
  keywords: ["context compaction", "coding agents", "Claude Code", "Terminal-Bench", "prompt caching", "CliffCompaction"],
  admission: {
    href: "/blog/gobstopper-on-terminal-bench",
    lifecycle: "indexable",
    readerJob: "Decide whether to put gobstopper proxy between a coding agent and its provider, knowing what one Terminal-Bench run measured, what it did not, and how to try it.",
    nonObviousAnswer: "The tool's own default lost its benchmark, so the default changed: keeping more recent context verbatim cost more tokens on forward-moving tasks, and the token cut against no proxy is almost entirely cache reads, not new input.",
    originalContribution: "The first live task-success and token measurement of the Rust port of CliffCompaction's summary rule, with three arms (tail 0, tail 40, no proxy) on 89 Terminal-Bench 2.1 tasks, published aggregates, intervals, per-task churn, and the tasks that drive the cost gap.",
    hostFit: "The product's first benchmark and the mechanism behind it, on its own host.",
    nearestUrls: [
      { url: "/blog/introducing-gobstopper", distinction: "The introduction says what Gobstopper does in ten short beats; this post is the benchmark, the mechanism, and the limits behind them." },
      { url: "/benchmarks", distinction: "The benchmarks page holds every study's setup, statistics, and downloads; this post interprets the Terminal-Bench run and why it changed the default." },
      { url: "/docs", distinction: "The docs are the full command reference; this post walks the proxy and the vault at the level a first reader needs." },
    ],
    sources: [
      source("Terminal-Bench 2.1 study, September 27 and 28, 2026", "https://gobstopper.sh/benchmarks#terminal-bench-2026-09-28", INTRO_REVIEWED_ON),
      source("Terminal-Bench aggregate results (JSON)", "https://gobstopper.sh/benchmarks/2026-09-28/terminal-bench-results.json", INTRO_REVIEWED_ON),
      source("Replay grid over 24 recorded sessions (JSON)", "https://gobstopper.sh/benchmarks/2026-09-28/replay-grid.json", INTRO_REVIEWED_ON),
      source("CliffCompaction paper, arXiv:2609.26779", "https://arxiv.org/abs/2609.26779", INTRO_REVIEWED_ON),
      source("Gobstopper README: proxy, departures, recoverable history, install", repo("README.md"), INTRO_REVIEWED_ON),
      source("gobstopper proxy reference: prefix reuse, retry ladder, departures", repo("docs/proxy.md"), INTRO_REVIEWED_ON),
      source("v0.7.3 changelog entry: tail 0 by default", repo("CHANGELOG.md"), INTRO_REVIEWED_ON),
      source("Gobstopper roadmap: stack role and planned work", repo("docs/roadmap.md"), INTRO_REVIEWED_ON),
      source("Gobstopper public writing rules and facts public copy must keep", repo("STYLE.md"), INTRO_REVIEWED_ON),
      source("Default elision stub text", repo("crates/gobstopper-core/src/strategy/elide.rs")),
      source("Registered xcb relation sentence", repo("src/portfolio.generated.json", "design-kit")),
    ],
    observations: [
      "The study separates input volume, task completion and dollar cost; the observed token reduction is not presented as established task or cost improvement.",
      "The historical model, threshold, one-trial setup and build/log limitations stay with the data, while the surrounding lesson explains how to evaluate other workflows."
    ],
    scores: {"readerUtility": 2, "originalEvidence": 2, "factualConfidence": 2, "hostFit": 2, "voiceIntegrity": 2, "maintenanceValue": 2},
    owner: "Hraness",
    drafting: "ai-from-source",
    review: { reviewer: REVIEWER, reviewerType: "ai", reviewedOn: REVIEWED_ON },
    humanReview: null,
    reassessOn: REASSESS_ON,
    harmIfWrong: "A reader could trust a single-trial resolution or cost difference as a ranking, read gateway dollars for one model as their own bill, or assume the 128,000-token default behaves like the 45,000-token benchmark.",
    refreshTriggers: [
      "A new gobstopper release tag (the status line must be rechecked)",
      "A multi-trial Terminal-Bench rerun",
      "A default threshold or tail change",
      "An Anthropic-model benchmark",
      "Change to the proxy commands or flags shown: proxy run, install, status, --threshold, --keep-tail-percent, --carry-max-chars, --no-calibrate, --threshold-1m",
      "Change to the CLI file commands shown: detect, plan --trigger/--floor/--json, eval, apply --strategy, search-snapshot, read-snapshot, undo, mcp --allow-transcript-content",
      "gobstopper or xcb rename",
    ],
  },
};

const proofs: BlogPost = {
  slug: "proofs-for-the-admission-math",
  body: "markdown",
  title: "What Kani and Lean prove about Gobstopper's compaction",
  dek: "Kani checks Gobstopper’s numeric edit limits. Lean proves which transcript relationships survive masking, and tests connect that model to the program.",
  shareLine: "Kani checks limits; Lean proves masking laws.",
  shareHeadline: "What Kani and Lean prove about compaction",
  eyebrow: "Technique",
  published: "2026-09-24",
  updated: "2026-09-30",
  keywords: ["Kani", "Lean", "Rust", "formal proofs", "context compaction", "coding agents"],
  admission: {
    href: "/blog/proofs-for-the-admission-math",
    lifecycle: "indexable",
    readerJob: "Find out which parts of Gobstopper's compaction logic are proved, with which tools, and what the proofs leave out.",
    nonObviousAnswer: "The proofs cover edit-plan limits, token-estimate arithmetic and structural masking laws, not the context budget or recovery of the original transcript; a Rust test replays the Lean cases through the shipped Codex and Claude Code code paths, and planted bugs show each check can fail.",
    originalContribution: "Explains admission boundaries and transcript masking laws with a concrete retry example, then distinguishes model statements from the Rust parsing and file-edit path.",
    hostFit: "A product-specific technique post about Gobstopper's own proofs, on Gobstopper's host.",
    nearestUrls: [
      { url: "/blog/vault-models-that-fail-on-purpose", distinction: "That post covers the TLA+ models of the archive; this one covers Kani and Lean proofs of edit limits and masking." },
      { url: "/methodology", distinction: "Methodology explains the occupancy model and benchmarks, not the proofs." },
    ],
    sources: [
      source("Edit-plan limits and their Kani proofs", repo("crates/gobstopper-core/src/admission.rs")),
      source("Token estimate, token sum and savings kernels with Kani proofs", repo("crates/gobstopper-core/src/estimate.rs")),
      source("Kani scope, production callers, planted boundary bug and tool versions", repo("verify/core/README.md")),
      source("Lean transcript model and its 27 theorems", repo("verify/transcript/Transcript.lean")),
      source("Lean scope, Rust correspondence cases and planted bugs", repo("verify/transcript/README.md")),
      source("Rust test that replays the Lean cases on synthetic Codex and Claude Code transcripts", repo("crates/gobstopper-adapters/tests/lean_correspondence.rs")),
      source("Assurance ledger: scope, limits and exclusions for the core and transcript checks", repo("docs/assurance/ledger.json")),
      source("Claims register: status bounded_check for CLAIM-CORE and CLAIM-TRANSCRIPT", repo("docs/assurance/claims.json")),
      source("Passing Kani run record, 2026-09-24", repo("docs/assurance/receipts/2026-09-24-core.json")),
      source("Passing Lean and correspondence run record, 2026-09-24", repo("docs/assurance/receipts/2026-09-24-transcript-r2.json")),
      source("CI jobs that run both checks on pull requests and pushes to main", repo(".github/workflows/ci.yml")),
      source("Public wording rule for verification claims", repo("STYLE.md")),
    ],
    observations: [
      "Scalar boundary checks and transcript list laws answer distinct failure questions; neither substitutes for the other.",
      "The revised fixed-ID/protection-flag masking law and file-edit scope avoid implying the proxy or every public repeated request inherits the model proof."
    ],
    scores: {"readerUtility": 2, "originalEvidence": 2, "factualConfidence": 2, "hostFit": 2, "voiceIntegrity": 2, "maintenanceValue": 2},
    owner: "Hraness",
    drafting: "ai-from-source",
    review: { reviewer: REVIEWER, reviewerType: "ai", reviewedOn: REVIEWED_ON },
    humanReview: null,
    reassessOn: REASSESS_ON,
    harmIfWrong: "A reader could believe Gobstopper proves more than it does, such as staying under a context budget or recovering the original transcript.",
    refreshTriggers: [
      "Changes to the admission limits or transcript masking behavior",
      "Change to crates/gobstopper-core/src/admission.rs or estimate.rs (limits, constants, Kani harnesses)",
      "Change to verify/transcript/Transcript.lean theorem count or names, or to the correspondence case and step counts",
      "New run record or status change for CLAIM-CORE or CLAIM-TRANSCRIPT in docs/assurance/claims.json",
      "hraness.com correctness lessons go live (add the Kani, Lean and claims-ledger links to the body then)",
      "Rename of Gobstopper or of the introduction post",
    ],
  },
};

const vault: BlogPost = {
  slug: "vault-models-that-fail-on-purpose",
  body: "markdown",
  title: "How Gobstopper model-checks its archive against crashes",
  dek: "A save can succeed while cleanup deletes its data. Gobstopper’s archive models show how locks and recovery pins prevent that race.",
  shareLine: "Switching off any rule must reproduce a loss.",
  shareHeadline: "Model-checking the archive for crashes",
  eyebrow: "Technique",
  published: "2026-09-24",
  updated: "2026-09-30",
  keywords: ["Gobstopper", "TLA+", "model checking", "property testing", "crash recovery", "transcripts"],
  admission: {
    href: "/blog/vault-models-that-fail-on-purpose",
    lifecycle: "indexable",
    readerJob: "Decide whether Gobstopper's local archive can be trusted to keep an original transcript intact through crashes and concurrent cleanup, and see how that is checked.",
    nonObviousAnswer: "A clean model-check pass is trusted only because broken copies of the model, each with one safety rule switched off, must fail on that exact rule; removing the lock loses indexed data even when cleanup rechecks the index atomically, which is why saves and cleanup share a lock.",
    originalContribution: "Walks through the archive's save order, the rules the TLA+ models check, the broken copies that must fail, the Hegel tests against the real code, and the recorded state counts, with the limits of each.",
    hostFit: "A product-specific technique post about Gobstopper's own archive, on Gobstopper's host.",
    nearestUrls: [
      { url: "/blog/proofs-for-the-admission-math", distinction: "That post covers Kani and Lean proofs of edit limits and masking; this one covers the archive's crash and concurrency models." },
      { url: "/blog/introducing-gobstopper", distinction: "The introduction says what the archive is for; this post shows how its design is checked." },
    ],
    sources: [
      source("Vault model: publishers, a reader and cleanup, with a crash at every step", repo("verify/vault/Vault.tla")),
      source("Vault model notes: properties, broken variants, bounds, code correspondence and limits", repo("verify/vault/README.md")),
      source("Publication and restart model", repo("verify/vault/Publication.tla")),
      source("Vault model runner: expected failure for each broken variant", repo("verify/vault/check.py")),
      source("Vault model run of 24 September 2026: 11 configurations, state counts", repo("docs/assurance/receipts/2026-09-24-vault.json")),
      source("Watch model: native compaction requests, crashes and uncertain outcomes", repo("verify/watch/Watch.tla")),
      source("Watch model notes: five broken variants, six reachability witnesses, limits", repo("verify/watch/README.md")),
      source("Watch model run of 24 September 2026: 13 configurations, state counts", repo("docs/assurance/receipts/2026-09-24-watch.json")),
      source("Hegel stateful tests against the real transcript edits and vault", repo("crates/gobstopper-adapters/tests/surgery_hegel.rs")),
      source("Assurance ledger: what each check covers, its bounds and exclusions", repo("docs/assurance/ledger.json")),
      source("Ledger checker: file hashes and failed or stale results", repo("scripts/check_assurance.py")),
      source("CI workflow: ledger check on every run and a job that reruns both models", repo(".github/workflows/ci.yml")),
    ],
    observations: [
      "Locks protect overlapping operations while durable pins retain recovery data after a process has ended.",
      "The shrunk Rust regression examples connect finite model claims to concrete transform bugs, including a replacement larger than its input and changes on a repeated edit."
    ],
    scores: {"readerUtility": 2, "originalEvidence": 2, "factualConfidence": 2, "hostFit": 2, "voiceIntegrity": 2, "maintenanceValue": 2},
    owner: "Hraness",
    drafting: "ai-from-source",
    review: { reviewer: REVIEWER, reviewerType: "ai", reviewedOn: REVIEWED_ON },
    humanReview: null,
    reassessOn: REASSESS_ON,
    harmIfWrong: "A reader could trust the archive through failures the models do not cover, such as a power cut that loses unsynced writes.",
    refreshTriggers: [
      "Changes to the file-edit/archive behavior described by the article",
      "Any change to verify/vault/Vault.tla, Publication.tla, verify/watch/Watch.tla or their configs, or a new receipt in docs/assurance/receipts",
      "Change to the save order, lock scheme, pin rule or prune retention default (newest 10 per session) in the vault code",
      "Change to surgery_hegel.rs command range (1 to 12), case count (64) or the two regression tests",
      "Native compaction requests enabled in released builds",
      "Change to scripts/check_assurance.py or the CI model-rerun job",
      "The hraness.com correctness reference URLs going live or moving",
    ],
  },
};

const aicharts: BlogPost = {
  slug: "how-gobstopper-uses-aicharts",
  body: "markdown",
  title: "How Gobstopper uses aicharts to show token use across your agents",
  dek: "Gobstopper's installer adds aicharts, which keeps a daily record of your agents' token use on your computer. gobstopper usage reads it beside the proxy's own counts.",
  shareLine: "Every agent's token use, kept on your computer.",
  shareHeadline: "How Gobstopper uses aicharts",
  eyebrow: "Integration",
  published: "2026-10-04",
  keywords: ["Gobstopper", "aicharts", "token usage", "coding agents", "Claude Code", "Codex", "local usage history"],
  admission: {
    href: "/blog/how-gobstopper-uses-aicharts",
    lifecycle: "quarantined",
    readerJob: "Find out what the Gobstopper installer adds for usage history, how to read it, and how it relates to the proxy's own counts.",
    nonObviousAnswer: "gobstopper usage counts every agent aicharts can read, including sessions that never pass through the proxy, so a proxied request appears in both gobstopper data and the aicharts record; compare the two instead of adding them.",
    originalContribution: "The installer's checks and defaults, the five forwarded history commands, and the difference between the proxy's counts and aicharts' record, read from the v0.8.6 source and the aicharts 0.3.1 release.",
    hostFit: "A How Gobstopper uses aicharts post on Gobstopper's host for the registered relation runtime:gobstopper:aicharts:installs.",
    nearestUrls: [
      { url: "https://aicharts.io/usage", distinction: "aicharts' page covers the collector and its dashboard; this post covers what Gobstopper installs and how its usage command reads the record." },
      { url: "/blog/gobstopper-on-terminal-bench", distinction: "That post measures compaction on a benchmark; this one explains where everyday token totals come from." },
    ],
    sources: [
      source("gobstopper usage: the five forwarded aicharts history commands", repo("crates/gobstopper-cli/src/usage.rs"), "2026-10-04"),
      source("Installer: pinned aicharts digests, signature check and first-install history", repo("scripts/install.sh"), "2026-10-04"),
      source("Session data: what gobstopper data counts and how it relates to gobstopper usage", repo("docs/session-data.md"), "2026-10-04"),
      source("Gobstopper v0.8.6 release", "https://github.com/hraness/gobstopper/releases/tag/v0.8.6", "2026-10-04"),
      source("aicharts usage history and agent queries", repo("docs/usage-history.md", "aicharts"), "2026-10-04"),
      source("aicharts CLI 0.3.1 release", "https://github.com/hraness/aicharts/releases/tag/cli-v0.3.1", "2026-10-04"),
    ],
    observations: [
      "The installer verifies the aicharts archive against digests pinned in scripts/install.sh, not a checksum file fetched alongside it.",
      "History is turned on only on a first install and only when aicharts reports collection off.",
    ],
    scores: {"readerUtility": 2, "originalEvidence": 1, "factualConfidence": 2, "hostFit": 2, "voiceIntegrity": 2, "maintenanceValue": 2},
    owner: "Hraness",
    drafting: "ai-from-source",
    review: null,
    humanReview: null,
    reassessOn: "2026-11-15",
    harmIfWrong: "A reader could add the proxy's counts to aicharts' totals and overstate their token use, or expect usage data to leave the computer.",
    refreshTriggers: [
      "Change to the relation runtime:gobstopper:aicharts:installs",
      "Change to the pinned aicharts version or digests in scripts/install.sh",
      "Change to the forwarded commands in crates/gobstopper-cli/src/usage.rs",
      "Change to aicharts' collection schedule or MCP tools",
      "Rename of Gobstopper or aicharts",
    ],
  },
};

/** Every post, newest first. Quarantined and archived posts stay readable but are not listed. */
export const blogPosts: readonly BlogPost[] = [aicharts, introducing, terminalBench, proofs, vault];

/** The posts written in Markdown, whose bodies scripts/sync-blog.ts generates. */
export const markdownPosts = blogPosts.filter((post): post is BlogPost & { slug: MarkdownSlug } => post.body === "markdown");

export const articleAdmissions: readonly ArticleAdmission[] = blogPosts.map((post) => post.admission);

export const indexablePosts: readonly BlogPost[] = blogPosts.filter((post) => isArticleIndexable(post.admission));

export function postPath(post: Pick<BlogPost, "slug">): `/blog/${string}` {
  return `${BLOG_PATH}/${post.slug}`;
}

export function findPost(slug: string): BlogPost | undefined {
  return blogPosts.find((post) => post.slug === slug);
}

export function postProvenance(post: BlogPost) {
  return articleProvenanceFromAdmission(post.admission);
}

/** The release status label rendered in place of {{release.version}}. */
export function releaseLabel(): string {
  if (publishedRelease === null) throw new Error("Blog posts name the latest release; published-release.json has none.");
  return `v${publishedRelease.version}`;
}

/**
 * The launch film for a post body: exactly the homepage's `<GobFilm>` markup
 * (controls, no autoplay, captions, poster, and the film's text and scope as
 * the alternative), from one helper so the two cannot drift. Empty until the
 * film ships.
 */
export const filmHtml: (film: GobFilm | null) => string = gobFilmHtml;

/** Post body HTML with release data and the film filled in. */
export function postHtml(post: Pick<BlogPost, "slug">): string {
  if (post.slug === INTRODUCING_SLUG) return introducingHtml();
  const html = postBodies[post.slug].html
    .replaceAll("{{release.version}}", releaseLabel())
    .replace(FILM_TOKEN, () => filmHtml(gobFilm));
  if (html.includes("{{")) throw new Error(`Post ${post.slug} has an unfilled template value.`);
  return html;
}

export function postToc(post: Pick<BlogPost, "slug">) {
  // The launch post is short beats; a contents list would repeat them.
  if (post.slug === INTRODUCING_SLUG) return [];
  const toc = postBodies[post.slug].toc;
  // ARTICLE_COPY.md: a contents list only for four to eight sections.
  return toc.length >= 4 && toc.length <= 8 ? toc : [];
}

export function publishedTime(date: ArticleIsoDate): string {
  return `${date}T00:00:00.000Z`;
}

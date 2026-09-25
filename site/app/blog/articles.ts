import {
  articleProvenanceFromAdmission,
  isArticleIndexable,
  type ArticleAdmission,
  type ArticleIsoDate,
  type ArticleSourceRecord,
} from "@hraness/design-kit";

import { publishedRelease } from "../publication";
import { postBodies } from "./posts.generated";

export const BLOG_PATH = "/blog";
export const BLOG_FEED_PATH = "/blog/feed.xml";
export const BLOG_TITLE = "Gobstopper blog";
export const BLOG_DESCRIPTION =
  "Posts about Gobstopper: what it does, how its archive and edit limits are checked, and what those checks leave out.";

const REVIEWER = "Claude Opus 5.5 (claude-opus-5-5) editorial review";
const REVIEWED_ON: ArticleIsoDate = "2026-09-24";
const REASSESS_ON: ArticleIsoDate = "2026-11-05";

function repo(path: string, name = "gobstopper"): string {
  return `https://github.com/hraness/${name}/blob/main/${path}`;
}

function source(title: string, url: string, checkedOn: ArticleIsoDate = "2026-09-24"): ArticleSourceRecord {
  return { title, url, checkedOn };
}

export type PostSlug = keyof typeof postBodies;

export type BlogPost = Readonly<{
  slug: PostSlug;
  title: string;
  dek: string;
  eyebrow: string;
  published: ArticleIsoDate;
  keywords: readonly string[];
  admission: ArticleAdmission;
}>;

const introducing: BlogPost = {
  slug: "introducing-gobstopper",
  title: "Introducing Gobstopper",
  dek: "Gobstopper stubs out stale tool output in Claude Code and Codex sessions by a rule you set, after archiving the original so you can search it or restore it.",
  eyebrow: "Introducing",
  published: "2026-09-24",
  keywords: ["context compaction", "coding agents", "Claude Code", "Codex", "transcripts", "recovery"],
  admission: {
    href: "/blog/introducing-gobstopper",
    lifecycle: "indexable",
    readerJob: "Decide whether to use Gobstopper to shrink long Claude Code or Codex sessions without losing details the next turn needs, and learn how to try it.",
    nonObviousAnswer: "Gobstopper does not shrink a live session; it writes a separate compacted copy after archiving the original bytes, and a detail dropped by compaction comes back only when you or your agent search the hash-checked archive for it.",
    originalContribution: "Explains the separate-copy and archive model from the source, with the commands, the default stub text, the no-plan reason codes, and the dated benchmark figures including the sessions that produced no plan.",
    hostFit: "The product's own introduction on its own host.",
    nearestUrls: [
      { url: "/", distinction: "The homepage lists features and install steps; this post explains why the product exists and who should use something else." },
      { url: "/docs", distinction: "The docs are the full command reference; this post walks one path from preview to recovery." },
    ],
    sources: [
      source("Gobstopper README: purpose, strategies, recoverable history, install, status", repo("README.md")),
      source("Gobstopper roadmap: stack role and planned work", repo("docs/roadmap.md")),
      source("Gobstopper public writing rules and facts public copy must keep", repo("STYLE.md")),
      source("Edit plan limits and Kani proof harnesses", repo("crates/gobstopper-core/src/admission.rs")),
      source("Assurance ledger: what each check covers and excludes", repo("docs/assurance/ledger.json")),
      source("Default elision stub text", repo("crates/gobstopper-core/src/strategy/elide.rs")),
      source("Vault concurrency models and their failing variants", repo("verify/vault/README.md")),
      source("September 19, 2026 offline retrospective", "https://gobstopper.sh/benchmarks#retrospective-2026-09-19"),
      source("Registered xcb relation sentence", repo("src/portfolio.generated.json", "design-kit")),
    ],
    observations: [
      "The latest release tag predates the safeguards the post describes, so the correct install path is the main branch, not the tagged release.",
      "In the September 19, 2026 replay, 637 of 729 sessions produced no plan, so the all-session median reduction was 0% while the 73 high-context Codex root tasks had a 36.4% median.",
    ],
    scores: { readerUtility: 2, originalEvidence: 2, factualConfidence: 1, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 1 },
    owner: "Hraness",
    drafting: "ai-from-source",
    review: { reviewer: REVIEWER, reviewerType: "ai", reviewedOn: REVIEWED_ON },
    humanReview: null,
    reassessOn: REASSESS_ON,
    harmIfWrong: "A reader could run a command that does not exist on the release they installed, or trust the archive to return a detail it does not hold.",
    refreshTriggers: [
      "A new gobstopper release tag (the status line and the 'predates the safeguards' sentence must be rechecked)",
      "Change to the CLI commands or flags shown: detect, plan --trigger/--floor/--json, eval, apply --strategy, search-snapshot, read-snapshot, undo, mcp --allow-transcript-content",
      "Change to the elide stub text, keep_recent_tool_outputs default, the 64-edit or one-state-card limits, or the vault path",
      "Native provider dispatch enabled in released builds",
      "A newer benchmarks retrospective replacing the September 19, 2026 figures",
      "Change to the registered xcb relation detail sentence, or the xcb post going live (add the body link)",
      "gobstopper or xcb rename",
    ],
  },
};

const proofs: BlogPost = {
  slug: "proofs-for-the-admission-math",
  title: "How Gobstopper proves its compaction arithmetic and transcript laws",
  dek: "Gobstopper uses Kani to check its edit limits and token sums for every value of their numeric inputs, and Lean to prove that masking keeps each record's ID, order, and tool links.",
  eyebrow: "Technique",
  published: "2026-09-24",
  keywords: ["Kani", "Lean", "Rust", "formal proofs", "context compaction", "coding agents"],
  admission: {
    href: "/blog/proofs-for-the-admission-math",
    lifecycle: "indexable",
    readerJob: "Find out which parts of Gobstopper's compaction logic are proved, with which tools, and what the proofs leave out.",
    nonObviousAnswer: "The proofs cover edit-plan limits, token-estimate arithmetic and structural masking laws, not the context budget or recovery of the original transcript; a Rust test replays the Lean cases through the shipped Codex and Claude Code code paths, and planted bugs show each check can fail.",
    originalContribution: "Shows the real Kani harness and Lean theorem statements from the repository, lists the laws in plain words, and states the fixed sizes, trusted components, and correspondence coverage the proofs leave out.",
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
      "The v0.2.1 release tag contains neither the limit module nor the verify directory, so every proof in the post exists only on the main branch.",
      "The working title claimed the proofs bound the context budget; reading the harnesses showed they bound edit-plan limits and token arithmetic instead, and the title was changed to say so.",
      "On September 24, 2026 the source hashes in the Kani and Lean run records still matched the main branch the post was checked against.",
    ],
    scores: { readerUtility: 2, originalEvidence: 2, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 1 },
    owner: "Hraness",
    drafting: "ai-from-source",
    review: { reviewer: REVIEWER, reviewerType: "ai", reviewedOn: REVIEWED_ON },
    humanReview: null,
    reassessOn: REASSESS_ON,
    harmIfWrong: "A reader could believe Gobstopper proves more than it does, such as staying under a context budget or recovering the original transcript.",
    refreshTriggers: [
      "Release tag bump past v0.2.1 (the status sentence says the proofs are not yet in a tagged release)",
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
  title: "How Gobstopper checks its archive against crashes at every step",
  dek: "Gobstopper model-checks its archive design with a crash allowed at every step, and each safety rule has a broken copy that must reproduce the loss it prevents.",
  eyebrow: "Technique",
  published: "2026-09-24",
  keywords: ["Gobstopper", "TLA+", "model checking", "property testing", "crash recovery", "transcripts"],
  admission: {
    href: "/blog/vault-models-that-fail-on-purpose",
    lifecycle: "indexable",
    readerJob: "Decide whether Gobstopper's local archive can be trusted to keep an original transcript intact through crashes and concurrent cleanup, and see how that is checked.",
    nonObviousAnswer: "A clean model-check pass is trusted only because each safety rule has a broken copy that must fail on that exact rule; removing the lock loses indexed data even when cleanup rechecks the index atomically, which is why saves and cleanup share a lock.",
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
      "The TLA+ models were added on September 23, 2026, after the v0.2.1 tag of September 18, so no release includes them.",
      "Cleanup does not keep every indexed snapshot: it keeps the ones its retention setting selects plus pinned ones, and the CLI's prune keeps the newest 10 per session by default.",
    ],
    scores: { readerUtility: 1, originalEvidence: 2, factualConfidence: 2, hostFit: 2, voiceIntegrity: 2, maintenanceValue: 1 },
    owner: "Hraness",
    drafting: "ai-from-source",
    review: { reviewer: REVIEWER, reviewerType: "ai", reviewedOn: REVIEWED_ON },
    humanReview: null,
    reassessOn: REASSESS_ON,
    harmIfWrong: "A reader could trust the archive through failures the models do not cover, such as a power cut that loses unsynced writes.",
    refreshTriggers: [
      "A new gobstopper release tag (the status sentence says the release does not include the TLA+ models)",
      "Any change to verify/vault/Vault.tla, Publication.tla, verify/watch/Watch.tla or their configs, or a new receipt in docs/assurance/receipts (state counts 25,810 / 28,082 / 32,251, variant and witness counts)",
      "Change to the save order, lock scheme, pin rule or prune retention default (newest 10 per session) in the vault code",
      "Change to surgery_hegel.rs command range (1 to 12), case count (64) or the two regression tests",
      "Native compaction requests enabled in released builds",
      "Change to scripts/check_assurance.py or the CI model-rerun job",
      "The hraness.com correctness reference URLs going live or moving",
    ],
  },
};

/** Every post, newest first. Quarantined and archived posts stay readable but are not listed. */
export const blogPosts: readonly BlogPost[] = [introducing, proofs, vault];

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

/** Post body HTML with release data filled in from published-release.json. */
export function postHtml(post: Pick<BlogPost, "slug">): string {
  const html = postBodies[post.slug].html.replaceAll("{{release.version}}", releaseLabel());
  if (html.includes("{{")) throw new Error(`Post ${post.slug} has an unfilled template value.`);
  return html;
}

export function postToc(post: Pick<BlogPost, "slug">) {
  const toc = postBodies[post.slug].toc;
  // ARTICLE_COPY.md: a contents list only for four to eight sections.
  return toc.length >= 4 && toc.length <= 8 ? toc : [];
}

export function publishedTime(date: ArticleIsoDate): string {
  return `${date}T00:00:00.000Z`;
}

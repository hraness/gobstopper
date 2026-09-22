import {
  MarketingCallToAction,
  MarketingInstallPanel,
  MarketingInterfaceGrid,
  MarketingMaker,
  MarketingPage,
  MarketingPrimitives,
  MarketingProofFrame,
  MarketingQuestionList,
  MarketingSection,
  MarketingTrustBoundary,
  ProductHero,
} from "@hraness/design-kit/react/server";

import { SiteHeader, SiteFooter } from "./_components/site-chrome";
import { publishedRelease } from "./publication";


function TopicIcon({ slug }: Readonly<{ slug: string }>) {
  return (
    <img className="gobstopper-topic-icon" src={`/icons/${slug}.svg`} alt="" aria-hidden="true" width="88" height="88" loading="lazy" decoding="async" />
  );
}

const releaseVersion = publishedRelease?.version;
const repository = "https://github.com/hraness/gobstopper";

const heading = "Compact, resume, and audit every agent session.";
const summary =
  "gobstopper is the first cross-provider context compactor that preserves a content-addressed archive of every conversation state, drives provider-native compaction for Claude Code, Codex, and Devin sessions, and scores what each compaction actually kept.";
const footnote =
  `Free and MIT licensed. Rust 1.85 or newer, local transcripts, no account.${releaseVersion === undefined ? " First Gobstopper release in preparation — install from source today." : ` Current verified release v${releaseVersion}.`}`;

const primitives = [
  {
    icon: "session-detection",
    label: "Session detection",
    summary: "Scans Codex, Claude Code, and Devin session stores for live and idle sessions, reading each provider's own token-usage records — real context size where available, with a fallback estimate otherwise.",
  },
  {
    icon: "edit-ir",
    label: "A small edit IR",
    summary: "Every strategy lowers to the same edits — elide, inject digest, provider compact — and the host validates the candidate before publication. Provider linkage (parent chains, ordinals) is never removed, only rewritten in place within a line.",
  },
  {
    icon: "strategies",
    label: "Strategies",
    summary: "auto picks by transcript shape; sawtooth delegates to the provider's native compaction; elide masks stale tool output; structured emits a conservative state-card placeholder; scored can rank candidates and draft state cards with a free on-device model on macOS (opt-in); agentic is reserved for a bounded editor-model backend.",
  },
  {
    icon: "presets-config",
    label: "Presets, plugins and config",
    summary: "Global defaults, per-provider and per-session overrides, named presets, an explicitly-trusted legacy command hook, and versioned plugin bundles with exact artifact identity and host-side edit validation.",
  },
  {
    icon: "undo-vault",
    label: "Undo vault",
    summary: "Every apply snapshots the transcript into a content-addressed vault first. gobstopper undo restores byte-identical bytes into a new fork — the original transcript is never overwritten by a standalone compaction.",
  },
  {
    icon: "telemetry-eval",
    label: "Telemetry and eval",
    summary: "Every mutation emits a compaction event carrying vault snapshot references and realized retention scores for dashboards — `gobstopper events --retention` rolls them up per provider. gobstopper eval runs all strategies on temp copies and scores structural retention; it is a regression signal, not a live cost measurement.",
  },
] as const;

const trust = [
  {
    label: "Transcripts are never destroyed",
    detail: "Rewrites replace payload content inside existing lines, never deleting records, so provider resume chains stay valid. Standalone compaction publishes a separate, verified transcript copy; the source file is left unchanged. A verified snapshot exists before any byte changes, and gobstopper verify checks structural integrity after.",
  },
  {
    label: "Provider keeps live authority",
    detail: "On a running session Gobstopper asks the provider's own compaction machinery instead of editing files under it. Custom transcript surgery stays an offline operation on idle sessions and forks.",
  },
  {
    label: "Honest outcomes",
    detail: "A provider-side compaction that fails is recorded as failed — quota rejections included. Eval reports structural retention and verify findings, not measured subscription savings. Public claims distinguish occupancy models from completed benchmarks.",
  },
] as const;

const questions = [
  {
    question: "How much does it actually save?",
    answer: "Compaction lowers context occupancy in a sawtooth model: average context per turn is roughly (trigger+floor)/2. A 250k/40k policy is ~3.3x lower occupancy than a 1M-window default, and 150k/20k is ~5.6x lower. These are occupancy projections, not measured subscription savings: real cost depends on cache hit rates, billing for summary turns, re-fetches from lost detail, and how often compaction runs. We report file-byte changes and observed provider usage where available; we do not claim dollar or quota savings without a completed benchmark.",
  },
  {
    question: "Does it edit my live session?",
    answer: "Standalone `apply` or `watch` produces a separate, validated transcript copy and leaves the source unchanged. For a session the provider is actively serving, Gobstopper delegates to provider-native compaction — Codex thread/compact/start over the app-server protocol, Claude /compact through stream-json, Devin /compact over ACP. Local rewrites only touch idle transcripts, and always with a vault snapshot first.",
  },
  {
    question: "What if a compaction loses something important?",
    answer: "gobstopper undo restores the exact bytes into a new fork, leaving the current transcript untouched. gobstopper eval replays every strategy against a transcript copy and reports probe recall — which verbatim details survived — plus post-rewrite integrity findings, so you can pick a strategy with evidence before trusting it live.",
  },
  {
    question: "Which agents does it support?",
    answer: "Codex, Claude Code, and Devin today, over their real session formats. Devin stays provider-owned — detection, policy, and native compaction, never transcript surgery. The edit IR is provider-neutral, so another JSONL-transcript agent needs only a small adapter.",
  },
  {
    question: "Can I run my own compaction logic?",
    answer: "Yes — a `preset.command` pipes normalized transcript JSON to a program and applies the edits it returns, or a versioned `gobstopper-plugin.json` bundle declares capabilities and a checked executable. Host-side validation bounds every proposal: no edit can grow the transcript, skip protected recent output, bypass linkage checks, or exceed digest size. Trusted subprocesses are not security sandboxes, so explicit trust and artifact identity are required.",
  },
  {
    question: "Who made it?",
    answer: "Ben Guo, a musician and builder, formerly a founder and engineering leader at companies including Venmo and Stripe, now building from Puerto Rico. Gobstopper is published by Hraness under the MIT license.",
  },
] as const;

const structuredData = {
  "@context": "https://schema.org",
  "@type": "FAQPage",
  mainEntity: questions.map(({ answer, question }) => ({
    "@type": "Question",
    acceptedAnswer: { "@type": "Answer", text: answer },
    name: question,
  })),
};

export default function Home() {
  return (
    <div data-hraness-marketing-preset="editorial">
      <script
        dangerouslySetInnerHTML={{ __html: JSON.stringify(structuredData) }}
        type="application/ld+json"
      />
      <SiteHeader path="/" />

      <main id="main" tabIndex={-1}>
        <MarketingPage>
          <div className="hraness-material-wall">
            <ProductHero
              align="start"
              actions={[
                { href: "#install", label: "Install Gobstopper" },
                { href: "/docs", label: "Read the docs" },
              ]}
              boundary={footnote}
              className="gobstopper-marketing-hero"
              eyebrow=""
              frame={(
                <MarketingProofFrame
                  className="hraness-material-pane"
                  caption="On a 333k-token Claude session, native --autocompact 100 appended 59 records and removed 0; gobstopper compacted removed 43 stale tool records and resumed cleanly."
                  credit="Live qualification"
                  title="Head-to-head: gobstopper vs. Claude --autocompact"
                >
                  <pre className="transcript" tabIndex={0}><code>{`$ claude --resume 034... --autocompact 100
# provider added 59 records, removed 0

$ gobstopper apply 034... --in-place --strategy compacted
# elided 43 records, injected digest, resumed cleanly

$ gobstopper diff 034-original 034-compacted
removed: 43   added: 46 (43 stubs + 3 tail records)`}</code></pre>
                </MarketingProofFrame>
              )}
              heading={heading}
              headingId="hero-title"
              name=""
              summary={summary}
            />
          </div>

          <MarketingPrimitives
            heading="A policy over your transcripts, not a new editor."
            headingId="model-title"
            id="model"
            items={primitives.map((primitive) => ({
              example: <TopicIcon slug={primitive.icon} />,
              label: primitive.label,
              summary: primitive.summary,
            }))}
            label=""
            summary="Providers compact late — near the top of the context window, where every turn is most expensive and recall is already degrading. Gobstopper moves the boundary down and lets you choose what happens there."
          />

          <MarketingInterfaceGrid
            heading="One policy, three ways in."
            headingId="interfaces-title"
            id="interfaces"
            interfaces={[
              {
                label: "CLI",
                summary: "Detect, plan, apply, verify, undo — every step inspectable before anything changes.",
                example: (
                  <>
                    <TopicIcon slug="cli" />
                    <pre tabIndex={0}><code>{`gobstopper plan <session> --trigger 250000
gobstopper apply <session> --strategy elide
gobstopper verify <session> && gobstopper undo <session>`}</code></pre>
                  </>
                ),
              },
              {
                label: "Watcher and hooks",
                summary: "A polling daemon that stages a plan before the threshold and publishes a validated copy when crossed — or provider hooks that snapshot and log around native compaction.",
                example: (
                  <>
                    <TopicIcon slug="watcher" />
                    <pre tabIndex={0}><code>{`gobstopper watch --trigger 250000 --double-buffer
gobstopper install-hooks   # Claude settings + Codex hooks.json`}</code></pre>
                  </>
                ),
              },
              {
                label: "Your program",
                summary: "preset.command receives normalized transcript JSON and returns edits. A versioned plugin bundle declares capabilities and a checked executable for the same seam.",
                example: (
                  <>
                    <TopicIcon slug="custom-program" />
                    <pre tabIndex={0}><code>{`[presets.my-policy]
strategy = "elide"
keep_recent_tool_outputs = 4

[presets.custom]
command = ["node", "my-editor.js"]`}</code></pre>
                  </>
                ),
              },
            ]}
            label=""
            summary="The same plans, edits, and safety rails underneath each entry point. There is no agent-only path behind the convenient one."
          />

          <MarketingSection
            heading="What Gobstopper will not do."
            headingId="boundary-title"
            id="boundary"
            label=""
            summary="Compaction that breaks a resume chain or hides a failure is worse than none. The constraints are enforced in code, not promised in prose."
          >
            <MarketingTrustBoundary
              heading="Small enough to trust."
              headingId="kernel-title"
              id="kernel"
              items={trust}
              label=""
              summary="These rules are enforced by the command surface and its tests, not by convention."
            />
          </MarketingSection>

          <MarketingInstallPanel
            eyebrow=""
            heading="Install and compact your first session."
            headingId="install-title"
            id="install"
          >
            <p className="install-note">{releaseVersion === undefined ? "First Gobstopper release in preparation — install from source" : `Current verified release · v${releaseVersion}`}</p>
            {publishedRelease === null ? (
              <>
                <pre className="install-command" tabIndex={0}><code>{`cargo install --git ${repository} gobstopper
gobstopper --help`}</code></pre>
                <pre className="install-command" tabIndex={0}><code>{`gobstopper detect
gobstopper plan <session> --trigger 250000
gobstopper watch`}</code></pre>
              </>
            ) : (
              <>
                <pre className="install-command" tabIndex={0}><code>{`cargo install --git ${repository} --tag v${releaseVersion} gobstopper
gobstopper --help`}</code></pre>
                <pre className="install-command" tabIndex={0}><code>{`gobstopper detect
gobstopper plan <session> --trigger 250000
gobstopper watch`}</code></pre>
                <p className="install-note">
                  <a href={publishedRelease.verificationRun}>Public release verification</a>.{" "}
                </p>
              </>
            )}
            <p className="install-note">
              Needs Rust 1.85 or newer. Reads Codex, Claude Code, and Devin session data locally;
              nothing leaves the machine.{" "}
              <a href="/docs#install">Read the full reference</a>.
            </p>
          </MarketingInstallPanel>

          <MarketingQuestionList
            heading="Before you install."
            headingId="questions-title"
            id="questions"
            label=""
            questions={questions.map(({ answer, question }) => ({
              answer: <p>{answer}</p>,
              question,
            }))}
          />

          <MarketingMaker
            heading="Built by Ben Guo"
            headingId="maker-title"
            id="maker"
            label=""
            links={[
              { href: "https://hraness.com", label: "hraness.com" },
              { href: "https://x.com/hraness", label: "@hraness" },
              { href: repository, label: "GitHub" },
            ]}
          >
            <p>
              Gobstopper is built by Ben Guo, a musician and builder, formerly a founder and
              engineering leader at companies including Venmo and Stripe, now building from
              Puerto Rico. It is published by Hraness under the MIT license.
            </p>
          </MarketingMaker>

          <MarketingCallToAction
            actions={[
              { href: "#install", label: "Install Gobstopper" },
              { href: "/docs", label: "Read the docs" },
            ]}
            footnote={footnote}
            heading="Let long sessions stay long — and cheap."
            headingId="cta-title"
            summary="Set a trigger, pick a strategy, and stop paying for the same 900k tokens on every turn."
          />
        </MarketingPage>
      </main>

      <SiteFooter path="/" />
    </div>
  );
}

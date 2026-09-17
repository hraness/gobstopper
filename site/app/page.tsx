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
  MarketingSiteHeader,
  MarketingTrustBoundary,
  ProductHero,
} from "@hraness/design-kit/react/server";
import { AskAiAboutThis } from "@hraness/ui";

import { publishedRelease } from "./publication";
import { readmeLead, readmeTitle } from "./readme.generated";

function TopicIcon({ slug }: Readonly<{ slug: string }>) {
  return (
    <img className="gobstopper-topic-icon" src={`/icons/${slug}.svg`} alt="" aria-hidden="true" width="88" height="88" loading="lazy" decoding="async" />
  );
}

const releaseVersion = publishedRelease?.version;
const repository = "https://github.com/hraness/gobstopper";

const heading = "Compact earlier. Spend less. Keep the thread.";
const footnote =
  `Free and MIT licensed. Rust 1.85 or newer, local transcripts, no account.${releaseVersion === undefined ? " First Gobstopper release in preparation — install from source today." : ` Current verified release v${releaseVersion}.`}`;

const primitives = [
  {
    icon: "session-detection",
    label: "Session detection",
    summary: "Scans Codex and Claude Code transcript stores for live and idle sessions, reading each provider's own token-usage records — real context size, not an estimate.",
  },
  {
    icon: "edit-ir",
    label: "A small edit IR",
    summary: "Every strategy lowers to the same edits — elide, inject digest, provider compact — applied in place so parent chains and ordinals never break.",
  },
  {
    icon: "strategies",
    label: "Strategies",
    summary: "auto picks by transcript shape; sawtooth delegates to the provider's native compaction; elide masks stale tool output; structured writes a state-card digest; agentic hands bounded keep/elide/summarize tools to a small editor model.",
  },
  {
    icon: "presets-config",
    label: "Presets and config",
    summary: "Global defaults, per-provider and per-session overrides, named presets, and a preset.command escape hatch that runs your own edit program over normalized transcript JSON.",
  },
  {
    icon: "undo-vault",
    label: "Undo vault",
    summary: "Every apply snapshots the transcript into a content-addressed vault first. gobstopper undo restores byte-identical — and the undo itself is snapshotted.",
  },
  {
    icon: "telemetry-eval",
    label: "Telemetry and eval",
    summary: "Every mutation emits a compaction event for dashboards. gobstopper eval runs all strategies on temp copies and scores what the compacted session still recalls.",
  },
] as const;

const trust = [
  {
    label: "Transcripts are never destroyed",
    detail: "Rewrites are in-place stubs, not deletions — provider resume chains stay valid. A verified snapshot exists before any byte changes, and gobstopper verify checks structural integrity after.",
  },
  {
    label: "Provider keeps live authority",
    detail: "On a running session Gobstopper asks the provider's own compaction machinery instead of editing files under it. Custom transcript surgery stays an offline operation on idle sessions and forks.",
  },
  {
    label: "Honest outcomes",
    detail: "A provider-side compaction that fails is recorded as failed — quota rejections included. Eval reports probe recall and verify findings, not just token deltas.",
  },
] as const;

const questions = [
  {
    question: "How much does it actually save?",
    answer: "Compaction is a sawtooth: steady-state cost per turn is roughly (trigger+floor)/2. Against a 1M-window provider default (~480k average input), a 250k trigger lands near 3.3x fewer input tokens; an aggressive 150k/20k setting near 5.6x. The best case — long tool-heavy sessions where elision drives the floor toward zero — approaches 10x.",
  },
  {
    question: "Does it edit my live session?",
    answer: "No. For a session the provider is actively serving, Gobstopper delegates to provider-native compaction — Codex thread/compact/start over the app-server protocol, Claude /compact through stream-json. Local rewrites only touch idle transcripts, and always with a vault snapshot first.",
  },
  {
    question: "What if a compaction loses something important?",
    answer: "gobstopper undo restores the exact bytes. gobstopper eval replays every strategy against a transcript copy and reports probe recall — which verbatim details survived — plus post-rewrite integrity findings, so you can pick a strategy with evidence before trusting it live.",
  },
  {
    question: "Which agents does it support?",
    answer: "Codex and Claude Code today, over their real transcript formats. The edit IR is provider-neutral, so another JSONL-transcript agent needs only a small adapter.",
  },
  {
    question: "Can I run my own compaction logic?",
    answer: "Yes — preset.command pipes normalized transcript JSON to any program and applies the edits it returns. The agentic preset uses the same seam: a bounded editor model that may keep, elide, summarize, or defer each item.",
  },
  {
    question: "Who made it?",
    answer: "Ben Guo, a musician and builder, formerly a founder and engineering leader at companies including Venmo and Stripe, now building from Puerto Rico. Gobstopper is published by Hraness under the MIT license.",
  },
] as const;

const navigation = [
  { href: "#model", label: "Model" },
  { href: "#interfaces", label: "Interfaces" },
  { href: "#install", label: "Install" },
  { href: "/docs", label: "Docs" },
  { href: repository, label: "GitHub" },
] as const;

function BrandMark() {
  return <span aria-hidden="true" className="brand-mark">🍬</span>;
}

export default function Home() {
  const structuredData = [
    {
      "@context": "https://schema.org",
      "@type": "SoftwareSourceCode",
      codeRepository: repository,
      description: readmeLead,
      license: "https://opensource.org/license/mit",
      name: readmeTitle,
      programmingLanguage: "Rust",
      runtimePlatform: "Cargo",
      url: "https://gobstopper.sh",
    },
    {
      "@context": "https://schema.org",
      "@type": "FAQPage",
      mainEntity: questions.map(({ answer, question }) => ({
        "@type": "Question",
        acceptedAnswer: { "@type": "Answer", text: answer },
        name: question,
      })),
    },
  ];

  return (
    <div data-hraness-marketing-preset="editorial">
      <script
        dangerouslySetInnerHTML={{ __html: JSON.stringify(structuredData) }}
        type="application/ld+json"
      />
      <a className="skip-link" href="#main">Skip to content</a>
      <MarketingSiteHeader
        className="hraness-material-chrome"
        action={{ href: "#install", label: "Install Gobstopper" }}
        brand={<><BrandMark />Gobstopper</>}
        brandLabel="Gobstopper home"
        links={navigation}
      />

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
                caption="Example session: find the heavy sessions, preview the edit, then let the watcher hold the threshold."
                credit="From the README"
                title="Watch one long session get cheaper"
              >
                <pre className="transcript" tabIndex={0}><code>{`$ gobstopper detect
claude_code  4f3a…  active   context ~231k tokens   lifetime ~416M in

$ gobstopper plan 4f3a --strategy auto
context: 93k -> ~40k tokens (saves ~53k)
strategy: elide — 10 stale tool outputs masked, tail preserved

$ gobstopper watch --trigger 250000`}</code></pre>
              </MarketingProofFrame>
            )}
            heading={heading}
            headingId="hero-title"
            name=""
            summary={readmeLead}
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
                summary: "A polling daemon that stages a plan before the threshold and swaps it in when crossed — or provider hooks that snapshot and log around native compaction.",
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
                summary: "preset.command receives normalized transcript JSON and returns edits. The agentic preset runs a bounded editor model over the same seam.",
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
              Needs Rust 1.85 or newer. Reads Codex and Claude Code transcripts locally;
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

      <AskAiAboutThis className="ask-ai" url="https://gobstopper.sh" />

      <div className="site-footer">
        <p>Gobstopper is open source for developers and the agents working beside them.</p>
        <nav aria-label="Project links">
          <a href="/docs">Docs</a>
          <a href={repository}>hraness/gobstopper</a>
          <a href="https://hraness.com/projects">Hraness projects</a>
        </nav>
      </div>
    </div>
  );
}

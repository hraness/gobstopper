import {
  MarketingCallToAction,
  MarketingInstallPanel,
  MarketingInterfaceGrid,
  MarketingMaker,
  MarketingPage,
  MarketingPrimitives,
  MarketingProofFrame,
  MarketingQuestionList,
  MarketingRelated,
  MarketingSection,
  MarketingTrustBoundary,
  ProductHero,
} from "@hraness/design-kit/react/server";

import { SiteHeader, SiteFooter } from "./_components/site-chrome";
import { HeroField } from "./hero-field";
import { HeroGraphic } from "./hero-graphic";
import { publishedRelease } from "./publication";
import { readmeLead } from "./readme.generated";


function TopicIcon({ slug }: Readonly<{ slug: string }>) {
  return (
    <img className="gobstopper-topic-icon" src={`/icons/${slug}.svg`} alt="" aria-hidden="true" width="88" height="88" loading="lazy" decoding="async" />
  );
}

const releaseVersion = publishedRelease?.version;
const repository = "https://github.com/hraness/gobstopper";

const heading = "Prepare smaller coding-agent sessions, with a way back.";
const summary = readmeLead;
// This published release predates both Devin support and the source-build guards.
const releasePredatesPage = releaseVersion === "0.2.1";
const footnote =
  "Source preview. Install from source for the behavior described here. Free and open source (MIT or Apache-2.0). Needs Rust 1.85 or newer. Local inspection needs no account.";

const primitives = [
  {
    icon: "session-detection",
    label: "Session detection",
    summary: "Finds sessions in the Codex, Claude Code, and Devin stores. Reports measured context separately from estimates and missing usage. Recent file activity cannot tell you whether another process is using a session.",
  },
  {
    icon: "edit-ir",
    label: "A few kinds of edit",
    summary: "Strategies propose replacing stale tool output, inserting a digest, or having the provider compact. Gobstopper checks supported record links, order, and tool-call pairs before writing a copy. Those checks do not establish that a provider can resume it.",
  },
  {
    icon: "strategies",
    label: "Strategies",
    summary: "The default, auto, picks a strategy from the transcript. Sawtooth recommends provider compaction, elide replaces eligible stale tool output, and structured writes a state card from metadata. Scored ranks candidates; optional on-device models can score or draft cards. Agentic accepts edits proposed by a program you trust.",
  },
  {
    icon: "presets-config",
    label: "Presets, plugins and config",
    summary: "Set defaults once, override them per provider or per session, and save named presets. Plugins are versioned bundles pinned to an exact executable, and Gobstopper checks every edit they return. The older command hook still works once you trust it explicitly.",
  },
  {
    icon: "undo-vault",
    label: "Undo vault",
    summary: "Before writing a separate Claude Code or Codex copy, Gobstopper archives the original and prepared bytes. Search the snapshot for a missing record, read its saved text, or use undo to prepare a restored copy with a new session identity.",
  },
  {
    icon: "telemetry-eval",
    label: "Telemetry and eval",
    summary: "Events link snapshots with observed usage. Eval compares strategies on frozen input, checks supported structures, and counts which sampled details remain. It reports missing measurements and coverage; model judgments, billing, and successful task continuation need separate evidence.",
  },
] as const;

const trust = [
  {
    label: "Source files stay unchanged",
    detail: "Claude Code and Codex compaction writes a separate copy and archives the original and prepared bytes. Snapshot readers coordinate with cleanup, and damaged recovery data stops cleanup. Keep backups of the vault: local snapshots depend on your storage.",
  },
  {
    label: "Automatic provider compaction is disabled",
    detail: "The source build refuses automatic provider compaction, including auto_compact_closed, and direct Devin-store or in-place edits. An idle check cannot establish that another process has finished using a session. Provider commands need separate testing before they can be enabled.",
  },
  {
    label: "Unknown outcomes stay unknown",
    detail: "If a provider operation has an uncertain result, Gobstopper does not retry it automatically after a restart or cooldown. Missing usage stays unmeasured. Hook observations do not establish that Gobstopper caused a compaction.",
  },
] as const;

const questions = [
  {
    question: "How much does it actually save?",
    answer: "Compaction lowers how much context each turn carries. Context grows and drops in a sawtooth, so the average per turn is roughly (trigger + floor) / 2. A 250k/40k policy carries about 3.3x less context than a 1M-window default, and 150k/20k about 5.6x less. Those are projections, not measured savings. Real cost depends on cache hit rates, how summary turns are billed, details the agent has to fetch again, and how often compaction runs. We report file-byte changes and observed provider usage where we have them, and we don't claim dollar or quota savings without a completed benchmark.",
  },
  {
    question: "Does it edit my live session?",
    answer: "The source build prepares separate Claude Code and Codex copies and refuses direct Devin-store edits. It can recommend that you compact through the session's own provider controls, but automatic provider compaction is disabled. The existing session stays under its provider's control.",
  },
  {
    question: "What if a compaction loses something important?",
    answer: "Snapshot search can locate an archived record, and snapshot reads retrieve its verified bytes. For Claude Code and Codex, `gobstopper undo` prepares a separate fork with a new session identity. `gobstopper eval` measures literal probe retention and structural findings on copies. Those checks cannot guarantee that every task fact survives or that an agent will retrieve a missing fact.",
  },
  {
    question: "Which agents does it support?",
    answer: "Gobstopper inspects supported Codex, Claude Code, and Devin session formats and prepares separate Codex and Claude Code copies. Devin exports can be inspected but cannot replace its session store. New provider versions need format tests and separate resume tests.",
  },
  {
    question: "Can I run my own compaction logic?",
    answer: "Yes. A trusted `preset.command` or plugin bundle receives normalized transcript data and proposes edits. Gobstopper checks which records may change, protected output, edit combinations, size estimates, digest limits, and supported structures. Your program runs as ordinary local code. Read-only MCP inspection refuses executable strategies.",
  },
  {
    question: "Who made it?",
    answer: "Ben Guo, a musician and builder, formerly a founder and engineering leader at companies including Venmo and Stripe, now building from Puerto Rico. Hraness publishes Gobstopper under your choice of the MIT or Apache-2.0 license.",
  },
] as const;

function withCode(text: string) {
  return text.split("`").map((part, index) => (index % 2 === 1 ? <code key={index}>{part}</code> : part));
}

const structuredData = {
  "@context": "https://schema.org",
  "@type": "FAQPage",
  mainEntity: questions.map(({ answer, question }) => ({
    "@type": "Question",
    acceptedAnswer: { "@type": "Answer", text: answer.replaceAll("`", "") },
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
          <div className="hraness-material-wall gob-opening">
            <ProductHero
              backdrop={<HeroField />}
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
                  caption="On a 333k-token Claude Code session, Claude's own autocompact cut the resume context by 82% and then said unfinished renames were done. Gobstopper's elide and compacted strategies cut about 30% and recalled the task correctly. One session, recorded on an earlier build; not a general benchmark."
                  credit="Recorded September 17, 2026 · chart is illustrative"
                  title="Earlier compaction is a tradeoff."
                >
                  <HeroGraphic />
                  <pre className="transcript" tabIndex={0}><code>{`# input tokens on resume · recalled?
no compaction     312,722  yes
elide             219,167  yes
compacted         220,447  yes
autocompact 100    56,300  no`}</code></pre>
                </MarketingProofFrame>
              )}
              heading={heading}
              headingId="hero-title"
              name=""
              summary={summary}
            />
          </div>

          <MarketingPrimitives
            heading="You decide when to compact and how."
            headingId="model-title"
            id="model"
            items={primitives.map((primitive) => ({
              example: <TopicIcon slug={primitive.icon} />,
              label: primitive.label,
              summary: primitive.summary,
            }))}
            label=""
            summary="Set a threshold, compare strategies on frozen input, and inspect the candidate before provider resume. Smaller context, cache behavior and task quality need separate evidence."
          />

          <MarketingInterfaceGrid
            heading="Run it yourself, in the background, or from your own code."
            headingId="interfaces-title"
            id="interfaces"
            interfaces={[
              {
                label: "CLI",
                summary: "Find sessions, preview a plan, and prepare a separate Codex or Claude Code copy. Inspect its supported structures and keep the original for recovery.",
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
                summary: "The watcher checks sessions every 30 seconds by default and can prepare separate Claude Code or Codex copies. Dry-run mode previews the decisions. Hook setup writes a settings candidate for you to review; it does not change provider settings.",
                example: (
                  <>
                    <TopicIcon slug="watcher" />
                    <pre tabIndex={0}><code>{`gobstopper watch --dry-run --once
gobstopper install-hooks --output ./hook-candidates.json`}</code></pre>
                  </>
                ),
              },
              {
                label: "Your program",
                summary: "A preset command receives the transcript as normalized JSON and returns edits. It runs only after you mark it trusted. Package it as a versioned plugin bundle to pin the exact executable.",
                example: (
                  <>
                    <TopicIcon slug="custom-program" />
                    <pre tabIndex={0}><code>{`[presets.my-policy]
strategy = "elide"
keep_recent_tool_outputs = 4

[presets.custom]
command = "node my-editor.js"
trusted_legacy_command = true`}</code></pre>
                  </>
                ),
              },
            ]}
            label=""
            summary="Built-in strategies and trusted programs go through the same checks before Gobstopper writes a copy."
          />

          <MarketingSection
            heading="What Gobstopper won't do."
            headingId="boundary-title"
            id="boundary"
            label=""
            summary="A compaction that breaks resume or hides a failure is worse than none."
          >
            <MarketingTrustBoundary
              heading="Rules the code enforces."
              headingId="kernel-title"
              id="kernel"
              items={trust}
              label=""
              summary="Each rule is checked by the commands themselves and covered by tests."
            />
          </MarketingSection>

          <MarketingInstallPanel
            eyebrow=""
            heading="Install and inspect your first session."
            headingId="install-title"
            id="install"
          >
            <p className="install-note">Install the current source build to use the behavior described on this page.</p>
            <pre className="install-command" tabIndex={0}><code>{`cargo install --git ${repository} gobstopper --locked
gobstopper --help`}</code></pre>
            <pre className="install-command" tabIndex={0}><code>{`gobstopper detect
gobstopper plan <session> --trigger 250000
gobstopper watch --dry-run --once`}</code></pre>
            {publishedRelease === null ? (
              <p className="install-note">No release yet.</p>
            ) : (
              <>
                <p className="install-note">
                  Latest tagged release: <a href={`${repository}/releases/tag/v${releaseVersion}`}>v{releaseVersion}</a>.{" "}
                  <a href={publishedRelease.verificationRun}>See how this release was verified</a>.{" "}
                </p>
                {releasePredatesPage && (
                  <p className="install-note">Version {releaseVersion} predates Devin support and the source build&apos;s safeguards. The source install above includes both.</p>
                )}
              </>
            )}
            <p className="install-note">
              Needs Rust 1.85 or newer. Gobstopper reads Codex, Claude Code, and Devin session data on your
              machine. Built-in inspection makes no model call. If you enable a remote scorer,
              it receives selected transcript text; trusted plugins run your code.{" "}
              <a href="/docs#install--use">Read the full reference</a>.
            </p>
            <p className="install-note">
              <a href={`${repository}/blob/main/docs/assurance/qualification.json`}>Provider support status</a>{" · "}
              <a href={`${repository}/blob/main/docs/assurance/operations.md`}>Recovery runbook</a>{" · "}
              <a href={`${repository}/blob/main/verify/vault/README.md`}>Proof scopes and assumptions</a>
            </p>
          </MarketingInstallPanel>

          <MarketingQuestionList
            heading="Before you install."
            headingId="questions-title"
            id="questions"
            label=""
            questions={questions.map(({ answer, question }) => ({
              answer: <p>{withCode(answer)}</p>,
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
              Puerto Rico. Hraness publishes it under your choice of the MIT or Apache-2.0 license.
            </p>
          </MarketingMaker>

          <MarketingRelated
            groups={[
              {
                heading: "The agent platform",
                headingId: "related-tools",
                summary: "Tools for the accounts, web reads, and models your agent runs on.",
                items: [
                  {
                    name: "Ghostget",
                    href: "https://ghostget.com",
                    role: "A fast web gateway for agents",
                    relationship: "Ghostget shrinks each web read before it reaches the context. One measured article came to about 3,800 tokens, against 36,000 for the raw page.",
                  },
                  {
                    name: "xcb",
                    href: "https://xcb.sh",
                    role: "One router for your Claude, Codex, and Devin subscriptions",
                    relationship: "xcb runs the subscriptions behind your sessions from one terminal workspace and shows the token spend on each account.",
                  },
                  {
                    name: "Aicharts",
                    href: "https://aicharts.io",
                    role: "AI model benchmarks and usage inspection",
                    relationship: "Aicharts benchmarks the models your agent uses and shows what each local session cost.",
                  },
                ],
              },
              {
                heading: "The personal apps",
                headingId: "related-apps",
                items: [
                  {
                    name: "PeopleBlade",
                    href: "https://peopleblade.com",
                    role: "A private contact book for you and your agent",
                    relationship: "PeopleBlade gives your agent a whole contact book to work through; Gobstopper keeps that long session compact.",
                  },
                  {
                    name: "Soulscrape",
                    href: "https://soulscrape.com",
                    role: "A dated, cited dossier on a person",
                    relationship: "Deep dossier research runs long; Gobstopper can compare smaller context candidates while retaining source bytes for inspection.",
                  },
                  {
                    name: "Textbutler",
                    href: "https://textbutler.app",
                    role: "A personal message butler for Mac",
                    relationship: "Textbutler studies whole conversation histories; Gobstopper can measure candidate context reduction separately from cost.",
                  },
                  {
                    name: "Wordcell",
                    href: "https://wordcell.io",
                    role: "A Markdown knowledge base for agents",
                    relationship: "Wordcell gives an agent a whole vault to traverse; Gobstopper compacts the traversal context.",
                  },
                ],
              },
            ]}
            heading="More from Hraness."
            headingId="related-title"
            label="Related"
            summary="Other tools your agent can use alongside Gobstopper."
          />

          <MarketingCallToAction
            actions={[
              { href: "#install", label: "Install Gobstopper" },
              { href: "/docs", label: "Read the docs" },
            ]}
            footnote={footnote}
            heading="Inspect the tradeoff before you resume."
            headingId="cta-title"
            summary="Set a trigger, compare a strategy, and retain the exact source behind every prepared copy."
          />
        </MarketingPage>
      </main>

      <SiteFooter path="/" />
    </div>
  );
}

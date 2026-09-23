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


function TopicIcon({ slug }: Readonly<{ slug: string }>) {
  return (
    <img className="gobstopper-topic-icon" src={`/icons/${slug}.svg`} alt="" aria-hidden="true" width="88" height="88" loading="lazy" decoding="async" />
  );
}

const releaseVersion = publishedRelease?.version;
const repository = "https://github.com/hraness/gobstopper";

const heading = "Compact coding-agent sessions early, with a way back.";
const summary =
  "Gobstopper watches your Claude Code, Codex, and Devin sessions and compacts each one when its context passes a size you choose. It snapshots every transcript before changing it, so the original is never lost, and it can score what each strategy preserved.";
// v0.2.1 (September 18, 2026) predates Devin support and several features
// described below. The note disappears once a newer release is published.
const releasePredatesPage = releaseVersion === "0.2.1";
const footnote =
  `Free and open source (MIT or Apache-2.0). Needs Rust 1.85 or newer. Runs on your machine with no account.${releaseVersion === undefined ? " No release yet; install from source." : ` Latest release: v${releaseVersion}.`}${releasePredatesPage ? " Devin support and several features described here are newer than that release; install from source to use them." : ""}`;

const primitives = [
  {
    icon: "session-detection",
    label: "Session detection",
    summary: "Finds live and idle sessions in the Codex, Claude Code, and Devin session stores and reads each provider's own token counts. When a provider doesn't report context size, Gobstopper estimates it.",
  },
  {
    icon: "edit-ir",
    label: "A few kinds of edit",
    summary: "Every strategy reduces to the same small set of edits, such as hiding stale tool output, inserting a digest, or asking the provider to compact. Gobstopper validates the result before writing it and never removes the parent links and ordinals a provider needs to resume the session.",
  },
  {
    icon: "strategies",
    label: "Strategies",
    summary: "The default, auto, picks a strategy from the shape of the transcript. Sawtooth hands off to the provider's own compaction, elide hides stale tool output, and structured writes a conservative state card. Scored ranks what to hide and, on macOS, can draft state cards with a free on-device model if you opt in. Agentic accepts edits proposed by a program you trust.",
  },
  {
    icon: "presets-config",
    label: "Presets, plugins and config",
    summary: "Set defaults once, override them per provider or per session, and save named presets. Plugins are versioned bundles pinned to an exact executable, and Gobstopper checks every edit they return. The older command hook still works once you trust it explicitly.",
  },
  {
    icon: "undo-vault",
    label: "Undo vault",
    summary: "Before changing anything, Gobstopper stores an exact copy of the transcript in a local vault. By default, Claude Code and Codex compactions are written to a new fork, and undo restores the original bytes into another. Idle Devin sessions are edited in place, and undo writes the original back unless Devin has added to the session since.",
  },
  {
    icon: "telemetry-eval",
    label: "Telemetry and eval",
    summary: "Each compaction is logged with a pointer to its snapshot and a score for what it kept, and gobstopper events --retention totals those scores by provider. The eval command runs every strategy on temporary copies and scores how much structure each one preserved. Treat it as a regression check, not a measure of cost savings.",
  },
] as const;

const trust = [
  {
    label: "Transcripts are never destroyed",
    detail: "Gobstopper snapshots a transcript before it changes a byte. By default, Claude Code and Codex compactions go to a separate copy and leave the source file alone. An idle Devin session is edited in place in one database transaction, after Gobstopper checks that Devin does not hold the session lock, and the watcher rewrites idle Claude Code sessions in place only if you turn that on. Rewrites keep the links a provider needs to resume the session, and gobstopper verify checks the result.",
  },
  {
    label: "Running sessions are left alone",
    detail: "Gobstopper does not edit a session while it is running. Claude Code and Devin report which sessions are live, and a Codex session counts as running if it was written in the last three minutes. For a running Claude Code or Devin session, Gobstopper's prompt hook can suggest /compact, and the provider does the compaction. Custom rewrites run only on idle sessions and forks.",
  },
  {
    label: "Failures are reported as failures",
    detail: "A provider compaction that fails, including a quota rejection, is recorded as failed. Eval reports what a strategy kept and what verify found, not money saved, and this site labels projections separately from benchmark results.",
  },
] as const;

const questions = [
  {
    question: "How much does it actually save?",
    answer: "Compaction lowers how much context each turn carries. Context grows and drops in a sawtooth, so the average per turn is roughly (trigger + floor) / 2. A 250k/40k policy carries about 3.3x less context than a 1M-window default, and 150k/20k about 5.6x less. Those are projections, not measured savings. Real cost depends on cache hit rates, how summary turns are billed, details the agent has to fetch again, and how often compaction runs. We report file-byte changes and observed provider usage where we have them, and we don't claim dollar or quota savings without a completed benchmark.",
  },
  {
    question: "Does it edit my live session?",
    answer: "No. For a running Claude Code or Devin session, Gobstopper's prompt hook can suggest `/compact`, and the provider does the compaction. If you set `auto_compact_closed`, Gobstopper asks the provider to compact a closed session: `thread/compact/start` over the Codex app-server protocol, `claude --resume <id> -p /compact` for Claude Code, and `/compact` over ACP for Devin. Otherwise it rewrites idle transcripts itself, always after a snapshot. By default, Claude Code and Codex rewrites go to a separate, validated copy, and an idle Devin session is edited in place in one database transaction.",
  },
  {
    question: "What if a compaction loses something important?",
    answer: "`gobstopper undo` restores the exact original bytes into a new fork and leaves the current transcript untouched. To choose a strategy before you rely on it, `gobstopper eval` runs every strategy on a copy of a transcript and reports probe recall (which exact details survived) along with any integrity problems the rewrite caused.",
  },
  {
    question: "Which agents does it support?",
    answer: "Codex, Claude Code, and Devin, each through its real session format. Edits are provider-neutral, so another agent that stores JSONL transcripts needs only a small adapter.",
  },
  {
    question: "Can I run my own compaction logic?",
    answer: "Yes. A `preset.command` sends the normalized transcript JSON to your program and applies the edits it returns. A versioned `gobstopper-plugin.json` bundle does the same with declared capabilities and a pinned executable. Gobstopper checks every proposed edit: none may grow the transcript, remove protected recent output, break the resume links, or exceed the digest size limit. Your program runs as an ordinary subprocess, not in a sandbox, so Gobstopper runs it only after you trust that exact executable.",
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
            <HeroField />
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
                  caption="On a 333k-token Claude Code session, Claude's own autocompact cut the resume context by 82% and then said unfinished renames were done. Gobstopper's elide and compacted strategies cut about 30% and recalled the task correctly. One session, recorded on an earlier build; not a general benchmark."
                  credit="Recorded September 17, 2026 · chart is illustrative"
                  title="The sawtooth: compact early, every crossing."
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
            summary="Providers compact near the top of the context window, where each turn costs the most and long-context recall is weakest. Gobstopper lets you set a lower threshold and choose what happens when a session crosses it."
          />

          <MarketingInterfaceGrid
            heading="Run it yourself, in the background, or from your own code."
            headingId="interfaces-title"
            id="interfaces"
            interfaces={[
              {
                label: "CLI",
                summary: "Find sessions, preview a plan, apply it, verify the result, and undo it if you need to. Nothing changes until you apply.",
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
                summary: "The watcher checks your sessions every 30 seconds by default and writes a validated copy of an idle Claude Code or Codex session once it crosses your threshold. Provider hooks snapshot and log each time the provider compacts on its own.",
                example: (
                  <>
                    <TopicIcon slug="watcher" />
                    <pre tabIndex={0}><code>{`gobstopper watch --dry-run
gobstopper watch
gobstopper install-hooks`}</code></pre>
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
            summary="All three use the same plans, edits, and checks. None of them skips the snapshot or the validation."
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
            heading="Install and compact your first session."
            headingId="install-title"
            id="install"
          >
            <p className="install-note">{releaseVersion === undefined ? "No release yet. Install from source." : `Latest release: v${releaseVersion}`}</p>
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
                  <a href={publishedRelease.verificationRun}>See how this release was verified</a>.{" "}
                </p>
                {releasePredatesPage && (
                  <>
                    <p className="install-note">For Devin support and the other features newer than v{releaseVersion}, install from source:</p>
                    <pre className="install-command" tabIndex={0}><code>{`cargo install --git ${repository} gobstopper`}</code></pre>
                  </>
                )}
              </>
            )}
            <p className="install-note">
              Needs Rust 1.85 or newer. Gobstopper reads Codex, Claude Code, and Devin session data on your
              machine and sends none of it anywhere.{" "}
              <a href="/docs#install--use">Read the full reference</a>.
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
                    relationship: "Deep dossier research runs long; Gobstopper compacts the session without losing what the agent already established.",
                  },
                  {
                    name: "Textbutler",
                    href: "https://textbutler.app",
                    role: "A personal message butler for Mac",
                    relationship: "Textbutler studies whole conversation histories; Gobstopper keeps the study session cheap.",
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
            heading="Keep long sessions going for less."
            headingId="cta-title"
            summary="Set a trigger, pick a strategy, and stop sending the same 900k tokens on every turn."
          />
        </MarketingPage>
      </main>

      <SiteFooter path="/" />
    </div>
  );
}

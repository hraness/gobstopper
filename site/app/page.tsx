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
  ProviderMarkChip,
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

const heading = "Context compaction you can undo.";
const summary =
  "A local proxy that keeps long sessions in Claude Code, Codex, opencode, Crush, Aider, Goose, and other agents under a context threshold. Past it, each request keeps the task and the newest turns word for word and drops old tool output the agent can read again, following CliffCompaction's rule. File commands preview the same kind of cut on saved sessions and write a smaller copy. Every original byte stays in a local vault you can search and restore from.";
const footnote =
  "Free and open source (MIT or Apache-2.0). Installs with Cargo and needs Rust 1.85 or newer; the proxy also needs curl 8.3 or newer. Runs on your machine with no account.";

const primitives = [
  {
    icon: "session-detection",
    label: "Session detection",
    summary: "Finds sessions in the Codex and Claude Code stores. Reports measured context separately from estimates and missing usage. Recent file activity cannot tell you whether another process is using a session.",
  },
  {
    icon: "edit-ir",
    label: "A few kinds of edit",
    summary: "Strategies propose replacing stale tool output, inserting a digest, or having the provider compact. Gobstopper checks supported record links, order, and tool-call pairs before writing a copy. Those checks do not establish that a provider can resume it.",
  },
  {
    icon: "strategies",
    label: "Strategies",
    summary: "The default, auto, picks a strategy from the transcript. Sawtooth recommends provider compaction, elide replaces eligible stale tool output, cliff keeps the newest assistant steps and drops older tool results over 500 bytes, and structured writes a state card from metadata. For running sessions in Claude Code, Codex, and OpenAI-compatible agents, gobstopper proxy applies CliffCompaction's rule to each outgoing request. Scored ranks candidates; optional on-device models can score or draft cards. Agentic accepts edits proposed by a program you trust.",
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

const agents = ["claudecode", "codex", "opencode", "crush", "aider", "goose"] as const;

const trust = [
  {
    label: "The proxy sends the original when in doubt",
    detail: "The proxy listens on 127.0.0.1 only and logs no request or response content. If it cannot parse a request, hits an internal error, or the provider rejects a compacted request for any reason other than length, it sends the client's original bytes.",
  },
  {
    label: "Source files stay unchanged",
    detail: "Claude Code and Codex compaction writes a separate copy and archives the original and prepared bytes. Snapshot readers coordinate with cleanup, and damaged recovery data stops cleanup. Keep backups of the vault: local snapshots depend on your storage.",
  },
  {
    label: "Automatic provider compaction is disabled",
    detail: "The source build refuses automatic provider compaction, including auto_compact_closed, and direct in-place edits. An idle check cannot establish that another process has finished using a session. Provider commands need separate testing before they can be enabled.",
  },
  {
    label: "Unknown outcomes stay unknown",
    detail: "If a provider operation has an uncertain result, Gobstopper does not retry it automatically after a restart or cooldown. Missing usage stays unmeasured. Hook observations do not establish that Gobstopper caused a compaction.",
  },
] as const;

const questions = [
  {
    question: "How much does it actually save?",
    answer: "The proxy's summary rule comes from CliffCompaction, whose authors report up to 50% lower cost at a bounded context with Terminal-Bench scores held or improved on the models they tested, and a higher Terminal-Bench 2.1 score through Claude Code than Claude Code's own auto-compaction. Those are their measurements of their proxy, with costs modeled on perfect prompt caching. Gobstopper has not rerun them. On September 26, 2026, on one Mac running v0.4.1 at its defaults for about 77 minutes of Claude Code, requests the proxy compacted went out 38% smaller in estimated tokens; most requests were under the threshold and went out unchanged. That is an estimate, not a bill: real cost depends on cache hits, details the agent reads again, and how long the session runs. The benchmarks page lists every dated measurement.",
  },
  {
    question: "Does it edit my live session?",
    answer: "Session files, no. The source build prepares separate Claude Code and Codex copies and refuses in-place edits. If you point Claude Code or Codex at `gobstopper proxy`, it compacts the requests the client sends while the session runs and leaves the session files unchanged. Automatic provider compaction stays disabled.",
  },
  {
    question: "What if a compaction loses something important?",
    answer: "Snapshot search can locate an archived record, and snapshot reads retrieve its verified bytes. For Claude Code and Codex, `gobstopper undo` prepares a separate fork with a new session identity. `gobstopper eval` measures literal probe retention and structural findings on copies. Those checks cannot guarantee that every task fact survives or that an agent will retrieve a missing fact.",
  },
  {
    question: "Which agents does it support?",
    answer: "The proxy speaks all three dialects coding agents use: Anthropic Messages (Claude Code, opencode, Crush), OpenAI Responses (Codex), and OpenAI Chat Completions (opencode, Crush, Aider, Goose, and other OpenAI-compatible clients). Any agent that lets you set a custom provider address can point at it. File commands read Claude Code and Codex session formats and prepare separate copies. Agents without a configurable model address, such as service-bound CLIs, cannot be proxied. Claude Code and Codex routing is live-checked; Chat Completions coverage is contract-tested on synthetic histories. New provider versions need format tests and separate resume tests.",
  },
  {
    question: "How is this different from CliffCompaction?",
    answer: "CliffCompaction is an API proxy: it rewrites each request over a token threshold while the session runs, keeps the head and the last three turns verbatim, drops tool results over 500 characters, and never paraphrases. `gobstopper proxy` ports its summary rule to the Anthropic Messages, OpenAI Responses, and Chat Completions dialects and by default keeps more of the recent session verbatim: at least the last three turns, plus older whole turns that fit its tail budget. Gobstopper's file commands prepare copies you inspect and resume, with the source archived in a vault, and the `cliff` strategy applies the drop rule to those copies. The comparison page lists the differences and the authors' benchmark figures.",
  },
  {
    question: "Can I run my own compaction logic?",
    answer: "Yes. A trusted `preset.command` or plugin bundle receives normalized transcript data and proposes edits. Gobstopper checks which records may change, protected output, edit combinations, size estimates, digest limits, and supported structures. Your program runs as ordinary local code. Read-only MCP inspection refuses executable strategies.",
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
                { href: "/benchmarks", label: "See the benchmarks" },
              ]}
              boundary={footnote}
              className="gobstopper-marketing-hero"
              eyebrow="Session compaction tool"
              frame={(
                <MarketingProofFrame
                  className="hraness-material-pane"
                  caption="On a 333k-token Claude Code session, Claude's own autocompact cut the resume context by 82% and then said unfinished renames were done. Gobstopper's elide and compacted strategies cut about 30% and recalled the task correctly. One session, recorded on an earlier build; not a general benchmark."
                  credit="Recorded September 17, 2026 · chart is illustrative"
                  title="The smallest context forgot the task."
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

          <MarketingSection
            heading="Works with the agents you already use."
            headingId="agents-title"
            id="agents"
            label=""
            summary="The proxy speaks Anthropic Messages, OpenAI Responses, and OpenAI Chat Completions. Any agent that accepts a custom provider address can point at it; Claude Code and Codex routing is live-checked and the rest are contract-tested."
          >
            <div className="gob-agent-marks">
              {agents.map((agent) => (
                <ProviderMarkChip key={agent} mark={agent} size={34} />
              ))}
            </div>
          </MarketingSection>

          <MarketingInterfaceGrid
            heading="In front of your agent, on saved sessions, or from your own code."
            headingId="interfaces-title"
            id="interfaces"
            interfaces={[
              {
                label: "Proxy",
                summary: "Point an agent's provider address at the proxy, or let proxy run start one for a single session. Replay a recorded session to see what the proxy would have sent, without calling a provider.",
                example: (
                  <>
                    <TopicIcon slug="watcher" />
                    <pre tabIndex={0}><code>{`gobstopper proxy run -- claude
ANTHROPIC_BASE_URL=http://127.0.0.1:8260 claude
gobstopper proxy replay <session>`}</code></pre>
                  </>
                ),
              },
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
                    <TopicIcon slug="presets-config" />
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
            <p className="install-note">This installs the current source build, which this page describes.</p>
            <pre className="install-command" tabIndex={0}><code>{`cargo install --git ${repository} gobstopper --locked
gobstopper --help`}</code></pre>
            <pre className="install-command" tabIndex={0}><code>{`gobstopper proxy run -- claude    # one Claude Code session through the proxy
gobstopper proxy serve            # background proxy on http://127.0.0.1:8260
gobstopper proxy status           # requests compacted, estimated tokens saved`}</code></pre>
            <pre className="install-command" tabIndex={0}><code>{`gobstopper detect
gobstopper plan <session> --trigger 250000`}</code></pre>
            {publishedRelease === null ? (
              <p className="install-note">No release yet.</p>
            ) : (
              <>
                <p className="install-note">
                  Latest tagged release: <a href={`${repository}/releases/tag/v${releaseVersion}`}>v{releaseVersion}</a>.{" "}
                  <a href={publishedRelease.verificationRun}>See how this release was verified</a>.{" "}
                </p>
              </>
            )}
            <p className="install-note">
              Needs Rust 1.85 or newer. Gobstopper reads Codex and Claude Code session data on your
              machine. Built-in inspection makes no model call. If you enable a remote scorer,
              it receives selected transcript text; trusted plugins run your code.{" "}
              <a href="/docs#install--use">Read the full reference</a>.
            </p>
            <p className="install-note">
              <a href={`${repository}/blob/main/docs/assurance/qualification.json`}>Provider support status</a>{" · "}
              <a href={`${repository}/blob/main/docs/assurance/operations.md`}>Recovery runbook</a>{" · "}
              <a href={`${repository}/blob/main/verify/README.md`}>Verification scopes and assumptions</a>
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
            heading="Built by Hraness."
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
              Hraness is a software studio in Puerto Rico. We build tools that give AI
              agents memory, context, web access, and a record of their work, and we
              make apps and sourced archives for people. Hraness publishes Gobstopper
              under your choice of the MIT or Apache-2.0 license.
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
                    role: "Named web actions for AI agents: read pages, save media, use connected accounts",
                    relationship: "Ghostget gives the agent you already use a fixed list of reviewed web actions: read a page, save one media item, or act in a connected account. Your agent never sees your credentials and never steers a browser.",
                  },
                  {
                    name: "xcb",
                    href: "https://xcb.sh",
                    role: "Routes coding tasks across the Claude, Codex, and Devin plans you have",
                    relationship: "xcb uses Gobstopper's elision policy to drop stale tool output from Claude Code and Codex prompts once context passes a threshold, and keeps the original output in local history. It is on by default.",
                  },
                  {
                    name: "AI Charts",
                    href: "https://aicharts.io",
                    role: "Model benchmark scores plotted against cost and tokens per task",
                    relationship: "AI Charts plots published AI benchmark scores against cost and tokens per task, marking the best score at every budget. A local collector measures your own agents' token use.",
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
                    role: "Local personal CRM for everyone you know, built for your agent",
                    relationship: "PeopleBlade brings your contacts from Apple Contacts, iMessage, Google Contacts, WhatsApp, LinkedIn, and more into one private book on your computer. Keep notes beside each person, and let your agent search the book from the command line.",
                  },
                  {
                    name: "Soulscrape",
                    href: "https://soulscrape.com",
                    role: "Free agent skill that writes dated dossiers on people, sources cited",
                    relationship: "Soulscrape is a free agent skill that writes a dated dossier on how a person decides, writes, argues, and changes their mind, with every claim tied to its sources. Keep it private, or publish it.",
                  },
                  {
                    name: "Textbutler",
                    href: "https://textbutler.app",
                    role: "AI butler for the iMessage, WhatsApp, and Beeper chats you choose",
                    relationship: "Textbutler is an AI butler for the iMessage, WhatsApp, and Beeper chats you choose on your Mac. Turn it on for one person, and it replies as a clearly marked assistant that knows your history with them.",
                  },
                  {
                    name: "Wordcell",
                    href: "https://wordcell.io",
                    role: "Markdown knowledge base that gives agents the decisions behind code",
                    relationship: "Wordcell keeps decisions, plans, and sources as Markdown files beside your code. Coding agents find them by exact words, by meaning with an optional local model, or from the file they are about to change.",
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
              { href: "/benchmarks", label: "See the benchmarks" },
            ]}
            footnote={footnote}
            heading="Make your next long session smaller."
            headingId="cta-title"
            summary="Set a trigger, compare a strategy, and keep the exact source behind every prepared copy."
          />
        </MarketingPage>
      </main>

      <SiteFooter path="/" />
    </div>
  );
}

import {
  MarketingFlow,
  MarketingPage,
  MarketingQuestionList,
  MarketingSection,
  ProductHero,
  ProviderMarkChip,
} from "@hraness/design-kit/react/server";
import { product, type PortfolioProductId } from "@hraness/design-kit/portfolio";

import { SiteHeader, SiteFooter } from "./_components/site-chrome";
import { publishedRelease } from "./publication";

/** A related product from the portfolio snapshot: name, address, and one-line role. */
function related(id: PortfolioProductId, name: string) {
  const { canonicalUrl, oneLiner } = product(id);
  return { href: canonicalUrl, name, role: oneLiner };
}

const releaseVersion = publishedRelease?.version;
const repository = "https://github.com/hraness/gobstopper";

const heading = "Context compaction you can undo.";
const summary =
  "Gobstopper is a free, open-source command-line tool that makes long coding sessions smaller. Its local proxy keeps each request from Claude Code, Codex, and other agents under a token threshold by summarizing older turns and sending the newest ones word for word. On saved sessions it writes a smaller copy, and every original byte stays in a local vault you can search and restore from.";
const facts =
  "MIT or Apache-2.0 · Installs with Cargo (Rust 1.85 or newer) · The proxy needs curl 8.3 or newer · Runs on your machine, no account";

const agents = ["claudecode", "codex", "opencode", "crush", "aider", "goose"] as const;

const steps = [
  {
    label: "Start the proxy with your agent",
    code: "gobstopper proxy run -- claude",
    detail: "It starts on a free local port, points the agent's ANTHROPIC_BASE_URL and OPENAI_BASE_URL at itself, and stops when the agent exits. Use `gobstopper proxy serve` for a background proxy on 127.0.0.1:8260.",
  },
  {
    label: "Small requests pass through",
    detail: "Requests under the threshold, 128,000 estimated tokens by default, go to the provider unchanged.",
  },
  {
    label: "Large requests keep the recent work",
    detail: "Past the threshold, the system prompt, the first task, and at least the last three turns go out word for word. Older turns become one summary that keeps human and assistant text and tool results up to 500 characters. Longer tool results are dropped, because the agent can read the file or rerun the command.",
  },
  {
    label: "Check what it did",
    code: "gobstopper proxy status",
    detail: "Shows the running proxy's settings, how many requests it compacted, and the estimated tokens saved. Session files stay unchanged.",
  },
] as const;

const ways = [
  {
    label: "Proxy",
    summary: "Point an agent's provider address at the proxy, or let `proxy run` start one for a single session. Replay a recorded session to see what the proxy would have sent, without calling a provider.",
    code: `gobstopper proxy run -- claude
ANTHROPIC_BASE_URL=http://127.0.0.1:8260 claude
gobstopper proxy replay <session>`,
  },
  {
    label: "Saved sessions",
    summary: "Find Claude Code and Codex sessions, preview a plan, and prepare a separate smaller copy. Check it for problems that would break resume, and keep the original for recovery.",
    code: `gobstopper plan <session> --trigger 250000
gobstopper apply <session> --strategy elide
gobstopper verify <session> && gobstopper undo <session>`,
  },
  {
    label: "Watcher and hooks",
    summary: "The watcher checks sessions every 30 seconds by default and can prepare separate copies. Dry-run mode previews its decisions. Hook setup writes a settings file for you to review and does not change provider settings.",
    code: `gobstopper watch --dry-run --once
gobstopper install-hooks --output ./hook-candidates.json`,
  },
  {
    label: "Your program",
    summary: "A preset command receives the transcript as normalized JSON and returns edits. It runs only after you mark it trusted. Package it as a versioned plugin bundle to pin the exact executable.",
    code: `[presets.my-policy]
strategy = "elide"
keep_recent_tool_outputs = 4

[presets.custom]
command = "node my-editor.js"
trusted_legacy_command = true`,
  },
] as const;

const inside = [
  {
    label: "Strategies",
    detail: "The default, auto, picks a strategy from the transcript. Elide replaces eligible stale tool output, cliff keeps the newest assistant steps and drops older tool results over 500 bytes, structured writes a state card from metadata, and sawtooth recommends provider compaction. Scored ranks candidates; optional on-device models can score them or draft cards.",
  },
  {
    label: "Undo vault",
    detail: "Before writing a separate Claude Code or Codex copy, Gobstopper archives the original and prepared bytes. Search a snapshot for a missing record, read its saved text, or run `undo` to prepare a restored copy with a new session identity.",
  },
  {
    label: "Presets and plugins",
    detail: "Set defaults once, override them per provider or per session, and save named presets. Plugins are versioned bundles pinned to an exact executable, and Gobstopper checks every edit they return.",
  },
  {
    label: "Eval and telemetry",
    detail: "Eval compares strategies on frozen input, checks supported structures, and counts which sampled details remain. Events link snapshots with observed usage and report missing measurements as missing.",
  },
] as const;

const trust = [
  {
    label: "The proxy sends the original when in doubt",
    detail: "The proxy listens on 127.0.0.1 only and logs no request or response content. If it cannot parse a request, hits an internal error, or the provider rejects a compacted request for any reason other than length, it sends the client's original bytes.",
  },
  {
    label: "Source files stay unchanged",
    detail: "Claude Code and Codex compaction writes a separate copy and archives the original and prepared bytes. Damaged recovery data stops cleanup. Keep backups of the vault: local snapshots depend on your storage.",
  },
  {
    label: "Automatic provider compaction is disabled",
    detail: "The source build refuses automatic provider compaction, including auto_compact_closed, and direct in-place edits. An idle check cannot establish that another process has finished using a session. Provider commands need separate testing before they can be enabled.",
  },
  {
    label: "Unknown outcomes stay unknown",
    detail: "If a provider operation has an uncertain result, Gobstopper does not retry it automatically after a restart or cooldown. Missing usage stays unmeasured. Hook observations do not establish that Gobstopper caused a compaction.",
  },
  {
    label: "Smaller is not the same as better",
    detail: "Copy checks cover record links, order, and tool-call pairs. They do not establish that a provider can resume the copy, that every task fact survives, or what you will be billed.",
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
    answer: "CliffCompaction is an API proxy: it rewrites each request over a token threshold while the session runs, keeps the head and the last three turns verbatim, drops tool results over 500 characters, and never paraphrases. `gobstopper proxy` ports its summary rule to the Anthropic Messages, OpenAI Responses, and Chat Completions dialects and by default keeps more of the recent session verbatim: at least the last three turns, plus older whole turns that fit its tail budget. Each summary also carries the human's words and the assistant's visible replies from the turns earlier compactions summarized, up to 24,000 characters, where CliffCompaction discards the previous summary. Gobstopper's file commands prepare copies you inspect and resume, with the source archived in a vault, and the `cliff` strategy applies the drop rule to those copies. The comparison page lists the differences and the authors' benchmark figures.",
  },
  {
    question: "Can I run my own compaction logic?",
    answer: "Yes. A trusted `preset.command` or plugin bundle receives normalized transcript data and proposes edits. Gobstopper checks which records may change, protected output, edit combinations, size estimates, digest limits, and supported structures. Your program runs as ordinary local code. Read-only MCP inspection refuses executable strategies.",
  },
] as const;

const relatedProducts = [
  related("xcb", "xcb"),
  related("wrench", "Ghostget"),
  related("aicharts", "AI Charts"),
  related("peopleblade", "PeopleBlade"),
  related("kb", "Wordcell"),
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
    <div data-hraness-marketing-preset="editorial" data-hraness-pattern="none" className="gob-home">
      <script
        dangerouslySetInnerHTML={{ __html: JSON.stringify(structuredData) }}
        type="application/ld+json"
      />
      <SiteHeader path="/" />

      <main id="main" tabIndex={-1}>
        <MarketingPage>
          <ProductHero
            backdrop={false}
            align="start"
            actions={[
              { href: "#install", label: "Install Gobstopper" },
              { href: "/benchmarks", label: "See the benchmarks", emphasis: "secondary" },
            ]}
            boundary={facts}
            className="gob-hero"
            eyebrow="Session compaction tool"
            heading={heading}
            headingId="hero-title"
            name=""
            summary={summary}
          />

          <figure className="gob-proof" aria-labelledby="proof-title">
            <figcaption className="gob-proof__head">
              <span id="proof-title">Resume trial on one 333k-token Claude Code session</span>
              <span className="gob-proof__date">September 17, 2026</span>
            </figcaption>
            <pre className="gob-code gob-proof__table" tabIndex={0}><code>{`strategy          input tokens on resume   recalled the task?
no compaction          312,722               yes
elide                  219,167               yes
compacted              220,447               yes
autocompact 100         56,300               no`}</code></pre>
            <p className="gob-proof__note">
              Claude&apos;s own autocompact cut the resume context by 82% and then said unfinished renames were done. Gobstopper&apos;s elide and compacted strategies cut about 30% and recalled the task correctly. One session on an earlier build, not a general benchmark.{" "}
              <a href="/benchmarks">All dated measurements</a>
            </p>
          </figure>

          <MarketingSection
            heading="It keeps each request under a threshold you choose."
            headingId="how-title"
            id="how"
            label="How the proxy works"
            summary="No model call writes the summary, and each compaction starts again from the full history your agent resends, so a summary is never summarized."
          >
            <MarketingFlow
              ariaLabel="How the proxy works"
              className="gob-steps"
              steps={steps.map(({ detail, label, ...rest }) => ({
                label,
                detail: detail.replaceAll("`", ""),
                ...("code" in rest ? { code: rest.code } : {}),
              }))}
            />
          </MarketingSection>

          <MarketingSection
            heading="Works with the agents you already use."
            headingId="agents-title"
            id="agents"
            label="Agents"
            summary="The proxy speaks Anthropic Messages, OpenAI Responses, and OpenAI Chat Completions. Any agent that accepts a custom provider address can point at it. Claude Code and Codex routing is live-checked; the others are contract-tested."
          >
            <div className="gob-agent-marks">
              {agents.map((agent) => (
                <ProviderMarkChip key={agent} mark={agent} size={32} />
              ))}
            </div>
          </MarketingSection>

          <MarketingSection
            heading="In front of your agent, on saved sessions, or from your own code."
            headingId="interfaces-title"
            id="interfaces"
            label="Four ways to run it"
            summary="Built-in strategies and trusted programs go through the same checks before Gobstopper writes a copy."
          >
            <div className="gob-rows">
              {ways.map((way) => (
                <div className="gob-row" key={way.label}>
                  <div className="gob-row__text">
                    <h3>{way.label}</h3>
                    <p>{withCode(way.summary)}</p>
                  </div>
                  <pre className="gob-code" tabIndex={0}><code>{way.code}</code></pre>
                </div>
              ))}
            </div>
          </MarketingSection>

          <MarketingSection
            heading="What's inside."
            headingId="model-title"
            id="model"
            label="Parts"
            summary="Set a threshold, compare strategies on frozen input, and inspect the candidate before you resume it."
          >
            <dl className="gob-list">
              {inside.map((item) => (
                <div key={item.label}>
                  <dt>{item.label}</dt>
                  <dd>{withCode(item.detail)}</dd>
                </div>
              ))}
            </dl>
          </MarketingSection>

          <MarketingSection
            heading="What Gobstopper won't do."
            headingId="boundary-title"
            id="boundary"
            label="Limits"
            summary="A compaction that breaks resume or hides a failure is worse than none. The commands enforce these rules, and tests cover them."
          >
            <dl className="gob-list">
              {trust.map((item) => (
                <div key={item.label}>
                  <dt>{item.label}</dt>
                  <dd>{item.detail}</dd>
                </div>
              ))}
            </dl>
          </MarketingSection>

          <MarketingSection
            heading="Install and inspect your first session."
            headingId="install-title"
            id="install"
            label="Install"
            summary="This installs the current source build, which this page describes. It needs Rust 1.85 or newer."
          >
            <div className="gob-install">
              <pre className="gob-code install-command" tabIndex={0}><code>{`cargo install --git ${repository} gobstopper --locked
gobstopper --help`}</code></pre>
              <pre className="gob-code install-command" tabIndex={0}><code>{`gobstopper proxy run -- claude    # one Claude Code session through the proxy
gobstopper proxy serve            # background proxy on http://127.0.0.1:8260
gobstopper proxy status           # requests compacted, estimated tokens saved`}</code></pre>
              <pre className="gob-code install-command" tabIndex={0}><code>{`gobstopper detect                         # list sessions and their size
gobstopper plan <session> --trigger 250000   # preview a compaction; changes nothing`}</code></pre>
              {publishedRelease === null ? (
                <p className="install-note">No release yet.</p>
              ) : (
                <p className="install-note">
                  Latest tagged release: <a href={`${repository}/releases/tag/v${releaseVersion}`}>v{releaseVersion}</a>.{" "}
                  <a href={publishedRelease.verificationRun}>See how this release was verified</a>.
                </p>
              )}
              <p className="install-note">
                Gobstopper reads Codex and Claude Code session data on your machine. Built-in
                inspection makes no model call. If you enable a remote scorer, it receives
                selected transcript text; trusted plugins run your code.{" "}
                <a href="/docs#install--use">Read the full reference</a>.
              </p>
              <p className="install-note">
                <a href={`${repository}/blob/main/docs/assurance/qualification.json`}>Provider support status</a>{" · "}
                <a href={`${repository}/blob/main/docs/assurance/operations.md`}>Recovery runbook</a>{" · "}
                <a href={`${repository}/blob/main/verify/README.md`}>Verification scopes and assumptions</a>
              </p>
            </div>
          </MarketingSection>

          <MarketingQuestionList
            heading="Before you install."
            headingId="questions-title"
            id="questions"
            label="Questions"
            questions={questions.map(({ answer, question }) => ({
              answer: <p>{withCode(answer)}</p>,
              question,
            }))}
          />

          <MarketingSection
            heading="Built by Hraness."
            headingId="maker-title"
            id="maker"
            label="Maker"
            summary="Hraness is a software studio in Puerto Rico. We build tools that give AI agents memory, context, web access, and a record of their work, and we make apps and sourced archives for people. Hraness publishes Gobstopper under your choice of the MIT or Apache-2.0 license."
          >
            <ul className="gob-related" aria-label="More from Hraness">
              {relatedProducts.map((item) => (
                <li key={item.href}>
                  <a href={item.href}>{item.name}</a>
                  <span>{item.role}</span>
                </li>
              ))}
            </ul>
            <p className="install-note">
              <a href="https://hraness.com">hraness.com</a>{" · "}
              <a href="https://x.com/hraness">@hraness</a>{" · "}
              <a href={repository}>GitHub</a>
            </p>
          </MarketingSection>
        </MarketingPage>
      </main>

      <SiteFooter path="/" />
    </div>
  );
}

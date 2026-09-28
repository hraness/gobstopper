import {
  MarketingCodeBlock,
  MarketingDataTable,
  MarketingFlow,
  MarketingInstallPanel,
  MarketingInterfaceGrid,
  MarketingMaker,
  MarketingPage,
  MarketingPrimitives,
  MarketingQuestionList,
  MarketingRelated,
  MarketingSection,
  MarketingTrustBoundary,
  ProductHero,
  ProviderMarkChip,
} from "@hraness/design-kit/react/server";
import { product, type PortfolioProductId } from "@hraness/design-kit/portfolio";

import { GobFilm } from "./_components/gob-film";
import { GobFuse } from "./_components/gob-fuse";
import { GobSawtooth } from "./_components/gob-sawtooth";
import { GobTokens } from "./_components/gob-tokens";
import { SiteHeader, SiteFooter } from "./_components/site-chrome";
import { gobFilm } from "./_data/gob-film";
import { publishedRelease } from "./publication";

/** A related product from the portfolio snapshot: mark, name, address, and one-line role. */
function related(id: PortfolioProductId, name: string) {
  const { canonicalUrl, mark, oneLiner } = product(id);
  return { href: canonicalUrl, mark, name, role: oneLiner };
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
    detail: "Past the threshold, the system prompt, the first task, and the last three turns go out word for word. Older turns become one summary that keeps human and assistant text and tool results up to 500 characters. Longer tool results are dropped, because the agent can read the file or rerun the command.",
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
    language: "sh",
  },
  {
    label: "Saved sessions",
    summary: "Find Claude Code and Codex sessions, preview a plan, and prepare a separate smaller copy. Check it for problems that would break resume, and keep the original for recovery.",
    code: `gobstopper plan <session> --trigger 250000
gobstopper apply <session> --strategy elide
gobstopper verify <session> && gobstopper undo <session>`,
    language: "sh",
  },
  {
    label: "Watcher and hooks",
    summary: "The watcher checks sessions every 30 seconds by default and can prepare separate copies. Dry-run mode previews its decisions. Hook setup writes a settings file for you to review and does not change provider settings.",
    code: `gobstopper watch --dry-run --once
gobstopper install-hooks --output ./hook-candidates.json`,
    language: "sh",
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
    language: "text",
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
    answer: "On Terminal-Bench 2.1 (September 27 and 28, 2026; 89 tasks, one trial per arm; Claude Code with GLM 5.3 Flash; 45,000-token threshold), Gobstopper at its default tail resolved 61 tasks and Claude Code with no proxy 60, within single-trial noise, while Gobstopper sent 29% fewer input tokens. Provider-reported cost for that model was about 16% lower, which one trial cannot separate from noise. Dollars depend on your provider's cache pricing, and subscriptions are not billed per token. Most short sessions never reach the default 128,000-token threshold and pass through unchanged. CliffCompaction's authors report their own results for the rule on the comparison page. The benchmarks page lists every dated measurement.",
  },
  {
    question: "Does it edit my live session?",
    answer: "Session files, no. The source build prepares separate Claude Code and Codex copies and refuses in-place edits. If you point Claude Code or Codex at `gobstopper proxy`, it compacts the requests the client sends while the session runs and leaves the session files unchanged. Automatic provider compaction stays disabled.",
  },
  {
    question: "What if a compaction loses something important?",
    answer: "The proxy drops only tool results over 500 characters from the request; the files and commands behind them are still there, and the agent's transcript keeps the full history. Snapshot search can locate an archived record, and snapshot reads retrieve its verified bytes. For Claude Code and Codex, `gobstopper undo` prepares a separate fork with a new session identity. `gobstopper eval` measures literal probe retention and structural findings on copies. Those checks cannot guarantee that every task fact survives or that an agent will retrieve a missing fact.",
  },
  {
    question: "Which agents does it support?",
    answer: "The proxy speaks all three dialects coding agents use: Anthropic Messages (Claude Code, opencode, Crush), OpenAI Responses (Codex), and OpenAI Chat Completions (opencode, Crush, Aider, Goose, and other OpenAI-compatible clients). Any agent that lets you set a custom provider address can point at it. File commands read Claude Code and Codex session formats and prepare separate copies. Agents without a configurable model address, such as service-bound CLIs, cannot be proxied. Claude Code and Codex routing is live-checked; Chat Completions coverage is contract-tested on synthetic histories. New provider versions need format tests and separate resume tests.",
  },
  {
    question: "How is this different from CliffCompaction?",
    answer: "CliffCompaction is an API proxy: it rewrites each request over a token threshold while the session runs, keeps the head and the last three turns verbatim, drops tool results over 500 characters, and never paraphrases. `gobstopper proxy` ports its summary rule to the Anthropic Messages, OpenAI Responses, and Chat Completions dialects and keeps the last three turns verbatim by default and can keep more with a tail budget. Each summary also carries the human's words and the assistant's visible replies from the turns earlier compactions summarized, up to 24,000 characters, where CliffCompaction discards the previous summary. Gobstopper's file commands prepare copies you inspect and resume, with the source archived in a vault, and the `cliff` strategy applies the drop rule to those copies. The comparison page has the authors' benchmark figures. In all, Gobstopper makes seven departures, listed on the comparison page.",
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
    <div data-hraness-marketing-preset="editorial" data-hraness-pattern="none">
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
            eyebrow="Session compaction tool"
            heading={heading}
            headingId="hero-title"
            name=""
            summary={summary}
          />

          <MarketingDataTable
            caption="Resume trial on one 333k-token Claude Code session"
            columns={[
              { label: "Strategy" },
              { label: "Input tokens on resume", numeric: true },
              { label: "vs. no compaction", numeric: true },
              { label: "Recalled the task?" },
            ]}
            meta="September 17, 2026"
            note={
              <>
                Claude&apos;s own autocompact cut the resume context by 82% and then said
                unfinished renames were done. Gobstopper&apos;s elide and compacted
                strategies cut about 30% and recalled the task correctly. One session on
                an earlier build, not a general benchmark.{" "}
                <a href="/benchmarks">All dated measurements</a>
              </>
            }
            rows={[
              ["no compaction", "312,722", "—", "yes"],
              ["elide", "219,167", "−29.9%", { content: "yes", tone: "positive" }],
              ["compacted", "220,447", "−29.5%", { content: "yes", tone: "positive" }],
              [{ content: "autocompact 100" }, "56,300", "−82.0%", { content: "no", tone: "negative" }],
            ]}
          />

          <MarketingSection
            heading="Same tasks solved, 29% fewer tokens sent."
            headingId="terminal-bench-title"
            id="terminal-bench"
            label="Terminal-Bench 2.1"
            summary="On Terminal-Bench 2.1, Claude Code behind Gobstopper at its default tail resolved 61 of 89 tasks, against 60 with no proxy, a difference within single-trial noise, and sent 84.3M input tokens against 118.6M. The old default tail of 40 cost more than no proxy, so v0.7.3 made tail 0 the default. One trial per arm, GLM 5.3 Flash, 45,000-token threshold, September 27 and 28, 2026."
          >
            <GobTokens variant="home" />
            <p className="gob-section-link">
              <a href="/benchmarks#terminal-bench-2026-09-28">Setup, statistics and downloads</a>
            </p>
          </MarketingSection>

          {gobFilm === null ? null : (
            <MarketingSection
              heading="Watch it in 75 seconds."
              headingId="film-title"
              id="film"
              label="Film"
              summary="No narration. Captions carry every line. The film never plays until you press play."
            >
              <GobFilm film={gobFilm} />
            </MarketingSection>
          )}

          <MarketingSection
            heading="It keeps each request under a threshold you choose."
            headingId="how-title"
            id="how"
            label="How the proxy works"
            summary="No model call writes the summary, and each compaction starts again from the full history your agent resends, so a summary is never summarized."
          >
            <GobFuse />
            <MarketingFlow
              ariaLabel="How the proxy works"
              steps={steps.map(({ detail, label, ...rest }) => ({
                label,
                detail: detail.replaceAll("`", ""),
                ...("code" in rest ? { code: rest.code } : {}),
              }))}
            />
            <GobSawtooth variant="home" />
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

          <MarketingInterfaceGrid
            heading="In front of your agent, on saved sessions, or from your own code."
            headingId="interfaces-title"
            id="interfaces"
            interfaces={ways.map(({ code, label, language, summary: waySummary }) => ({
              example: <MarketingCodeBlock code={code} language={language} />,
              label,
              summary: waySummary.replaceAll("`", ""),
            }))}
            label="Four ways to run it"
            summary="Built-in strategies and trusted programs go through the same checks before Gobstopper writes a copy."
          />

          <MarketingPrimitives
            heading="What's inside."
            headingId="model-title"
            id="model"
            items={inside.map(({ detail, label }) => ({
              label,
              summary: detail.replaceAll("`", ""),
            }))}
            label="Parts"
            summary="Set a threshold, compare strategies on frozen input, and inspect the candidate before you resume it."
          />

          <MarketingTrustBoundary
            heading="What Gobstopper won't do."
            headingId="boundary-title"
            id="boundary"
            items={[...trust]}
            label="Limits"
            summary="A compaction that breaks resume or hides a failure is worse than none. The commands enforce these rules, and tests cover them."
          />

          <MarketingInstallPanel
            eyebrow="Install"
            heading="Install and inspect your first session."
            headingId="install-title"
            id="install"
            note={
              <p>
                This installs the current source build, which this page describes.
                It needs Rust 1.85 or newer.
              </p>
            }
          >
            <MarketingCodeBlock
              code={`cargo install --git ${repository} gobstopper --locked\ngobstopper --help`}
              language="sh"
            />
            <MarketingCodeBlock
              code={`gobstopper proxy run -- claude    # one Claude Code session through the proxy\ngobstopper proxy serve            # background proxy on http://127.0.0.1:8260\ngobstopper proxy status           # requests compacted, estimated tokens saved`}
              language="sh"
            />
            <MarketingCodeBlock
              code={`gobstopper detect                            # list sessions and their size\ngobstopper plan <session> --trigger 250000   # preview a compaction; changes nothing`}
              language="sh"
            />
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
          </MarketingInstallPanel>

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

          <MarketingMaker
            heading="Built by Hraness."
            headingId="maker-title"
            id="maker"
            label="Maker"
            links={[
              { href: "https://hraness.com", label: "hraness.com" },
              { href: "https://x.com/hraness", label: "@hraness" },
              { href: repository, label: "GitHub" },
            ]}
          >
            <p>
              Hraness is a software studio in Puerto Rico. We build tools that give
              AI agents memory, context, web access, and a record of their work,
              and we make apps and sourced archives for people. Hraness publishes
              Gobstopper under your choice of the MIT or Apache-2.0 license.
            </p>
          </MarketingMaker>

          <MarketingRelated
            heading="More from Hraness."
            headingId="related-title"
            items={[...relatedProducts]}
            label="Related"
            summary="The rest of the stack, one line each."
          />
        </MarketingPage>
      </main>

      <SiteFooter path="/" />
    </div>
  );
}

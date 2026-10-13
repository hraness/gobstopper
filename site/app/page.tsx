import {
  MarketingInstallPanel,
  MarketingPage,
  MarketingProofFrame,
  MarketingQuestionList,
  MarketingRelated,
  MarketingSection,
  MarketingStatStrip,
  ProductHero,
  ProviderMarkChip,
  SyntaxCode,
} from "@hraness/design-kit/react/server";
import { portfolioRelatedGroups } from "@hraness/design-kit/portfolio";
import { PlatformBadges, PlatformInstall } from "@hraness/design-kit/react";

import { FounderNote } from "./_components/founder-note";
import { TB_META } from "./_components/gob-bench-table";
import { GobFilm } from "./_components/gob-film";
import { GobMechanismDiagram } from "./_components/gob-mechanism";
import { GobCompactionSignature } from "./_components/gob-signature";
import { cli, commands } from "./_mockups/fixtures";
import { Testimonial } from "./_components/testimonial";
import { SiteHeader, SiteFooter } from "./_components/site-chrome";
import { gobFilm } from "./_data/gob-film";
import { ARM_LABEL } from "./_lib/gobbench-data";
import { F } from "./_lib/gobbench-format";
import { plainInline, renderInline } from "./_lib/inline";
import { publishedRelease } from "./publication";

import { productMessaging } from "./messaging";

const releaseVersion = publishedRelease?.version;
const repository = "https://github.com/hraness/gobstopper";
const heading = productMessaging.hero.heading;
const summary = productMessaging.hero.summary;
const agents = ["claudecode", "codex", "opencode", "crush", "aider", "goose"] as const;

const installSh = "curl -fsSL https://gobstopper.sh/install.sh | sh";
const buildFromSource = { label: "Build from source (Rust 1.85+)", command: `cargo install --git ${repository} gobstopper --locked` };
const installPlatforms = [
  { id: "macos", command: installSh, shell: "Terminal", note: "Apple silicon", alternatives: [buildFromSource] },
  { id: "linux", command: installSh, shell: "Terminal", note: "x86_64 and ARM64, glibc 2.34+", alternatives: [buildFromSource] },
  {
    id: "windows",
    command: "irm https://gobstopper.sh/install.ps1 | iex",
    shell: "PowerShell",
    note: "x86_64 · the proxy works; the vault, watch and provider hooks need macOS or Linux",
    alternatives: [buildFromSource],
  },
] as const;

const boundary = releaseVersion === undefined
  ? "Free and open source · MIT or Apache-2.0"
  : `Free and open source · MIT or Apache-2.0 · Latest release: v${releaseVersion}`;

/** Real `gobstopper proxy run` output: the proxy starts on a free local port and the agent is pointed at it. */
const runTranscript = [`$ ${commands.proxyRun}`, ...cli.proxyRun].join("\n");

/** Real `gobstopper plan` output: a 130,060-token session previewed at a 100,000-token trigger. */
const planTranscript = [`$ ${commands.plan}`, ...cli.plan].join("\n");

const recordedCredit = "Real output from a release build on September 29, 2026, against a synthetic Claude Code session; the project and user are made up.";

const questions = [
  {
    question: "Which agents can use the proxy?",
    answer: "Claude Code and Codex start through it with one command. Any other agent that lets you set a custom provider address and speaks Anthropic Messages, OpenAI Responses, or OpenAI Chat Completions, such as opencode, Crush, Aider, and Goose, points at the proxy's local address. File commands read Claude Code and Codex session formats.",
  },
  {
    question: "What if I need something the summary left out?",
    answer: "Before writing a separate Claude Code or Codex copy, Gobstopper archives the original and prepared bytes. Search the local vault for a missing record, read its saved text, or run `gobstopper undo` to prepare a restored copy with a new session identity.",
  },
  {
    question: "Does it edit my live session?",
    answer: "The proxy changes outgoing requests and leaves session files unchanged. File commands prepare separate copies. Gobstopper does not trigger Claude Code's or Codex's own compaction and does not edit session files in place. Automatic provider compaction stays disabled.",
  },
  {
    question: "What if an agent keeps rereading the same files?",
    answer: "You or the agent can reserve a larger context for a difficult phase, within the window the model and client support. The budget expires by time or request count, and Gobstopper keeps selected original tool results and images through repeated compactions. See [context budgets and retention](/docs#temporary-context-budgets).",
  },
  {
    question: "What happens if the proxy stops responding?",
    answer: "Request and compaction deadlines limit stalls, while logging and metrics run in background workers. `gobstopper proxy launch --client claude` checks readiness before opening Claude Code and can start directly with its official provider when the configuration permits. See [recovery and fallback](/docs#startup-recovery-and-direct-fallback).",
  },
  {
    question: "When should I use built-in compaction instead?",
    answer: "Use your agent's built-in `/compact` when you want a model-written summary without another tool. Gobstopper adds previews and archived originals for saved sessions, plus a proxy that applies local rules without a model call. See [the Claude Code comparison](/compare/claude-code-compact) or [the CliffCompaction comparison](/compare/cliffcompaction).",
  },
] as const;

const structuredData = {
  "@context": "https://schema.org",
  "@type": "FAQPage",
  mainEntity: questions.map(({ answer, question }) => ({
    "@type": "Question",
    acceptedAnswer: { "@type": "Answer", text: plainInline(answer) },
    name: question,
  })),
};

export default function Home() {
  return (
    <div data-hraness-marketing-preset="editorial" data-hraness-pattern="none">
      <script dangerouslySetInnerHTML={{ __html: JSON.stringify(structuredData) }} type="application/ld+json" />
      <SiteHeader path="/" />
      <main id="main" tabIndex={-1}>
        <MarketingPage landscape="page">
          <ProductHero
            actions={[
              { href: "#install", label: productMessaging.hero.primaryAction, emphasis: "primary" },
              { href: "#terminal-bench", label: productMessaging.hero.secondaryAction, emphasis: "secondary" },
            ]}
            align="start"
            boundary={boundary}
            eyebrow={productMessaging.category}
            frame={
              <div className="gob-hero-sig">
                <GobCompactionSignature />
              </div>
            }
            heading={heading}
            headingId="hero-title"
            layout="split"
            name=""
            summary={summary}
          />

          <Testimonial
            quote={"btw, like a week in, Gobstopper is absolutely kicking ass. I’m using the higher context scope for my orchestration agent, and then all workers on the default, and the token savings are insane and the quality of task completion is unchanged."}
            name="Ryaan"
            role="Software Engineer"
            company="BandLab"
          />

          <FounderNote
            emoji={productMessaging.blurb.emoji}
            paragraphs={[productMessaging.blurb.text]}
            action={{ label: `${productMessaging.blurb.cta.label}:`, href: productMessaging.blurb.cta.url }}
            signature="Ben Guo"
          />

          <MarketingSection
            heading={productMessaging.headings["home-live"]}
            headingId="how-title"
            id="how"
            label="How it works"
            summary="Past the threshold, the proxy keeps the newest turns word for word and summarizes older ones with local rules, no extra model call."
          >
            <GobMechanismDiagram />
          </MarketingSection>

          <MarketingSection
            heading="Start your agent through it."
            headingId="run-title"
            id="run"
            label="Works with"
            summary="Claude Code and Codex start through the proxy with one command; opencode, Crush, Aider, and Goose point at its local address."
          >
            <MarketingProofFrame chrome="terminal" className="gob-run" title="Run a session">
              <pre tabIndex={0}><SyntaxCode code={runTranscript} language="shell" styles="classes" /></pre>
            </MarketingProofFrame>
            <div className="gob-agent-marks">
              {agents.map((agent) => <ProviderMarkChip key={agent} mark={agent} size={32} />)}
            </div>
          </MarketingSection>

          <MarketingSection
            heading={productMessaging.headings["home-file-copy"]}
            headingId="saved-title"
            id="saved"
            label="Saved sessions"
            summary="Preview what a strategy would cut from a saved session, then write a separate copy and keep the original in a local vault."
          >
            <MarketingProofFrame chrome="terminal" className="gob-plan" credit={recordedCredit} title="~/code/lanternshop">
              <pre tabIndex={0}><SyntaxCode code={planTranscript} language="shell" styles="classes" /></pre>
            </MarketingProofFrame>
            <p className="gob-more"><a href="/docs#install--use">Read how saved sessions work</a>.</p>
          </MarketingSection>

          <MarketingSection
            heading={`${F.inputFewer} fewer input tokens, as many tasks solved.`}
            headingId="terminal-bench-title"
            id="terminal-bench"
            label="Terminal-Bench 2.1"
            summary={`Claude Code through Gobstopper solved ${F.solved.tail0} of 89 tasks against ${F.solved.no_proxy} with its built-in compaction, on ${F.inputFewer} fewer input tokens.`}
          >
            <MarketingStatStrip
              ariaLabel="Terminal-Bench 2.1, with and without Gobstopper"
              className="gob-stats"
              columns={4}
              source={
                <>
                  Terminal-Bench 2.1, with and without Gobstopper. {TB_META}.{" "}
                  <a href="/benchmarks#terminal-bench-2026-09-28">Setup, statistics, and downloads</a> ·{" "}
                  <a href="/methodology">How Gobstopper measures</a>.
                </>
              }
              stats={[
                { value: F.inputFewer, label: "fewer input tokens", detail: `${F.input.tail0} against ${F.input.no_proxy}` },
                { value: `${F.solved.tail0} of 89`, label: "tasks solved", detail: ARM_LABEL.tail0 },
                { value: `${F.solved.no_proxy} of 89`, label: "tasks solved", detail: ARM_LABEL.no_proxy },
                { value: F.cacheFewer, label: "fewer cache reads", detail: `${F.cache.tail0} against ${F.cache.no_proxy}` },
              ]}
            />
          </MarketingSection>

          {gobFilm === null ? null : (
            <MarketingSection
              heading={productMessaging.headings["home-film"]}
              headingId="film-title"
              id="film"
              label="Film"
              summary="Captions carry every line, and nothing plays until you press play."
            >
              <GobFilm film={gobFilm} />
            </MarketingSection>
          )}

          <MarketingInstallPanel
            eyebrow="Install"
            heading={productMessaging.headings["home-install"]}
            headingId="install-title"
            id="install"
            note={<p>Each installer downloads the latest release for your platform, checks its SHA-256, and installs it for your user only.</p>}
          >
            <PlatformInstall platforms={installPlatforms} />
            <PlatformBadges platforms={["macos", "linux", { id: "windows", note: "partial" }]} />
            <p className="install-note">
              Then run one session through the proxy: <code>gobstopper proxy run -- claude</code>.{" "}
              <a href="/docs#install--use">Installation and command reference</a>.
            </p>
            {publishedRelease === null ? (
              <p className="install-note">No release yet.</p>
            ) : (
              <p className="install-note">Latest tagged release: <a href={`${repository}/releases/tag/v${releaseVersion}`}>v{releaseVersion}</a>.</p>
            )}
            <details className="gob-limits" id="limits">
              <summary>Limits</summary>
              <ul>
                <li>The proxy needs curl 8.3 or newer. Built-in compaction makes no model call; an optional remote scorer receives selected transcript text, and plugins you install run your code.</li>
                <li>The Terminal-Bench 2.1 result is one trial per arm with one model, GLM 5.3 Flash, at a 45,000-token threshold rather than the 128,000 default; the two-task gap in tasks solved is within single-trial noise, and 21 of the 89 tail-0 trials may have run an earlier build.</li>
                <li>Claude Code and Codex routing is checked against live sessions; OpenAI Chat Completions routing is tested against synthetic histories.</li>
                <li>On Windows the proxy works; the vault, watch, and provider hooks need macOS or Linux.</li>
                <li>The vault lives on your disk and is only as safe as your backups. Copy checks cover record links, order, and tool-call pairs; they cannot promise that a provider resumes the copy or that a summary kept every fact.</li>
              </ul>
            </details>
          </MarketingInstallPanel>

          <MarketingQuestionList
            heading={productMessaging.headings["home-questions"]}
            headingId="questions-title"
            id="questions"
            label="Questions"
            questions={questions.map(({ answer, question }) => ({ answer: <p>{renderInline(answer)}</p>, question }))}
          />
          <MarketingRelated
            className="gob-related"
            groups={portfolioRelatedGroups(["xcb"])}
            heading={productMessaging.headings["home-related"]}
            headingId="related-title"
          />
        </MarketingPage>
      </main>
      <SiteFooter path="/" />
    </div>
  );
}

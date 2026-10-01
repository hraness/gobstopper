import {
  MarketingInstallPanel,
  MarketingPage,
  MarketingPrimitives,
  MarketingQuestionList,
  MarketingRelated,
  MarketingSection,
  ProductHero,
  ProviderMarkChip,
} from "@hraness/design-kit/react/server";
import { portfolioRelatedGroups } from "@hraness/design-kit/portfolio";
import { PlatformBadges, PlatformInstall } from "@hraness/design-kit/react";

import { Terminal } from "./_components/code-block";
import { GobFilm } from "./_components/gob-film";
import { GobMeterShowcase, GobVaultSteps } from "./_mockups/gob-mockups";
import { GobTokens } from "./_components/gob-tokens";
import { SiteHeader, SiteFooter } from "./_components/site-chrome";
import { gobFilm } from "./_data/gob-film";
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

const questions = [
  {
    question: "Which agents can use the proxy?",
    answer: "Any agent that lets you set a custom provider address and uses Anthropic Messages, OpenAI Responses, or OpenAI Chat Completions. Claude Code and Codex routing is live-checked; Chat Completions is contract-tested on synthetic histories. File commands read Claude Code and Codex session formats.",
  },
  {
    question: "What if I need something the smaller copy left out?",
    answer: "Before writing a separate Claude Code or Codex copy, Gobstopper archives the original and prepared bytes. Search the local vault for a missing record, read its saved text, or run `gobstopper undo` to prepare a restored copy with a new session identity. Keep backups of the vault: recovery depends on your storage.",
  },
  {
    question: "Does it edit my live session?",
    answer: "The proxy changes outgoing requests and leaves session files unchanged. File commands prepare separate copies. Gobstopper does not trigger Claude Code's or Codex's own compaction and does not edit session files in place. Automatic provider compaction stays disabled.",
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
        <MarketingPage>
          <ProductHero
            backdrop={false}
            align="start"
            actions={[
              { href: "#how", label: productMessaging.hero.secondaryAction, emphasis: "secondary" },
            ]}
            heading={heading}
            headingId="hero-title"
            install={<PlatformInstall platforms={installPlatforms} />}
            name=""
            summary={summary}
          />

          <MarketingSection
            heading={productMessaging.headings["home-live"]}
            headingId="how-title"
            id="how"
            label="While you work"
            summary="Start Claude Code through Gobstopper. Small requests pass through unchanged; large ones keep the system prompt, the first task, and the newest turns word for word while older turns become a summary."
          >
            <Terminal title="Run a session" code="gobstopper proxy run -- claude" />
            <GobMeterShowcase />
            <p>
              The proxy starts on a free local port and stops when the agent exits.
              Its default threshold is 128,000 estimated tokens. It uses local rules
              to summarize, with no extra model call, and leaves your session files unchanged.
            </p>
            <div className="gob-agent-marks">
              {agents.map((agent) => <ProviderMarkChip key={agent} mark={agent} size={32} />)}
            </div>
            <p>
              Claude Code and Codex routing is live-checked. Other agents need a
              configurable provider address; OpenAI Chat Completions support is
              tested with synthetic histories. <a href="/docs#compact-live-coding-agent-requests">Proxy setup and options</a>.
            </p>
          </MarketingSection>

          <MarketingPrimitives
            heading={productMessaging.headings["home-file-copy"]}
            headingId="saved-title"
            id="saved"
            label="Saved sessions"
            items={[
              { label: productMessaging.headings["home-primitive-preview"], summary: "Find a Claude Code or Codex session and inspect what a strategy would remove before writing anything." },
              { label: productMessaging.headings["home-primitive-original"], summary: "Compaction creates a separate copy and saves the original bytes in a local vault. Search that vault or prepare a restored copy when you need an older detail." },
              { label: productMessaging.headings["home-primitive-resume"], summary: "Copy checks cover record links, order, and tool-call pairs. They cannot guarantee provider resume, retention of every task fact, or what you will be billed." },
            ]}
          />
          <div className="gob-home-demo">
            <GobVaultSteps />
            <Terminal title="Preview a saved session" code={`gobstopper detect\ngobstopper plan <session> --trigger 250000`} />
          </div>


          <MarketingSection
            heading={`About as many tasks solved, ${F.inputFewer} fewer tokens sent.`}
            headingId="terminal-bench-title"
            id="terminal-bench"
            label="Terminal-Bench 2.1"
            summary={`One recorded run: Gobstopper at tail 0 resolved ${F.solved.tail0} of 89 tasks against ${F.solved.no_proxy} with no proxy, within single-trial noise, and sent ${F.inputFewer} fewer input tokens. The benchmarks page has the setup and limits.`}
          >
            <GobTokens variant="home" />
            <p>
              These results cover one model and setup; they do not establish task-quality or billing improvements for other agents.{" "}
              <a href="/benchmarks#terminal-bench-2026-09-28">Setup, statistics and downloads</a>.
            </p>
          </MarketingSection>

          {gobFilm === null ? null : (
            <MarketingSection
              heading={productMessaging.headings["home-film"]}
              headingId="film-title"
              id="film"
              label="Film"
              summary="No narration. Captions carry every line. The film never plays until you press play."
            >
              <GobFilm film={gobFilm} />
              <p><a href="/blog/introducing-gobstopper">Read the launch post</a>.</p>
            </MarketingSection>
          )}

          <MarketingInstallPanel
            eyebrow="Install"
            heading={productMessaging.headings["home-install"]}
            headingId="install-title"
            id="install"
            note={<p>Each installer downloads the latest release for your platform, checks its SHA-256, and installs it for your user only. The proxy needs curl 8.3 or newer.</p>}
          >
            <PlatformBadges platforms={["macos", "linux", { id: "windows", note: "partial" }]} />
            <p className="install-note">
              Then run one Claude Code session through the proxy: <code>gobstopper proxy run -- claude</code>.
            </p>
            {publishedRelease === null ? (
              <p className="install-note">No release yet.</p>
            ) : (
              <p className="install-note">Latest tagged release: <a href={`${repository}/releases/tag/v${releaseVersion}`}>v{releaseVersion}</a>.</p>
            )}
            <p className="install-note">
              Built-in inspection makes no model call. An optional remote scorer
              receives selected transcript text; trusted plugins run your code.{" "}
              <a href="/docs#install--use">Installation and command reference</a>.
            </p>
          </MarketingInstallPanel>

          <MarketingQuestionList
            heading={productMessaging.headings["home-questions"]}
            headingId="questions-title"
            id="questions"
            label="Questions"
            questions={questions.map(({ answer, question }) => ({ answer: <p>{renderInline(answer)}</p>, question }))}
          />
          <MarketingRelated
            groups={portfolioRelatedGroups(["xcb"])}
            heading="Other tools from our studio"
            headingId="related-title"
          />
        </MarketingPage>
      </main>
      <SiteFooter path="/" />
    </div>
  );
}

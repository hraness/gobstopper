import {
  MarketingCardRow,
  MarketingInstallPanel,
  MarketingPage,
  MarketingPrimitives,
  MarketingProofFrame,
  MarketingQuestionList,
  MarketingRelated,
  MarketingSection,
  ProductHero,
  ProviderMarkChip,
  SyntaxCode,
} from "@hraness/design-kit/react/server";
import { portfolioRelatedGroups } from "@hraness/design-kit/portfolio";
import { PlatformBadges, PlatformInstall } from "@hraness/design-kit/react";

import { Terminal } from "./_components/code-block";
import { FounderNote } from "./_components/founder-note";
import { GobBenchTable } from "./_components/gob-bench-table";
import { GobFilm } from "./_components/gob-film";
import { GobMechanismDiagram } from "./_components/gob-mechanism";
import { GobCompactionSignature } from "./_components/gob-signature";
import { GobVaultSteps } from "./_mockups/gob-mockups";
import { cli, commands } from "./_mockups/fixtures";
import { Testimonial } from "./_components/testimonial";
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

const boundary = releaseVersion === undefined
  ? "Free and open source · MIT or Apache-2.0"
  : `Free and open source · MIT or Apache-2.0 · Latest release: v${releaseVersion}`;

/** Real `gobstopper plan` output: a 130,060-token session previewed at a 100,000-token trigger. */
const proofTranscript = [`$ ${commands.plan}`, ...cli.plan].join("\n");

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
    answer: "You or the agent can reserve a larger context for a difficult phase, within the window the model and client support. The budget expires by time or request count. Optional adaptive rescue raises the threshold for a while after repeated reads of unchanged output the proxy removed, and Gobstopper keeps selected original tool results and images through repeated compactions. See [context budgets and retention](/docs#temporary-context-budgets).",
  },
  {
    question: "What happens if the proxy stops responding?",
    answer: "Request and compaction deadlines limit stalls, while logging and metrics run in background workers. `gobstopper proxy launch --client claude` checks readiness before opening Claude Code and can start directly with its official provider when the configuration permits. See [recovery and fallback](/docs#startup-recovery-and-direct-fallback).",
  },
  {
    question: "Can it start automatically and keep my laptop awake?",
    answer: "Run `gobstopper proxy install` to start a user service at login and restart it after a process exit. Service changes wait for active inference to finish, and during inference Gobstopper asks the system not to idle-sleep. Local session metrics are available through `gobstopper data metrics`. See [startup and local visibility](/docs#longer-work-local-visibility-and-startup-recovery).",
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
              <MarketingProofFrame
                chrome="terminal"
                className="gob-hero-proof"
                credit="Real output from a release build on September 29, 2026, against a synthetic Claude Code session; the project and user are made up."
                title="~/code/lanternshop"
              >
                <pre tabIndex={0}><SyntaxCode code={proofTranscript} language="shell" styles="classes" /></pre>
              </MarketingProofFrame>
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
            heading="Compact more often. Keep everything."
            headingId="idea-title"
            id="idea"
            label="The idea"
            summary="Every request from a coding agent carries the whole session, including output that mattered once. Gobstopper compacts earlier and more often than the built-in step, so the model reads less on every step, and the full history stays on your machine."
          >
            <GobCompactionSignature />
          </MarketingSection>

          <MarketingSection
            heading={productMessaging.headings["home-live"]}
            headingId="how-title"
            id="how"
            label="How it works"
            summary="Start your agent through Gobstopper. Small requests pass through unchanged. Past the threshold, the proxy keeps the system prompt, the first task, and the newest turns word for word, and summarizes the older turns with local rules, with no extra model call."
          >
            <GobMechanismDiagram />
            <Terminal title="Run a session" code={commands.proxyRun} />
            <p>
              The proxy starts on a free local port and stops when the agent exits. Its default
              threshold is 128,000 estimated tokens. Session files on disk stay unchanged.
            </p>
            <div className="gob-agent-marks">
              {agents.map((agent) => <ProviderMarkChip key={agent} mark={agent} size={32} />)}
            </div>
            <p>
              Claude Code and Codex start through it with one command. opencode, Crush, Aider,
              Goose, and any other client of the Anthropic Messages, OpenAI Responses, or OpenAI
              Chat Completions dialect point at its local address.{" "}
              <a href="/docs/proxy">Proxy setup and options</a>.
            </p>
          </MarketingSection>

          <MarketingPrimitives
            heading={productMessaging.headings["home-file-copy"]}
            headingId="saved-title"
            id="saved"
            label="Saved sessions"
            items={[
              { label: productMessaging.headings["home-primitive-preview"], summary: "Find a Claude Code or Codex session and see what a strategy would remove before writing anything." },
              { label: productMessaging.headings["home-primitive-original"], summary: "Compaction writes a separate copy and saves the original bytes in the local vault. Search the vault or restore the original when you need an older detail." },
              { label: productMessaging.headings["home-primitive-resume"], summary: "Every copy is checked for record links, order, and tool-call pairs before you resume it." },
            ]}
          />
          <div className="gob-home-demo">
            <GobVaultSteps />
            <Terminal title="Preview a saved session" code={`gobstopper detect\ngobstopper plan <session> --trigger 250000`} />
          </div>

          <MarketingSection
            heading={`${F.inputFewer} fewer input tokens, as many tasks solved.`}
            headingId="terminal-bench-title"
            id="terminal-bench"
            label="Terminal-Bench 2.1"
            summary={`Claude Code through Gobstopper at tail 0 and a 45,000-token threshold solved ${F.solved.tail0} of 89 tasks against ${F.solved.no_proxy} with its built-in compaction, and sent ${F.inputFewer} fewer input tokens: ${F.input.tail0} against ${F.input.no_proxy}. Most of the gap is cache reads, the context resent on every step.`}
          >
            <GobBenchTable />
            <p>
              <a href="/benchmarks#terminal-bench-2026-09-28">Setup, statistics, and downloads</a> ·{" "}
              <a href="/methodology">How Gobstopper measures</a>.
            </p>
          </MarketingSection>

          <MarketingSection
            heading="Measure what your agents sent."
            headingId="usage-title"
            id="usage"
            label="Usage history"
            summary="The installer adds aicharts beside Gobstopper and turns on its local usage history: your token use by day, agent, provider, and model, kept on this computer and never uploaded."
          >
            <Terminal title="Usage history" code={`gobstopper usage\ngobstopper usage report --days 7 --csv`} />
            <p>
              <code>gobstopper usage</code> reads the same aicharts record across every agent,
              including ones that never pass through the proxy, and agents read it back through
              the read-only <code>aicharts mcp</code> tools.{" "}
              <a href="https://aicharts.io/usage">aicharts usage</a>.
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
            <PlatformInstall platforms={installPlatforms} />
            <PlatformBadges platforms={["macos", "linux", { id: "windows", note: "partial" }]} />
            <p className="install-note">
              Then run one session through the proxy: <code>gobstopper proxy run -- claude</code>.
            </p>
            {publishedRelease === null ? (
              <p className="install-note">No release yet.</p>
            ) : (
              <p className="install-note">Latest tagged release: <a href={`${repository}/releases/tag/v${releaseVersion}`}>v{releaseVersion}</a>.</p>
            )}
            <p className="install-note">
              Built-in compaction makes no model call. An optional remote scorer receives selected
              transcript text; plugins you install run your code.{" "}
              <a href="/docs#install--use">Installation and command reference</a>.
            </p>
            <div className="gob-limits" id="limits">
              <p className="gob-limits__heading">Limits</p>
              <ul>
                <li>The Terminal-Bench 2.1 result is one trial per arm with one model, GLM 5.3 Flash, at a 45,000-token threshold rather than the 128,000 default; the two-task gap in tasks solved is within single-trial noise, and 21 of the 89 tail-0 trials may have run an earlier build.</li>
                <li>Claude Code and Codex routing is checked against live sessions; OpenAI Chat Completions routing is tested against synthetic histories.</li>
                <li>On Windows the proxy works; the vault, watch, and provider hooks need macOS or Linux.</li>
                <li>The vault lives on your disk and is only as safe as your backups. Copy checks cover record links, order, and tool-call pairs; they cannot promise that a provider resumes the copy or that a summary kept every fact.</li>
              </ul>
            </div>
          </MarketingInstallPanel>

          <MarketingSection
            heading="How it compares."
            headingId="compare-title"
            id="compare"
            label="Compare"
            summary="Two side-by-side pages: the compaction built into Claude Code, and CliffCompaction, the Python proxy whose summary rule Gobstopper ports to Rust."
          >
            <MarketingCardRow
              ariaLabel="Comparisons"
              cards={[
                { href: "/compare/claude-code-compact", title: "Claude Code /compact", meta: "A model-written summary on demand, against local rules that run on every request and keep the original." },
                { href: "/compare/cliffcompaction", title: "CliffCompaction", meta: "The same summary rule in Python, against a Rust port with a local vault, undo, and nine documented departures." },
              ]}
              columns={2}
            />
          </MarketingSection>

          <MarketingQuestionList
            heading={productMessaging.headings["home-questions"]}
            headingId="questions-title"
            id="questions"
            label="Questions"
            questions={questions.map(({ answer, question }) => ({ answer: <p>{renderInline(answer)}</p>, question }))}
          />
          <MarketingRelated
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

import type { Metadata } from "next";
import { socialImageAlt } from "@hraness/web-discovery/social-image";
import { SyntaxCode } from "@hraness/design-kit/react/server";
import { Terminal } from "../../_components/code-block";

import { SiteDocument } from "../../_components/site-document";
import { SiteHeader, SiteFooter } from "../../_components/site-chrome";
import { GITHUB_URL } from "../../_lib/site";
import {
  CLIFF_BLOG,
  CLIFF_PAPER,
  CLIFF_PYPI,
  CLIFF_REPOSITORY,
  comparisonQuestions,
  comparisonRows,
  departureRows,
} from "./comparison";
import { socialSite } from "../../social";
import "./departures.css";

const title = "Gobstopper vs CliffCompaction";
const socialTitle = "Gobstopper compared with CliffCompaction";
const description =
  "A CliffCompaction alternative: gobstopper proxy ports its summary rule to a Rust binary and adds previewed, archived copies of saved agent sessions.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/compare/cliffcompaction" },
  openGraph: {
    title: socialTitle,
    description,
    siteName: "Gobstopper",
    type: "article",
    url: "/compare/cliffcompaction",
    images: [{ url: "/opengraph-image", width: 1200, height: 630, alt: socialImageAlt(socialSite) }],
  },
  twitter: {
    card: "summary_large_image",
    title: socialTitle,
    description,
    images: [{ url: "/opengraph-image", alt: socialImageAlt(socialSite) }],
  },
};

function withCode(text: string) {
  return text.split("`").map((part, index) => (index % 2 === 1 ? <code key={index}>{part}</code> : part));
}

const structuredData = {
  "@context": "https://schema.org",
  "@type": "FAQPage",
  mainEntity: comparisonQuestions.map(({ answer, question }) => ({
    "@type": "Question",
    acceptedAnswer: { "@type": "Answer", text: answer.replaceAll("`", "") },
    name: question,
  })),
};

export default function CompareCliffCompaction() {
  return (
    <>
      <script
        dangerouslySetInnerHTML={{ __html: JSON.stringify(structuredData) }}
        type="application/ld+json"
      />
      <SiteHeader path="/compare/cliffcompaction" />
      <main id="main" tabIndex={-1}>
        <SiteDocument
          dek="CliffCompaction and Gobstopper build mechanical summaries and keep the newest turns untouched. Gobstopper adds selected evidence retention and temporary context budgets."
          eyebrow="Comparison"
          heading={title}
          meta="Checked against the CliffCompaction paper and repository and Gobstopper's source on September 28, 2026; Gobstopper behavior updated from source on September 30, 2026"
          toc={[
            { href: "#cliffcompaction-rule", label: "CliffCompaction summarizes older turns mechanically" },
            { href: "#gobstopper-port", label: "Gobstopper ports the rule and adds archived copies" },
            { href: "#departures", label: "What Gobstopper adds to the request engine" },
            { href: "#side-by-side", label: "Side-by-side comparison" },
            { href: "#cliff-strategy", label: "The cliff strategy applies the rule to saved sessions" },
            { href: "#when-to-use-each", label: "When to use each" },
            { href: "#questions", label: "Questions" },
            { href: "#sources", label: "Sources" },
          ]}
        >
          <p>
            Pick CliffCompaction for the Python proxy the paper measured.
            Pick <code>gobstopper proxy</code> for a single Rust binary that
            carries your own words across compactions and can replay a recorded
            session without calling a provider. CliffCompaction&apos;s authors
            report task results on Terminal-Bench, SWE-bench Verified, and
            KernelBench; Gobstopper has one Terminal-Bench 2.1 trial.
          </p>
          <p>
            CliffCompaction is the research proxy that introduced the rule.{" "}
            <code>gobstopper proxy</code> ports it to the Anthropic Messages,
            OpenAI Responses, and Chat Completions dialects, for Claude Code,
            Codex, opencode, Crush, Aider, Goose, and other agents with a
            configurable provider address. Gobstopper also has file commands
            that prepare compacted copies of saved sessions for you to inspect
            and then resume. The proxy has shipped since v0.3.1 and the Chat
            Completions dialect since v0.4.0. Carrying the conversation&apos;s
            words across compactions, described in the table below, shipped
            in v0.6.0, and v0.7.3 made the reference tail of three turns the
            default.
          </p>

          <h2 id="cliffcompaction-rule">CliffCompaction summarizes older turns mechanically</h2>
          <p>
            <a href={CLIFF_REPOSITORY}>CliffCompaction</a> is an open-source
            (MIT) API proxy for coding agents by Trang Nguyen, Eulrang Cho,
            Bingqing Chen, and Tim Dettmers, described in{" "}
            <a href={CLIFF_PAPER}>arXiv:2609.26779</a> (September 2026) and
            published on PyPI as <a href={CLIFF_PYPI}><code>cliffcompaction</code></a>.
            You point an agent&apos;s base URL at it. When a request exceeds a
            token threshold, the proxy sends the system prompt and task
            verbatim, then one mechanical summary of the older turns, then the
            last three turns verbatim. The summary keeps tool results of at
            most 500 characters, drops longer ones because the files behind
            them are still readable, reduces tool calls to one-line signatures,
            and keeps assistant text. Each later compaction is rebuilt from the
            original history the agent resends, and the previous summary is
            discarded; the authors call this never compacting a compaction.
          </p>
          <p>
            Each compacted request is smaller, and between compactions every
            request repeats the same compacted prefix, so the provider&apos;s
            prompt cache keeps matching until the next compaction. The paper
            reports up to 50% lower cost at a bounded context with
            maintained or improved Terminal-Bench 2.0 results for the Kimi and
            GLM models the authors tested, plus SWE-bench Verified and
            KernelBench results. In one Terminal-Bench 2.1 run through Claude
            Code with GLM 5.3 Flash, their proxy scored 76.69% against 70.97%
            for Claude Code&apos;s own auto-compaction at about 45,000 tokens
            and 73.03% for its default 200,000-token setting. The cost
            figures model perfect prompt caching rather than metered bills,
            and the authors report that the benefit depends on the agent and
            the task. Those are the authors&apos; figures for their proxy.
            Gobstopper&apos;s own Terminal-Bench 2.1 run, of{" "}
            <code>gobstopper proxy</code> against Claude Code with no proxy,
            is on the{" "}
            <a href="/benchmarks#terminal-bench-2026-09-28">benchmarks page</a>;
            it had no CliffCompaction arm.
          </p>

          <h2 id="gobstopper-port">Gobstopper ports the rule and adds archived copies</h2>
          <p>
            Gobstopper makes long Claude Code and Codex sessions smaller. For a
            saved session, you choose a threshold and a strategy, preview the
            cut, and compare strategies on the same frozen bytes. Before it
            writes a copy, it archives the source and candidate bytes in a
            local vault, so you can search for an archived record and read it
            later.
          </p>
          <p>
            For a running session, <code>gobstopper proxy</code> listens on
            127.0.0.1 between the agent and its provider. Past the
            threshold (128,000 estimated tokens by default, or 256,000 for
            an Anthropic request that declares a 1M-token window) it sends
            the head, one mechanical summary with selected original evidence,
            and the newest three turns (more with <code>--keep-tail-percent</code>).
            The provider reports a smaller context, which can delay the
            client&apos;s own auto-compaction. The session files stay unchanged.
          </p>
          <Terminal code={`gobstopper proxy run -- claude       # one session through a temporary proxy
gobstopper proxy serve               # background proxy on http://127.0.0.1:8260
gobstopper proxy replay <session>    # what the proxy would have sent; calls no provider`} />

          <h2 id="departures">What Gobstopper adds to the request engine</h2>
          <p>
            The summary format, prefix reuse between compactions, the harsher
            settings applied when one pass leaves a request over the
            threshold, and the retry after a length rejection are
            CliffCompaction&apos;s. Gobstopper adds the behaviors below.
            Temporary context budgets require a configured capacity supported
            by your provider and client. Evidence retention has finite limits;
            neither feature guarantees that every task fact survives.
          </p>
          <figure className="gob-departures">
            <table>
              <caption>How gobstopper proxy departs from CliffCompaction&apos;s request engine, and how to restore the reference</caption>
              <thead>
                <tr>
                  <th scope="col">Departure</th>
                  <th scope="col">Default</th>
                  <th scope="col">Restore the reference</th>
                </tr>
              </thead>
              <tbody>
                {departureRows.map((row) => (
                  <tr key={row.departure}>
                    <th scope="row">{withCode(row.departure)}</th>
                    <td data-label="Default">{withCode(row.byDefault)}</td>
                    <td data-label="Restore">{withCode(row.restore)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </figure>

          <h2 id="side-by-side">Side-by-side comparison</h2>
          <figure>
            <table>
              <caption>CliffCompaction checked September 28, 2026; Gobstopper source checked September 30, 2026</caption>
              <thead>
                <tr>
                  <th scope="col">Aspect</th>
                  <th scope="col">CliffCompaction</th>
                  <th scope="col">Gobstopper</th>
                </tr>
              </thead>
              <tbody>
                {comparisonRows.map((row) => (
                  <tr key={row.aspect}>
                    <th scope="row">{row.aspect}</th>
                    <td>{withCode(row.cliff)}</td>
                    <td>{withCode(row.gobstopper)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </figure>

          <h2 id="cliff-strategy">The cliff strategy applies the rule to saved sessions</h2>
          <p>
            <code>cliff</code> keeps the head (the system prompt and the first
            user prompt) and the newest <code>keep_recent_turns</code> assistant
            steps byte-for-byte, drops older tool results larger than{" "}
            <code>result_max_bytes</code> except the newest{" "}
            <code>keep_recent_tool_outputs</code> (default 8), and leaves smaller results, user
            prompts, assistant text, and reasoning in place. The defaults are
            three steps and 500 bytes, the proxy&apos;s defaults. A step starts
            where the assistant side resumes after a user prompt or a tool
            result and includes the tool results that answer it. Nothing is
            summarized, no state card is added, and there is no floor to reach:
            the copy is as small as the rule makes it.
          </p>
          <p>
            When both passes run at the same cut and both produce a plan, the records dropped from the source and then from a{" "}
            <code>cliff</code> copy are, together, the records a single compaction from the source would drop.
            That is the file-side version of never compacting a compaction; a unit test
            in the repository checks one synthetic case. A copy below the trigger or the minimum savings is not compacted again, so the two paths can differ. Two parts of the proxy&apos;s rule are not
            part of the copy: tool-call signatures and reasoning caps, because
            Gobstopper&apos;s copy transforms replace tool-result payloads only.
            Codex <code>compacted</code> records count as one result.
          </p>
          <Terminal code={`gobstopper plan <session> --strategy cliff
gobstopper eval <session>`} />
          <p>Save the same choices in <code>~/.config/gobstopper/config.toml</code>:</p>
          <pre><SyntaxCode code={`[presets.cliff]
strategy = "cliff"
keep_recent_turns = 3
result_max_bytes = 500
keep_recent_tool_outputs = 0`} language="text" styles="classes" /></pre>

          <h2 id="when-to-use-each">When to use each</h2>
          <p>
            Use CliffCompaction for request-time compaction in any client that
            speaks the Anthropic Messages, OpenAI Chat Completions, or OpenAI
            Responses API. Use <code>gobstopper proxy</code> for the same
            dialects when you want a single Rust binary, an optional tail
            budget that keeps more recent turns while they fit, and{" "}
            <code>proxy replay</code> to see what the proxy would have sent
            for a recorded Claude Code or Codex session. Use Gobstopper&apos;s file commands to
            compare strategies on frozen input, keep the source, and resume a
            smaller copy. Run one proxy per client. For the built-in
            alternative, see{" "}
            <a href="/compare/claude-code-compact">Gobstopper compared with Claude Code /compact</a>.
          </p>

          <h2 id="questions">Questions</h2>
          {comparisonQuestions.map(({ answer, question }) => (
            <section key={question}>
              <h3>{question}</h3>
              <p>{withCode(answer)}</p>
            </section>
          ))}

          <h2 id="sources">Sources</h2>
          <ul>
            <li>
              Trang Nguyen, Eulrang Cho, Bingqing Chen, and Tim Dettmers,{" "}
              <a href={CLIFF_PAPER}>CliffCompaction: Cost-Efficient Compaction for Long-Horizon Coding Agents</a>,
              arXiv:2609.26779, September 2026.
            </li>
            <li><a href={CLIFF_REPOSITORY}>CliffCompaction source and README</a> (MIT).</li>
            <li><a href={CLIFF_BLOG}>The authors&apos; project page</a>.</li>
            <li><a href={`${GITHUB_URL}/blob/main/README.md`}>Gobstopper README</a>, which carries the same comparison table.</li>
          </ul>
        </SiteDocument>
      </main>
      <SiteFooter path="/compare/cliffcompaction" />
    </>
  );
}

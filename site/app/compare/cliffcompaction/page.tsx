import type { Metadata } from "next";

import { SiteHeader, SiteFooter } from "../../_components/site-chrome";
import { GITHUB_URL } from "../../_lib/site";
import {
  CLIFF_BLOG,
  CLIFF_PAPER,
  CLIFF_PYPI,
  CLIFF_REPOSITORY,
  comparisonQuestions,
  comparisonRows,
} from "./comparison";

const title = "Compared with CliffCompaction";
const socialTitle = "Gobstopper compared with CliffCompaction";
const description =
  "CliffCompaction and gobstopper proxy compact live API requests with the same rule. Gobstopper also prepares compacted copies of saved sessions. This page compares what each keeps, drops, and measures.";

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
    images: [{ url: "/opengraph-image", width: 1200, height: 630, alt: socialTitle }],
  },
  twitter: {
    card: "summary_large_image",
    title: socialTitle,
    description,
    images: [{ url: "/opengraph-image", alt: socialTitle }],
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
      <main id="main" tabIndex={-1} className="document-page">
        <article>
          <h1>Gobstopper compared with CliffCompaction</h1>
          <p>
            Both tools can shrink a coding agent&apos;s context without asking a
            model to summarize it, and both keep the newest turns untouched.
            CliffCompaction compacts each API request through a local proxy
            while the session runs. <code>gobstopper proxy</code> ports the same
            rule for Claude Code and Codex, and Gobstopper&apos;s file commands
            prepare compacted copies of saved sessions that you inspect and then
            resume. The proxy is in the current source build and not in a tagged
            release yet; install from source to use it.
          </p>

          <h2>What CliffCompaction does</h2>
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
            The paper reports up to 50% lower cost at a bounded context with
            maintained or improved Terminal-Bench 2.0 results for the Kimi and
            GLM models the authors tested, plus SWE-bench Verified and
            KernelBench results. Those are the authors&apos; figures for their
            proxy. Gobstopper has not run those benchmarks.
          </p>

          <h2>What Gobstopper does</h2>
          <p>
            Gobstopper makes long Claude Code and Codex sessions smaller. You
            choose a threshold and a strategy, preview the cut, and compare
            strategies on the same frozen bytes. Before it writes a Claude
            Code or Codex copy, it archives the source and candidate bytes in
            a local vault, so an exact archived record can be searched and
            read later.
          </p>
          <p>
            For a running session, <code>gobstopper proxy</code> listens on
            127.0.0.1 between Claude Code or Codex and the provider. Past the
            threshold (128,000 estimated tokens by default) it sends the head,
            one mechanical summary, and the newest three turns, so the provider
            reports a smaller context and the client&apos;s own auto-compaction
            does not reach its trigger. The session files stay unchanged. Devin
            CLI cannot use the proxy because it sends requests through
            Cognition&apos;s service and has no setting for a model address.
          </p>
          <pre tabIndex={0}><code>{`gobstopper proxy run -- claude       # one session through a temporary proxy
gobstopper proxy serve               # background proxy on http://127.0.0.1:8260
gobstopper proxy replay <session>    # what the proxy would have sent; calls no provider`}</code></pre>

          <h2>How they compare</h2>
          <table>
            <caption>Read from each tool&apos;s documentation and source on September 25, 2026</caption>
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

          <h2>The cliff strategy</h2>
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
            That is the file-side analogue of never compacting a compaction; a unit test
            in the repository checks one synthetic case. A copy below the trigger or the minimum savings is not compacted again, so the two paths can differ. Two parts of the proxy&apos;s rule are not
            part of the copy: tool-call signatures and reasoning caps, because
            Gobstopper&apos;s copy transforms replace tool-result payloads only.
            Codex <code>compacted</code> records count as one result.
          </p>
          <pre tabIndex={0}><code>{`gobstopper plan <session> --strategy cliff
gobstopper eval <session>      # cliff appears beside the other strategies

# ~/.config/gobstopper/config.toml
[presets.cliff]
strategy = "cliff"
keep_recent_turns = 3
result_max_bytes = 500
keep_recent_tool_outputs = 0`}</code></pre>

          <h2>Which one fits</h2>
          <p>
            Use CliffCompaction for request-time compaction in any client that
            speaks the Anthropic Messages, OpenAI Chat Completions, or OpenAI
            Responses API. Use <code>gobstopper proxy</code> for Claude Code or
            Codex when you also want to replay a recorded session and see what
            the proxy would have sent. Use Gobstopper&apos;s file commands to
            compare strategies on frozen input, keep the exact source, and
            resume a smaller copy. Run one proxy per client.
          </p>

          <h2>Questions</h2>
          {comparisonQuestions.map(({ answer, question }) => (
            <section key={question}>
              <h3>{question}</h3>
              <p>{withCode(answer)}</p>
            </section>
          ))}

          <h2>Sources</h2>
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
        </article>
      </main>
      <SiteFooter path="/compare/cliffcompaction" />
    </>
  );
}

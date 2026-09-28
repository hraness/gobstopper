import type { Metadata } from "next";
import { socialImageAlt } from "@hraness/web-discovery/social-image";
import { SyntaxCode } from "@hraness/design-kit/react/server";

import { SiteDocument } from "../../_components/site-document";
import { SiteHeader, SiteFooter } from "../../_components/site-chrome";
import { GITHUB_URL } from "../../_lib/site";
import {
  CLAUDE_BLOG,
  CLAUDE_CHECKPOINTING,
  CLAUDE_COMMANDS,
  CLAUDE_CONTEXT_WINDOW,
  CLAUDE_PROMPT_CACHING,
  CLAUDE_SESSIONS,
  comparisonQuestions,
  comparisonRows,
} from "./comparison";
import { socialSite } from "../../social";

const title = "Gobstopper vs Claude Code /compact";
const socialTitle = "Gobstopper compared with Claude Code /compact";
const description =
  "Claude Code's /compact swaps history for a model-written summary with no preview. Gobstopper previews the cut, archives the original, and can undo it.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/compare/claude-code-compact" },
  openGraph: {
    title: socialTitle,
    description,
    siteName: "Gobstopper",
    type: "article",
    url: "/compare/claude-code-compact",
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

export default function CompareClaudeCodeCompact() {
  return (
    <>
      <script
        dangerouslySetInnerHTML={{ __html: JSON.stringify(structuredData) }}
        type="application/ld+json"
      />
      <SiteHeader path="/compare/claude-code-compact" />
      <main id="main" tabIndex={-1}>
        <SiteDocument
          dek="Claude Code's /compact swaps history for a model-written summary with no preview; Gobstopper previews the cut, archives the original, and can undo it."
          eyebrow="Comparison"
          heading={title}
          meta="Checked against Anthropic's Claude Code documentation and Gobstopper's README and source on September 26, 2026"
          toc={[
            { href: "#compact-summary", label: "/compact replaces history with a model-written summary" },
            { href: "#gobstopper-copy", label: "Gobstopper previews the cut and archives the original" },
            { href: "#side-by-side", label: "Side-by-side comparison" },
            { href: "#when-to-use-each", label: "When to use each" },
            { href: "#questions", label: "Questions" },
            { href: "#sources", label: "Sources" },
          ]}
        >
          <p>
            Claude Code&apos;s <code>/compact</code> command and Gobstopper both
            make a long session&apos;s context smaller. They differ in what
            happens to the history. <code>/compact</code> replaces the running
            session&apos;s messages with a summary the model writes at that
            moment, and you see what it kept only afterward. Gobstopper shows
            the cut first, writes a separate compacted copy, and keeps the
            source bytes in a local vault you can search later. For a session
            that is still running, <code>gobstopper proxy</code> keeps each
            request small enough that Claude Code&apos;s auto-compaction does
            not reach its trigger. If a model-chosen summary is good enough
            and you want nothing to install, <code>/compact</code> is the
            simpler choice.
          </p>

          <h2 id="compact-summary">/compact replaces the history with a model-written summary</h2>
          <p>
            <code>/compact</code> is built into Claude Code. To produce the
            summary, Claude Code sends a separate request carrying the same
            system prompt, tools, and history as your conversation, plus a
            summarization instruction, then replaces the in-context message
            history with what the model writes. Passing instructions, such as{" "}
            <code>/compact focus on the auth bug fix</code>, steers what the
            summary keeps; there is no preview of what it will keep or drop.
            Claude Code also compacts automatically as the context approaches
            the model&apos;s limit, and <code>/autocompact</code> sets how full
            the window gets first. After a compaction, Claude Code reloads its
            startup content: CLAUDE.md files, unscoped rules, and auto memory come back
            from disk, up to five of the files most recently modified are
            re-read, and invoked skill bodies are re-injected.
          </p>
          <p>
            The original messages stay in the session transcript file, and{" "}
            <code>/rewind</code> can restore the conversation to an earlier
            checkpoint while that checkpoint&apos;s snapshots remain. The loss
            is in the context every later request carries: whatever the
            summary left out is no longer in it. Anthropic&apos;s{" "}
            <a href={CLAUDE_BLOG}>session-management guide</a> calls the trade
            &ldquo;lossy&rdquo; and notes that the model is at its least
            intelligent point when it compacts, because context rot has already
            set in. If the dropped detail matters, recovering it means finding
            it in the transcript yourself and pasting it back.
          </p>

          <h2 id="gobstopper-copy">Gobstopper previews the cut and archives the original</h2>
          <p>
            Gobstopper inspects saved Claude Code and Codex session files and
            prepares compacted copies. <code>gobstopper plan</code> shows what a
            strategy would cut at a threshold you choose,{" "}
            <code>gobstopper eval</code> compares strategies on the same frozen
            bytes, and <code>gobstopper diff</code> compares two archived
            snapshots. <code>gobstopper apply</code> writes the copy as a new
            fork under a fresh session ID and never changes the source file.
            Before a copy is published, a local vault stores the source and
            candidate bytes by content hash. <code>gobstopper undo</code> can
            then restore a snapshot into a new fork, and{" "}
            <code>search-snapshot</code> and <code>read-snapshot</code> can
            return a record a strategy left out. The built-in strategies are
            local rules and make no model calls. Test that Claude Code or Codex
            resumes a copy before you rely on it.
          </p>
          <p>
            For a running session, <code>gobstopper proxy</code> listens on
            127.0.0.1 between the agent and its provider. Past the threshold
            (128,000 estimated tokens by default) it sends the head, one
            mechanical summary, and the last three turns verbatim, so the
            provider reports a smaller context and the client&apos;s own
            auto-compaction does not reach its trigger. The session files stay
            unchanged. In one Terminal-Bench 2.1 run through Claude Code (GLM
            5.3 Flash, 45,000-token threshold, one trial per arm, September 27
            and 28, 2026), it solved 61 of 89 tasks against 60 with no proxy,
            within single-trial noise, and sent 29% fewer input tokens; it had
            no arm with Claude Code&apos;s own auto-compaction at that size.
            The{" "}
            <a href="/benchmarks#terminal-bench-2026-09-28">benchmarks page</a>{" "}
            has the setup and limits.
          </p>
          <pre><SyntaxCode code={`gobstopper detect                 # sessions, context sizes
gobstopper plan <session>         # preview the cut under each strategy
gobstopper apply <session>        # write the compacted copy as a new fork
gobstopper undo <session>         # restore a vaulted snapshot into a new fork`} language="sh" /></pre>

          <h2 id="side-by-side">Side-by-side comparison</h2>
          <figure>
            <table>
            <caption>Checked against Anthropic&apos;s Claude Code documentation and blog and Gobstopper&apos;s README and source on September 26, 2026</caption>
            <thead>
              <tr>
                <th scope="col">Aspect</th>
                <th scope="col">Claude Code /compact</th>
                <th scope="col">Gobstopper</th>
              </tr>
            </thead>
            <tbody>
              {comparisonRows.map((row) => (
                <tr key={row.aspect}>
                  <th scope="row">{row.aspect}</th>
                  <td>{withCode(row.compact)}</td>
                  <td>{withCode(row.gobstopper)}</td>
                </tr>
              ))}
            </tbody>
            </table>
          </figure>

          <h2 id="when-to-use-each">When to use each</h2>
          <p>
            Run <code>/compact</code> when a live session is bloated and a
            model-chosen summary is an acceptable trade: it is already
            installed, it works in place, and focus instructions steer it. Run{" "}
            <code>gobstopper proxy</code> to keep a running Claude Code or Codex
            session under a threshold so auto-compaction does not fire mid-task.
            Use Gobstopper&apos;s file commands when you want to see the cut
            before it happens, keep the source bytes, and be able to undo into
            a new fork. To compare the proxy with the CliffCompaction research
            proxy, see{" "}
            <a href="/compare/cliffcompaction">Gobstopper compared with CliffCompaction</a>.
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
              <a href={CLAUDE_COMMANDS}>Claude Code commands reference</a>{" "}
              (<code>/compact</code>, <code>/autocompact</code>,{" "}
              <code>/rewind</code>).
            </li>
            <li>
              <a href={CLAUDE_CONTEXT_WINDOW}>Explore the context window</a>{" "}
              (what survives compaction; automatic compaction near the limit).
            </li>
            <li>
              <a href={CLAUDE_PROMPT_CACHING}>How Claude Code uses prompt caching</a>{" "}
              (the summarization request and what it costs).
            </li>
            <li>
              <a href={CLAUDE_CHECKPOINTING}>Checkpointing</a>{" "}
              (<code>/rewind</code>; original messages stay in the session
              transcript; snapshot retention).
            </li>
            <li>
              <a href={CLAUDE_SESSIONS}>Manage sessions</a>{" "}
              (resume from a summary; transcript files under{" "}
              <code>~/.claude/projects/</code>).
            </li>
            <li>
              Anthropic,{" "}
              <a href={CLAUDE_BLOG}>Using Claude Code: session management and 1M context</a>,
              April 15, 2026.
            </li>
            <li><a href={`${GITHUB_URL}/blob/main/README.md`}>Gobstopper README</a>, which carries the same comparison table.</li>
          </ul>
        </SiteDocument>
      </main>
      <SiteFooter path="/compare/claude-code-compact" />
    </>
  );
}

import type { Metadata } from "next";
import { ProviderMarkChip } from "@hraness/design-kit/react/server";
import { socialImageAlt } from "@hraness/web-discovery/social-image";

import { SiteDocument } from "../_components/site-document";
import { SiteHeader, SiteFooter } from "../_components/site-chrome";
import { readmeHtml, readmeSections } from "../readme.generated";
import { socialSite } from "../social";
import { docsSocialPage } from "./social-page";

const agents = ["claudecode", "codex", "opencode", "crush", "aider", "goose"] as const;

const title = "Documentation";
const socialTitle = "Gobstopper documentation";
const description =
  "The Gobstopper reference: how to install it, choose a compaction strategy, and configure it per provider, per session, or with named presets.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/docs" },
  openGraph: {
    title: socialTitle,
    description,
    siteName: "Gobstopper",
    type: "article",
    url: "/docs",
    images: [{ url: "/docs/opengraph-image", width: 1200, height: 630, alt: socialImageAlt(socialSite, docsSocialPage) }],
  },
  twitter: {
    card: "summary_large_image",
    title: socialTitle,
    description,
    images: [{ url: "/docs/opengraph-image", alt: socialImageAlt(socialSite, docsSocialPage) }],
  },
};

const body = readmeHtml.replace(/^<h1 id="[^"]*">[\s\S]*?<\/h1>\s*/u, "");

/** The reference, routed by what the reader wants to do. */
const tasks = [
  { href: "#install--use", title: "Install Gobstopper", meta: "Installers for macOS, Linux, and Windows, the build from source, and automatic updates." },
  { href: "/docs/proxy", title: "Run your agent through the proxy", meta: "Point Claude Code, Codex, opencode, Crush, Aider, or Goose at it, choose a threshold, and fix connection problems." },
  { href: "#longer-work-local-visibility-and-startup-recovery", title: "Keep a long task going", meta: "Reserve a larger context for a hard phase, read local counters, and restart the proxy after a crash." },
  { href: "#recoverable-history", title: "Shrink a saved session and keep a way back", meta: "Preview the cut, write a smaller copy, search the vault, and undo." },
  { href: "#strategies", title: "Choose a strategy", meta: "What each built-in strategy keeps and drops, and when auto picks it." },
  { href: "/benchmarks", title: "Read the measurements", meta: "Terminal-Bench 2.1, replays of recorded sessions, and the methodology behind them." },
] as const;

export default function Docs() {
  return (
    <>
      <SiteHeader path="/docs" />
      <main id="main" tabIndex={-1} data-hraness-landscape="page">
        <SiteDocument
          dek={description}
          eyebrow="Reference"
          heading={title}
          meta={
            <>

              <a href="https://github.com/hraness/gobstopper/blob/main/README.md">Source on GitHub</a>
            </>
          }
          toc={readmeSections}
        >
          <nav aria-labelledby="docs-tasks" className="gob-doc-tasks">
            <h2 id="docs-tasks">Start with what you want to do</h2>
            <ul>
              {tasks.map((task) => (
                <li key={task.href}>
                  <a href={task.href}>
                    <strong>{task.title}</strong>
                    <span>{task.meta}</span>
                  </a>
                </li>
              ))}
            </ul>
          </nav>
          <div className="gob-doc-marks" aria-label="Supported agents">
            {agents.map((agent) => (
              <ProviderMarkChip key={agent} mark={agent} size={26} />
            ))}
          </div>
          <div dangerouslySetInnerHTML={{ __html: body }} />
        </SiteDocument>
      </main>
      <SiteFooter path="/docs" />
    </>
  );
}

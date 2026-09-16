import type { Metadata } from "next";
import { AskAiAboutThis } from "@hraness/ui";

import { publishedRelease } from "../publication";
import { readmeHtml, readmeTitle } from "../readme.generated";

export const metadata: Metadata = {
  title: `${readmeTitle} documentation`,
  description: "The complete Gobstopper README: installation, strategies, configuration, hooks, telemetry, evaluation, and design notes.",
  alternates: { canonical: "/docs" },
  openGraph: {
    title: `${readmeTitle} documentation`,
    description: "The complete Gobstopper README.",
    type: "article",
    url: "/docs",
  },
};

export default function Docs() {
  return (
    <>
      <a className="skip-link" href="#main">Skip to content</a>
      <main id="main" tabIndex={-1} className="document-page">
        <nav aria-label="Site" className="document-nav">
          <a href="/">Gobstopper home</a>
          <a href="https://github.com/hraness/gobstopper">Source on GitHub</a>
          <a href="https://github.com/hraness/gobstopper/releases">Releases</a>
        </nav>
        {publishedRelease === null && <p>Release preview: the installation examples below target the forthcoming Gobstopper release. <a href="https://github.com/hraness/gobstopper/releases">Check published releases before installing</a>.</p>}
        <article dangerouslySetInnerHTML={{ __html: readmeHtml }} />
      </main>
      <AskAiAboutThis className="ask-ai" url="https://gobstopper.sh/docs" />
    </>
  );
}

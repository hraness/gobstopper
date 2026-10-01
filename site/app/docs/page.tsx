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

export default function Docs() {
  return (
    <>
      <SiteHeader path="/docs" />
      <main id="main" tabIndex={-1}>
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

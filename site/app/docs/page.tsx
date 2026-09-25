import type { Metadata } from "next";
import { ProviderMarkChip } from "@hraness/design-kit/react/server";

import { SiteHeader, SiteFooter } from "../_components/site-chrome";
import { readmeHtml } from "../readme.generated";

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
    images: [{ url: "/docs/opengraph-image", width: 1200, height: 630, alt: socialTitle }],
  },
  twitter: {
    card: "summary_large_image",
    title: socialTitle,
    description,
    images: [{ url: "/docs/opengraph-image", alt: socialTitle }],
  },
};

export default function Docs() {
  return (
    <>
      <SiteHeader path="/docs" />
      <main id="main" tabIndex={-1} className="document-page">
        <article>
          <div className="gob-doc-marks" aria-label="Supported agents">
            {agents.map((agent) => (
              <ProviderMarkChip key={agent} mark={agent} size={26} />
            ))}
          </div>
          <div dangerouslySetInnerHTML={{ __html: readmeHtml }} />
        </article>
      </main>
      <SiteFooter path="/docs" />
    </>
  );
}

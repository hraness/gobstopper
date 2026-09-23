import type { Metadata } from "next";

import { SiteHeader, SiteFooter } from "../_components/site-chrome";
import { publishedRelease } from "../publication";
import { readmeHtml } from "../readme.generated";

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
        {publishedRelease === null && <p>Release preview: the installation examples below target the forthcoming Gobstopper release. <a href="https://github.com/hraness/gobstopper/releases">Check published releases before installing</a>.</p>}
        <article dangerouslySetInnerHTML={{ __html: readmeHtml }} />
      </main>
      <SiteFooter path="/docs" />
    </>
  );
}

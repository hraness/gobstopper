import type { Metadata } from "next";

import { SiteHeader, SiteFooter } from "../_components/site-chrome";
import { publishedRelease } from "../publication";
import { readmeHtml, readmeTitle } from "../readme.generated";

const title = `${readmeTitle} documentation`;
const description =
  "The complete Gobstopper README: installation, strategies, configuration, hooks, telemetry, evaluation, and design notes.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/docs" },
  openGraph: {
    title,
    description,
    siteName: "Gobstopper",
    type: "article",
    url: "/docs",
    images: [{ url: "/docs/opengraph-image", width: 1200, height: 630, alt: title }],
  },
  twitter: {
    card: "summary_large_image",
    title,
    description,
    images: [{ url: "/docs/opengraph-image", alt: title }],
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

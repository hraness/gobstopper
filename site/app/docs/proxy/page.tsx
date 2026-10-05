import type { Metadata } from "next";
import { socialImageAlt } from "@hraness/web-discovery/social-image";

import { SiteDocument } from "../../_components/site-document";
import { SiteHeader, SiteFooter } from "../../_components/site-chrome";
import { GITHUB_URL } from "../../_lib/site";
import { socialSite } from "../../social";
import { proxyGuideHtml, proxyGuideSections } from "./guide.generated";
import { proxyGuideSocialPage } from "./social-page";

const path = "/docs/proxy";
const title = "Route Claude Code and Codex through the proxy";
const socialTitle = "Route Claude Code and Codex through the Gobstopper proxy";
const description =
  "Point Claude Code, Codex, or another coding agent at gobstopper proxy, choose its settings, and fix connection, routing, and service problems.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: path },
  openGraph: {
    title: socialTitle,
    description,
    siteName: "Gobstopper",
    type: "article",
    url: path,
    images: [{ url: `${path}/opengraph-image`, width: 1200, height: 630, alt: socialImageAlt(socialSite, proxyGuideSocialPage) }],
  },
  twitter: {
    card: "summary_large_image",
    title: socialTitle,
    description,
    images: [{ url: `${path}/opengraph-image`, alt: socialImageAlt(socialSite, proxyGuideSocialPage) }],
  },
};

// The guide supplies its own heading, so the source document's title is dropped.
const body = proxyGuideHtml.replace(/^<h1 id="[^"]*">[\s\S]*?<\/h1>\s*/u, "");

export default function ProxyGuide() {
  return (
    <>
      <SiteHeader path={path} />
      <main id="main" tabIndex={-1}>
        <SiteDocument
          dek={description}
          eyebrow="Guide"
          heading={title}
          meta={
            <>
              Source on GitHub:{" "}
              <a href={`${GITHUB_URL}/blob/main/docs/proxy.md`}>docs/proxy.md</a>
              {" "}and{" "}
              <a href={`${GITHUB_URL}/blob/main/docs/service.md`}>docs/service.md</a>
            </>
          }
          toc={proxyGuideSections}
        >
          <div dangerouslySetInnerHTML={{ __html: body }} />
        </SiteDocument>
      </main>
      <SiteFooter path={path} />
    </>
  );
}

import { NotFoundAnalytics } from "./not-found-analytics";
import type { Metadata } from "next";
import { RouteNotFoundPage } from "@hraness/design-kit/react";

import { SiteHeader, SiteFooter } from "./_components/site-chrome";
import { SITE_PAGES } from "./_lib/pages";
import { SITE_NAME } from "./_lib/site";
import { BLOG_PATH, BLOG_TITLE, indexablePosts, postPath } from "./blog/articles";

export const metadata: Metadata = {
  title: "Not found",
};

// The hint shows at most 48 characters; cut longer post titles at a word.
function routeLabel(title: string): string {
  if (title.length <= 48) return title;
  const cut = title.slice(0, 47);
  return `${cut.slice(0, cut.lastIndexOf(" ")).trimEnd()}…`;
}

// Known pages for "Did you mean": the sitemap's fixed pages, the blog, and every listed post.
const routes = [
  ...SITE_PAGES.map((page) => ({ href: page.path, label: page.label })),
  { href: BLOG_PATH, label: BLOG_TITLE },
  ...indexablePosts.map((post) => ({ href: postPath(post), label: routeLabel(post.title) })),
];

export default function NotFound() {
  return (
    <>
      <NotFoundAnalytics />
      <SiteHeader />
      <main id="main" tabIndex={-1}>
        <RouteNotFoundPage
          siteName={SITE_NAME}
          primaryAction={{ href: "/#install", label: "Install Gobstopper" }}
          next={[
            {
              href: "/docs",
              label: "Docs",
              description: "Install, pick a compaction strategy, and set it per provider or per session.",
            },
            {
              href: "/methodology",
              label: "How it measures compaction",
              description: "The occupancy model, what each strategy does, and what the numbers can show.",
            },
            {
              href: "/benchmarks",
              label: "Benchmarks",
              description: "Dated results from 729 archived sessions, proxy replays, and resume trials.",
            },
          ]}
          routes={routes}
          agentIndexHref="/llms.txt"
          canvasAs="div"
        />
      </main>
      <SiteFooter />
    </>
  );
}

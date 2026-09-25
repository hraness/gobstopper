import type { Metadata } from "next";
import {
  articleJsonLd,
  blogJsonLd,
  createArticleMetadata,
  createAtomFeed,
  createBlogSitemapPaths,
  createFeedEntry,
  NOINDEX_ROBOTS,
  type ArticleDiscovery,
  type ArticleParty,
  type SearchSite,
} from "@hraness/web-discovery";
import { isArticleIndexable, renderArticleProvenanceHtml } from "@hraness/design-kit";

import { SITE_DESCRIPTION, SITE_NAME, SITE_ORIGIN, SITE_TITLE } from "../_lib/site";
import {
  BLOG_DESCRIPTION,
  BLOG_FEED_PATH,
  BLOG_PATH,
  BLOG_TITLE,
  indexablePosts,
  postHtml,
  postPath,
  postProvenance,
  publishedTime,
  type BlogPost,
} from "./articles";

export const searchSite = {
  description: SITE_DESCRIPTION,
  name: SITE_NAME,
  origin: SITE_ORIGIN as `https://${string}`,
  title: SITE_TITLE,
} as const satisfies SearchSite;

export const hraness: ArticleParty = { kind: "Organization", name: "Hraness" };
const publisher: ArticleParty = { kind: "Organization", name: SITE_NAME, path: "/" };

export function postDiscovery(post: BlogPost): ArticleDiscovery {
  const path = postPath(post);
  return {
    authors: [hraness],
    blogPath: BLOG_PATH,
    canonicalPath: path,
    description: post.dek,
    image: {
      alt: post.title,
      contentType: "image/png",
      height: 630,
      path: `${path}/opengraph-image`,
      width: 1200,
    },
    keywords: post.keywords,
    publishedTime: publishedTime(post.published),
    publisher,
    section: post.eyebrow,
    title: post.title,
    type: "BlogPosting",
  };
}

/** Article metadata; posts that have not passed review are noindex. */
export function postMetadata(post: BlogPost): Metadata {
  const metadata = createArticleMetadata(searchSite, postDiscovery(post));
  return {
    ...metadata,
    alternates: { ...metadata.alternates, types: { "application/atom+xml": BLOG_FEED_PATH } },
    ...(isArticleIndexable(post.admission) ? {} : { robots: NOINDEX_ROBOTS }),
  };
}

export function postJsonLd(post: BlogPost) {
  return articleJsonLd(searchSite, postDiscovery(post));
}

export function blogIndexJsonLd() {
  return blogJsonLd(searchSite, {
    description: BLOG_DESCRIPTION,
    name: BLOG_TITLE,
    path: BLOG_PATH,
    publisher,
  }, indexablePosts.map(postDiscovery));
}

export function blogSitemapPaths() {
  return createBlogSitemapPaths({ path: BLOG_PATH }, indexablePosts.map(postDiscovery));
}

/** Feed readers have no page URL to resolve root-relative links against. */
function absoluteLinks(html: string): string {
  return html.replaceAll(' href="/', ` href="${SITE_ORIGIN}/`);
}

export function blogAtomFeed(): string {
  return createAtomFeed(searchSite, {
    authors: [hraness],
    description: BLOG_DESCRIPTION,
    homePath: BLOG_PATH,
    path: BLOG_FEED_PATH,
    title: BLOG_TITLE,
  }, indexablePosts.map((post) => createFeedEntry(postDiscovery(post), { contentHtml: feedContent(post) })));
}

/** Feed readers show the body without the page, so the entry repeats the provenance note first. */
function feedContent(post: BlogPost): string {
  return renderArticleProvenanceHtml(postProvenance(post)) + absoluteLinks(postHtml(post));
}

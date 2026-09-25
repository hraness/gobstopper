import type { Metadata } from "next";
import { ArticleIndex } from "@hraness/design-kit/react/server";

import { SiteFooter, SiteHeader } from "../_components/site-chrome";
import { serializeJsonLd } from "../_lib/site";
import { BLOG_DESCRIPTION, BLOG_FEED_PATH, BLOG_PATH, BLOG_TITLE, indexablePosts, postPath } from "./articles";
import { blogIndexJsonLd } from "./discovery";

export const metadata: Metadata = {
  title: "Blog",
  description: BLOG_DESCRIPTION,
  alternates: {
    canonical: BLOG_PATH,
    types: { "application/atom+xml": BLOG_FEED_PATH },
  },
  openGraph: {
    title: BLOG_TITLE,
    description: BLOG_DESCRIPTION,
    siteName: "Gobstopper",
    type: "website",
    url: BLOG_PATH,
    images: [{ url: "/opengraph-image", width: 1200, height: 630, alt: BLOG_TITLE }],
  },
  twitter: {
    card: "summary_large_image",
    title: BLOG_TITLE,
    description: BLOG_DESCRIPTION,
    images: [{ url: "/opengraph-image", alt: BLOG_TITLE }],
  },
};

export default function Blog() {
  return (
    <>
      <SiteHeader path={BLOG_PATH} />
      <main id="main" tabIndex={-1} className="document-page blog-page">
        <script
          dangerouslySetInnerHTML={{ __html: serializeJsonLd(blogIndexJsonLd()) }}
          type="application/ld+json"
        />
        <ArticleIndex
          heading="Blog"
          headingId="blog-title"
          headingLevel={1}
          items={indexablePosts.map((post) => ({
            href: postPath(post),
            title: post.title,
            dek: post.dek,
            published: post.published,
            eyebrow: post.eyebrow,
          }))}
          summary={BLOG_DESCRIPTION}
        />
        <p className="blog-feed-link">
          <a href={BLOG_FEED_PATH}>Subscribe with the Atom feed</a>
        </p>
      </main>
      <SiteFooter path={BLOG_PATH} />
    </>
  );
}

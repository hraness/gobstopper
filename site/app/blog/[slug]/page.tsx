import type { Metadata } from "next";
import { notFound } from "next/navigation";
import {
  ArticleRelatedProducts,
  ArticleSources,
  MarketingArticle,
} from "@hraness/design-kit/react/server";
import { relatedFor } from "@hraness/design-kit/portfolio";

import { SiteFooter, SiteHeader } from "../../_components/site-chrome";
import { serializeJsonLd } from "../../_lib/site";
import { blogPosts, findPost, postHtml, postPath, postProvenance, postToc } from "../articles";
import { postJsonLd, postMetadata } from "../discovery";

type Params = Promise<{ slug: string }>;

export const dynamicParams = false;

export function generateStaticParams() {
  return blogPosts.map((post) => ({ slug: post.slug }));
}

export async function generateMetadata({ params }: Readonly<{ params: Params }>): Promise<Metadata> {
  const post = findPost((await params).slug);
  if (post === undefined) notFound();
  return postMetadata(post);
}

export default async function BlogPost({ params }: Readonly<{ params: Params }>) {
  const post = findPost((await params).slug);
  if (post === undefined) notFound();
  const path = postPath(post);
  return (
    <>
      <SiteHeader path={path} />
      <main id="main" tabIndex={-1} className="document-page blog-page">
        <script
          dangerouslySetInnerHTML={{ __html: serializeJsonLd(postJsonLd(post)) }}
          type="application/ld+json"
        />
        <MarketingArticle
          after={(
            <>
              <ArticleSources
                sources={post.admission.sources.map((source) => ({
                  title: source.title,
                  href: source.url,
                  checkedOn: source.checkedOn,
                }))}
              />
              <ArticleRelatedProducts items={relatedFor("gobstopper")} />
            </>
          )}
          author={{ kind: "organization", name: "Hraness", href: "https://hraness.com" }}
          dek={post.dek}
          eyebrow={post.eyebrow}
          heading={post.title}
          provenance={postProvenance(post)}
          published={post.published}
          toc={postToc(post)}
        >
          <div dangerouslySetInnerHTML={{ __html: postHtml(post) }} />
        </MarketingArticle>
      </main>
      <SiteFooter path={path} />
    </>
  );
}

import { describe, expect, test } from "bun:test";
import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { renderToStaticMarkup } from "react-dom/server";
import {
  ArticleAdmissionError,
  articleProvenanceSentence,
  assertArticleAdmissions,
  type ArticleAdmission,
} from "@hraness/design-kit";
import { relatedFor } from "@hraness/design-kit/portfolio";
import { NOINDEX_ROBOTS } from "@hraness/web-discovery";

import {
  articleAdmissions,
  blogPosts,
  indexablePosts,
  postHtml,
  postPath,
  postProvenance,
  type BlogPost as BlogPostRecord,
} from "../app/blog/articles";
import { blogAtomFeed, blogIndexJsonLd, postJsonLd, postMetadata } from "../app/blog/discovery";
import BlogIndex from "../app/blog/page";
import BlogPost from "../app/blog/[slug]/page";
import sitemap from "../app/sitemap";
import { publishedRelease } from "../app/publication";
import { generatedBlogModule, generatedBlogPath } from "../scripts/sync-blog";

const site = join(import.meta.dir, "..");
const PAGE_ROUTES = new Set(["/", "/docs", "/methodology", "/benchmarks", "/blog"]);

describe("Gobstopper blog", () => {
  test("every post has an admission record that passes the shared rubric", () => {
    expect(() => assertArticleAdmissions(articleAdmissions)).not.toThrow();
    for (const post of blogPosts) expect(post.admission.href).toBe(postPath(post));
    expect(new Set(blogPosts.map((post) => post.slug)).size).toBe(blogPosts.length);
  });

  test("the validator refuses an indexable post without its review", () => {
    const [first] = articleAdmissions;
    const unreviewed: ArticleAdmission = { ...first!, review: null };
    expect(() => assertArticleAdmissions([unreviewed])).toThrow(ArticleAdmissionError);
  });

  test("every Markdown post has a record and the generated bodies are current", async () => {
    const files = (await readdir(join(site, "content/blog"))).filter((name) => name.endsWith(".md")).sort();
    expect(files).toEqual(blogPosts.map((post) => `${post.slug}.md`).sort());
    expect(await readFile(generatedBlogPath, "utf8")).toBe(await generatedBlogModule());
  });

  test("post bodies fill release data and link only to live routes", () => {
    const postPaths = new Set(blogPosts.map(postPath));
    for (const post of blogPosts) {
      const html = postHtml(post);
      expect(html).not.toContain("{{");
      if (publishedRelease !== null) expect(html).toContain(`Latest release: v${publishedRelease.version}.`);
      for (const [, href] of html.matchAll(/href="([^"]+)"/gu)) {
        if (href!.startsWith("#")) continue;
        if (href!.startsWith("/")) {
          const path = href!.split("#")[0]!;
          expect(PAGE_ROUTES.has(path) || postPaths.has(path as `/blog/${string}`)).toBe(true);
        } else {
          expect(href).toMatch(/^https:\/\//u);
        }
      }
    }
  });

  test("every post shows the Hraness byline and its recorded AI review", () => {
    for (const post of blogPosts) {
      const html = renderToStaticMarkup(<BlogPostBody post={post} />);
      expect(html).toContain('data-author-kind="organization"');
      expect(html).toMatch(/By <a href="https:\/\/hraness\.com" rel="author">Hraness<\/a>/u);
      const sentence = articleProvenanceSentence(postProvenance(post));
      expect(sentence).toBe("Drafted with AI from the source code and reviewed by Claude Opus 5.5 (claude-opus-5-5) editorial review.");
      expect(html).toContain(sentence);
      expect(html).toContain('data-reviewer-type="ai"');
      expect(html).not.toMatch(/human/iu);
      expect(post.admission.humanReview).toBeNull();
      for (const item of relatedFor("gobstopper")) expect(html).toContain(item.href);
      expect(html).toContain('"@type":"BlogPosting"');
    }
  });

  test("quarantined posts are noindex; indexable posts are indexable", () => {
    for (const post of blogPosts) {
      const quarantined: BlogPostRecord = { ...post, admission: { ...post.admission, lifecycle: "quarantined" } };
      expect(postMetadata(quarantined).robots).toEqual(NOINDEX_ROBOTS);
      const robots = postMetadata(post).robots as { index?: boolean };
      expect(robots.index).toBe(post.admission.lifecycle === "indexable");
    }
  });

  test("canonical, Open Graph, and BlogPosting data name the post URL", () => {
    for (const post of blogPosts) {
      const url = `https://gobstopper.sh${postPath(post)}`;
      const metadata = postMetadata(post);
      expect(metadata.alternates?.canonical).toBe(url);
      expect((metadata.openGraph as { type?: string }).type).toBe("article");
      const schema = postJsonLd(post);
      expect(schema["@type"]).toBe("BlogPosting");
      expect(schema["@id"]).toBe(`${url}#article`);
      expect(schema.isPartOf).toMatchObject({ "@type": "Blog", url: "https://gobstopper.sh/blog" });
    }
  });

  test("sitemap, feed, index, and llms.txt list exactly the indexable posts", async () => {
    const indexable = indexablePosts.map((post) => `https://gobstopper.sh${postPath(post)}`).sort();
    const hidden = blogPosts.filter((post) => !indexablePosts.includes(post)).map((post) => `https://gobstopper.sh${postPath(post)}`);
    const entries = sitemap().filter((entry) => entry.url.startsWith("https://gobstopper.sh/blog/"));
    expect(entries.map((entry) => entry.url).sort()).toEqual(indexable);
    for (const entry of entries) expect(entry.lastModified).toBeDefined();

    const feed = blogAtomFeed();
    expect(Array.from(feed.matchAll(/<entry>\n<id>([^<]+)<\/id>/gu), (match) => match[1]).sort()).toEqual(indexable);
    expect(feed).not.toContain('href="/');

    expect(blogIndexJsonLd().blogPost.map((post) => post.url).sort()).toEqual(indexable);
    const index = renderToStaticMarkup(<BlogIndex />);
    for (const post of indexablePosts) expect(index).toContain(`href="${postPath(post)}"`);

    const llms = await readFile(join(site, "public/llms.txt"), "utf8");
    const listed = Array.from(llms.matchAll(/\((https:\/\/gobstopper\.sh\/blog\/[^)]+)\)/gu), (match) => match[1]).sort();
    expect(listed).toEqual(indexable);
    for (const url of hidden) {
      expect(llms).not.toContain(url);
      expect(feed).not.toContain(url);
      expect(index).not.toContain(url.replace("https://gobstopper.sh", ""));
    }
  });
});

function BlogPostBody({ post }: Readonly<{ post: BlogPostRecord }>) {
  return <AsyncPost slug={post.slug} />;
}

// Server components are async; resolve the element before rendering to static markup.
const resolved = new Map<string, React.ReactNode>();
for (const post of blogPosts) {
  resolved.set(post.slug, await BlogPost({ params: Promise.resolve({ slug: post.slug }) }));
}

function AsyncPost({ slug }: Readonly<{ slug: string }>) {
  return <>{resolved.get(slug)}</>;
}

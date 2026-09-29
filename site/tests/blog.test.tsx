import { describe, expect, test } from "bun:test";
import { existsSync } from "node:fs";
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
  filmHtml,
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
import { renderPostHtml } from "../scripts/blog-html";
import { FILM_TOKEN } from "../app/blog/film-token";
import { gobFilm, type GobFilm } from "../app/_data/gob-film";
import { GOB_FILM_SCOPE, gobFilmHtml } from "../app/_lib/gobbench-film-html";
import { postBodies as postBodiesForTest } from "../app/blog/posts.generated";

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

  test("{{film}} is the one block token, and the introduction places it once after the lead figure", () => {
    const intro = blogPosts.find((post) => post.slug === "introducing-gobstopper")!;
    for (const post of blogPosts) {
      const body = generatedBody(post.slug);
      expect(body.split(FILM_TOKEN).length - 1).toBe(post === intro ? 1 : 0);
      expect(body.replace(FILM_TOKEN, "")).not.toContain("{{film");
    }
    const body = generatedBody(intro.slug);
    expect(body.indexOf("gob-fuse.png")).toBeLessThan(body.indexOf(FILM_TOKEN));
    expect(body.indexOf(FILM_TOKEN)).toBeLessThan(body.indexOf("<h2"));
    expect(() => renderPostHtml("Text {{film}} inline.")).toThrow();
    expect(() => renderPostHtml("{{film}}\n\n{{film}}")).toThrow();

    const html = postHtml(intro);
    if (gobFilm === null) expect(html).not.toContain("<video");
    else {
      expect(html).toContain(filmHtml(gobFilm));
      expect(html.split("<video").length - 1).toBe(1);
    }
  });

  test("the post film is an accessible player that never plays on its own", () => {
    expect(filmHtml(null)).toBe("");
    // Hostile values the manifest type would reject, to prove escaping holds anyway.
    const film = {
      src: "/media/gobstopper-film-1080p.mp4?v=1&t=0",
      poster: "/media/gobstopper-film-poster.jpg?v=1&w=1920",
      captions: "/media/gobstopper-film.en.vtt",
      width: 1920,
      height: 1080,
      durationSeconds: 75,
      bytes: 1,
      sha256: "0".repeat(64),
      beats: [
        { start: 0, end: 5, text: "A <b>session</b> & its \"tokens\"" },
        { start: 5, end: 9, text: "It cost $2.25, not $$ or $& or $'." },
      ],
    } as unknown as GobFilm;
    const html = filmHtml(film);
    expect(html).toBe(gobFilmHtml(film));
    expect(html.match(/<video\b[^>]*>/gu)?.length).toBe(1);
    const video = /<video\b[^>]*>/u.exec(html)![0];
    for (const attribute of ['controls=""', 'preload="none"', 'playsInline=""', 'width="1920"', 'height="1080"']) expect(video).toContain(attribute);
    expect(video).toContain('poster="/media/gobstopper-film-poster.jpg?v=1&amp;w=1920"');
    expect(html).toContain('<source src="/media/gobstopper-film-1080p.mp4?v=1&amp;t=0" type="video/mp4"/>');
    expect(html.match(/<track\b[^>]*>/gu)?.length).toBe(1);
    const track = /<track\b[^>]*>/u.exec(html)![0];
    for (const attribute of ['default=""', 'kind="captions"', 'srcLang="en"', `src="${film.captions}"`]) expect(track).toContain(attribute);
    const describedBy = /aria-describedby="([^"]+)"/u.exec(html)?.[1];
    expect(describedBy).toBeDefined();
    expect(html).toContain(`<details id="${describedBy}">`);
    expect(html.split(`id="${describedBy}"`).length - 1).toBe(1);
    expect(html).toContain("The film is also available as text below.");
    expect(html).toContain(GOB_FILM_SCOPE);
    expect(html).toContain("within single-trial noise");
    expect(html).toContain("45K threshold (default 128K)");
    expect(html).not.toMatch(/\b(?:autoplay|muted|loop)\b/u);
    expect(html.match(/<li>/gu)?.length).toBe(film.beats.length);
    expect(html).toContain("A &lt;b&gt;session&lt;/b&gt; &amp; its &quot;tokens&quot;");
    expect(html).toContain("It cost $2.25, not $$ or $&amp; or $&#x27;.");

    const intro = blogPosts.find((post) => post.slug === "introducing-gobstopper")!;
    const withFilm = generatedBody(intro.slug).replace(FILM_TOKEN, () => filmHtml(film));
    expect(withFilm).toContain("It cost $2.25, not $$ or $&amp; or $&#x27;.");
    expect(withFilm.split('id="film-player"').length - 1).toBe(1);
  });

  test("every post image is a captioned figure whose file ships in the site", () => {
    const intro = blogPosts.find((post) => post.slug === "introducing-gobstopper")!;
    for (const post of blogPosts) {
      const body = generatedBody(post.slug);
      const images = [...body.matchAll(/<img\b[^>]*\bsrc="([^"]+)"/gu)].map((match) => match[1]!);
      const figures = body.match(/<figure class="gob-post-figure"><img\b[\s\S]*?<figcaption>[\s\S]*?<\/figcaption><\/figure>/gu) ?? [];
      expect(figures.length).toBe(images.length);
      if (post === intro) expect(figures.length).toBe(10);
      for (const src of images) {
        expect(src).toMatch(/^\/blog\//u);
        expect(existsSync(join(site, "public", src))).toBe(true);
      }
    }
    expect(() => renderPostHtml("![A chart](/blog/x.png)\n\nNo caption.")).toThrow();
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
      // Hraness publishes the blog; Gobstopper is the product, not an organization.
      expect(schema.publisher).toMatchObject({ "@type": "Organization", name: "Hraness", url: "https://hraness.com" });
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
    for (const post of indexablePosts) {
      const entry = feed.slice(feed.indexOf(`<id>https://gobstopper.sh${postPath(post)}</id>`));
      expect(entry.slice(0, entry.indexOf("</entry>"))).toContain(articleProvenanceSentence(postProvenance(post)));
    }

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

function generatedBody(slug: BlogPostRecord["slug"]): string {
  return postBodiesForTest[slug].html;
}

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

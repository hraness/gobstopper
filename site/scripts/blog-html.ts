import { FILM_TOKEN } from "../app/blog/film-token.ts";
import { addHeadingIds, assertFragmentsResolve, assertSafeTarget, headingText } from "./readme-html.ts";

export type RenderedPost = Readonly<{
  html: string;
  toc: readonly Readonly<{ href: `#${string}`; label: string }>[];
}>;

export { FILM_TOKEN };

/**
 * Render one post body from Markdown. Raw HTML is disabled, every link must be
 * a web, mail, fragment, or root-relative target, and every fragment must name
 * a heading in the post. A `{{film}}` line stays as a bare token for
 * `postHtml` to fill. An image paragraph followed by an italic `Figure N.`
 * paragraph becomes one figure with a caption, and an optional plain line
 * after the italic caption becomes its data-source line.
 */
export function renderPostHtml(source: string): RenderedPost {
  const html = Bun.markdown.html(source, {
    noHtmlBlocks: true,
    noHtmlSpans: true,
    tagFilter: true,
  });
  for (const match of html.matchAll(/\s(?:href|src)="([^"]*)"/gu)) {
    const target = match[1];
    if (target === undefined) continue;
    assertSafeTarget(target);
    if (!/^(?:https:\/\/|mailto:|#|\/(?!\/))/u.test(target)) {
      throw new Error(`Post links must be https, mail, fragment, or root-relative: ${JSON.stringify(target)}`);
    }
  }
  if (/<h1\b/u.test(html)) throw new Error("Post bodies start below the article title; use ## headings.");
  const blockToken = `<p>${FILM_TOKEN}</p>`;
  const tokens = html.split(FILM_TOKEN).length - 1;
  if (tokens !== html.split(blockToken).length - 1) {
    throw new Error(`${FILM_TOKEN} must stand alone on its own line.`);
  }
  if (tokens > 1) throw new Error(`A post may place ${FILM_TOKEN} once.`);
  const rendered = addHeadingIds(captionFigures(html.replace(blockToken, FILM_TOKEN)));
  assertFragmentsResolve(rendered);
  const toc = Array.from(rendered.matchAll(/<h2 id="([^"]+)">([\s\S]*?)<\/h2>/gu), ([, id, body]) => ({
    href: `#${id}` as const,
    label: headingText(body ?? "").trim(),
  }));
  return { html: rendered, toc };
}

const FIGURE = /<p>(<img [^>]*\/?>)<\/p>\n<p><em>(Figure \d+\.[\s\S]*?)<\/em>(?:\n([\s\S]*?))?<\/p>/gu;

/**
 * Pair each post image with its caption as a `<figure>`. Every image must have
 * one, so a still cannot ship without its caption and data source.
 */
function captionFigures(html: string): string {
  let index = 0;
  const figured = html.replace(FIGURE, (_match, image: string, caption: string, provenance: string | undefined) => {
    index += 1;
    const img = image.replace(/\s*\/?>$/u, index === 1 ? ">" : ` loading="lazy" decoding="async">`);
    const source = provenance?.trim() ? `<span class="gob-post-figure__prov">${provenance.trim()}</span>` : "";
    return `<figure class="gob-post-figure">${img}<figcaption><span class="gob-post-figure__caption">${caption.trim()}</span>${source}</figcaption></figure>`;
  });
  const bare = figured.match(/<p>\s*<img\b|<p><img\b/u);
  if (bare !== null) throw new Error("Every post image needs an italic \"Figure N.\" caption paragraph right after it.");
  return figured;
}

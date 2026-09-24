import { addHeadingIds, assertFragmentsResolve, assertSafeTarget } from "./readme-html.ts";

export type RenderedPost = Readonly<{
  html: string;
  toc: readonly Readonly<{ href: `#${string}`; label: string }>[];
}>;

function decodeText(value: string): string {
  return value
    .replace(/<[^>]*>/gu, "")
    .replaceAll("&quot;", '"')
    .replaceAll("&#39;", "'")
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&amp;", "&");
}

/**
 * Render one post body from Markdown. Raw HTML is disabled, every link must be
 * a web, mail, fragment, or root-relative target, and every fragment must name
 * a heading in the post.
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
  const rendered = addHeadingIds(html);
  assertFragmentsResolve(rendered);
  const toc = Array.from(rendered.matchAll(/<h2 id="([^"]+)">([\s\S]*?)<\/h2>/gu), ([, id, body]) => ({
    href: `#${id}` as const,
    label: decodeText(body ?? ""),
  }));
  return { html: rendered, toc };
}

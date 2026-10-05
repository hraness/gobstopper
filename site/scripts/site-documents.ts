import { headingIds, renderReadmeHtml, type SiteLinkResolver } from "./readme-html.ts";

/** The site page that publishes the proxy guide. */
export const PROXY_GUIDE_ROUTE = "/docs/proxy";
/** The repository document the guide renders in full. */
export const PROXY_GUIDE_SOURCE = "docs/proxy.md";
/** The repository document the guide takes its service sections from. */
export const SERVICE_SOURCE = "docs/service.md";
/** The docs/service.md sections, with their subsections, that follow docs/proxy.md on the guide. */
export const PROXY_GUIDE_SERVICE_SECTIONS = ["Diagnose, repair, and remove", "Launch with a direct fallback"] as const;

/** One level-2 section of a Markdown document, through the end of its subsections. */
export function markdownSection(source: string, heading: string): string {
  const lines = source.split("\n");
  let fence: Readonly<{ marker: string; length: number }> | undefined;
  let start: number | undefined;
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index] ?? "";
    const opening = /^(`{3,}|~{3,})/u.exec(line)?.[1];
    if (opening !== undefined) {
      if (fence === undefined) fence = { marker: opening[0] ?? "`", length: opening.length };
      else if (opening[0] === fence.marker && opening.length >= fence.length) fence = undefined;
      continue;
    }
    if (fence !== undefined || !line.startsWith("## ")) continue;
    if (start !== undefined) return lines.slice(start, index).join("\n").trimEnd();
    if (line === `## ${heading}`) start = index;
  }
  if (start === undefined) throw new Error(`${SERVICE_SOURCE} has no section "## ${heading}"`);
  return lines.slice(start).join("\n").trimEnd();
}

/** The republished service sections, with same-file links kept pointing at docs/service.md. */
function serviceSections(service: string): readonly string[] {
  return PROXY_GUIDE_SERVICE_SECTIONS.map((heading) =>
    markdownSection(service, heading).replace(/\]\(#([^)\s]+)\)/gu, "](service.md#$1)"));
}

/** docs/proxy.md in full, followed by the republished docs/service.md sections. */
export function proxyGuideMarkdown(proxy: string, service: string): string {
  return `${[proxy.trimEnd(), ...serviceSections(service)].join("\n\n")}\n`;
}

/** Heading fragments of the docs/service.md sections the guide republishes. */
export function serviceGuideFragments(service: string): ReadonlySet<string> {
  return headingIds(renderReadmeHtml(serviceSections(service).join("\n\n"), { baseDirectory: "docs" }));
}

/**
 * Sends links to the README, docs/proxy.md, and the republished docs/service.md
 * sections to their site pages. Every other repository link stays on GitHub.
 */
export function createSiteLinkResolver(service: string): SiteLinkResolver {
  const published = serviceGuideFragments(service);
  return (path, fragment) => {
    const suffix = fragment === undefined ? "" : `#${fragment}`;
    if (path === "README.md") return `/docs${suffix}`;
    if (path === PROXY_GUIDE_SOURCE) return `${PROXY_GUIDE_ROUTE}${suffix}`;
    if (path === SERVICE_SOURCE && fragment !== undefined && published.has(fragment)) {
      return `${PROXY_GUIDE_ROUTE}#${fragment}`;
    }
    return undefined;
  };
}

/** The rendered guide; fails when a republished service heading changes its fragment. */
export function renderProxyGuideHtml(proxy: string, service: string): string {
  const html = renderReadmeHtml(proxyGuideMarkdown(proxy, service), {
    baseDirectory: "docs",
    currentRoute: PROXY_GUIDE_ROUTE,
    resolveSiteLink: createSiteLinkResolver(service),
  });
  const ids = headingIds(html);
  for (const fragment of serviceGuideFragments(service)) {
    if (!ids.has(fragment)) throw new Error(`Proxy guide lost the ${SERVICE_SOURCE} fragment #${fragment}`);
  }
  return html;
}

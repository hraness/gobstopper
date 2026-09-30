import { productMessaging, productName, productCanonicalUrl } from "../messaging";

export const SITE_ORIGIN = productCanonicalUrl;
export const SITE_NAME = productName;
export const SITE_TAGLINE = productMessaging.tagline;
export const SITE_TITLE = productMessaging.headings["home-search-title"];
export const SITE_DESCRIPTION = productMessaging.meta;
export const GITHUB_URL = "https://github.com/hraness/gobstopper";
/** The Hraness organization node, identified by the @id hraness.com publishes. */
export const HRANESS_ORGANIZATION = {
  "@type": "Organization",
  "@id": "https://hraness.com/#organization",
  name: "Hraness",
  url: "https://hraness.com",
} as const;
export const ARCHITECTURE_URL = "https://github.com/hraness/gobstopper/blob/main/docs/design.md";

export type CanonicalPagePath = "/" | "/docs" | "/methodology" | "/benchmarks" | "/compare/cliffcompaction" | "/compare/claude-code-compact" | "/blog" | `/blog/${string}`;

export function absoluteUrl(path: CanonicalPagePath | string): string {
  if (path.startsWith("http")) return path;
  const base = SITE_ORIGIN.endsWith("/") ? SITE_ORIGIN.slice(0, -1) : SITE_ORIGIN;
  const normalized = path.startsWith("/") ? path : `/${path}`;
  return `${base}${normalized}`;
}

export function serializeJsonLd(value: unknown): string {
  return JSON.stringify(value);
}

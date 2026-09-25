export const SITE_ORIGIN = "https://gobstopper.sh";
export const SITE_NAME = "Gobstopper";
export const SITE_TAGLINE = "Context compaction you can undo.";
export const SITE_TITLE = `${SITE_NAME}: context compaction you can undo`;
export const SITE_DESCRIPTION =
  "Gobstopper makes long Claude Code and Codex sessions smaller. Preview each cut, write a compacted copy, and keep every original byte in a local vault.";
export const GITHUB_URL = "https://github.com/hraness/gobstopper";
export const ARCHITECTURE_URL = "https://github.com/hraness/gobstopper/blob/main/docs/design.md";

export type CanonicalPagePath = "/" | "/docs" | "/methodology" | "/benchmarks" | "/compare/cliffcompaction" | "/blog" | `/blog/${string}`;

export function absoluteUrl(path: CanonicalPagePath | string): string {
  if (path.startsWith("http")) return path;
  const base = SITE_ORIGIN.endsWith("/") ? SITE_ORIGIN.slice(0, -1) : SITE_ORIGIN;
  const normalized = path.startsWith("/") ? path : `/${path}`;
  return `${base}${normalized}`;
}

export function serializeJsonLd(value: unknown): string {
  return JSON.stringify(value);
}

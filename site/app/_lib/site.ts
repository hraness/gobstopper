export const SITE_ORIGIN = "https://gobstopper.sh";
export const SITE_NAME = "Gobstopper";
export const SITE_DESCRIPTION =
  "Gobstopper compacts Claude Code, Codex, and Devin sessions at a context size you choose and snapshots every transcript first, so any compaction can be undone.";
export const GITHUB_URL = "https://github.com/hraness/gobstopper";
export const ARCHITECTURE_URL = "https://github.com/hraness/gobstopper/blob/main/docs/design.md";

export type CanonicalPagePath = "/" | "/docs" | "/methodology" | "/benchmarks";

export function absoluteUrl(path: CanonicalPagePath | string): string {
  if (path.startsWith("http")) return path;
  const base = SITE_ORIGIN.endsWith("/") ? SITE_ORIGIN.slice(0, -1) : SITE_ORIGIN;
  const normalized = path.startsWith("/") ? path : `/${path}`;
  return `${base}${normalized}`;
}

export function serializeJsonLd(value: unknown): string {
  return JSON.stringify(value);
}

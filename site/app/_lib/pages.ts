import type { MetadataRoute } from "next";

import type { CanonicalPagePath } from "./site";

type SitePage = Readonly<{
  path: CanonicalPagePath;
  /** Short name the 404 page offers as "Did you mean …?". */
  label: string;
  changeFrequency: NonNullable<MetadataRoute.Sitemap[number]["changeFrequency"]>;
  priority: number;
  /** The date of the page's last material content change, as YYYY-MM-DD. Bump it when the content changes. */
  lastModified: string;
}>;

/** The site's fixed pages. The sitemap and the 404 page's known routes both read this list. */
export const SITE_PAGES: readonly SitePage[] = [
  { path: "/", label: "Gobstopper", changeFrequency: "weekly", priority: 1, lastModified: "2026-09-28" },
  { path: "/docs", label: "Documentation", changeFrequency: "monthly", priority: 0.9, lastModified: "2026-09-28" },
  { path: "/methodology", label: "Methodology", changeFrequency: "monthly", priority: 0.7, lastModified: "2026-09-28" },
  { path: "/benchmarks", label: "Benchmarks", changeFrequency: "weekly", priority: 0.7, lastModified: "2026-09-28" },
  {
    path: "/compare/cliffcompaction",
    label: "Compared with CliffCompaction",
    changeFrequency: "monthly",
    priority: 0.6,
    lastModified: "2026-09-28",
  },
  {
    path: "/compare/claude-code-compact",
    label: "Compared with Claude Code /compact",
    changeFrequency: "monthly",
    priority: 0.6,
    lastModified: "2026-09-26",
  },
];

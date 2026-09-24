import type { MetadataRoute } from "next";
import { createSitemap } from "@hraness/web-discovery";

import { absoluteUrl, SITE_ORIGIN } from "./_lib/site";
import { blogSitemapPaths } from "./blog/discovery";

export default function sitemap(): MetadataRoute.Sitemap {
  const now = new Date();
  return [
    {
      url: absoluteUrl("/"),
      lastModified: now,
      changeFrequency: "weekly",
      priority: 1,
    },
    {
      url: absoluteUrl("/docs"),
      lastModified: now,
      changeFrequency: "monthly",
      priority: 0.9,
    },
    {
      url: absoluteUrl("/methodology"),
      lastModified: now,
      changeFrequency: "monthly",
      priority: 0.7,
    },
    {
      url: absoluteUrl("/benchmarks"),
      lastModified: now,
      changeFrequency: "weekly",
      priority: 0.7,
    },
    // Only posts whose review record admits them for indexing.
    ...createSitemap(SITE_ORIGIN as `https://${string}`, blogSitemapPaths()),
  ];
}

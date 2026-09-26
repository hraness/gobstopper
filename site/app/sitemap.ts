import type { MetadataRoute } from "next";
import { createSitemap } from "@hraness/web-discovery";

import { SITE_PAGES } from "./_lib/pages";
import { absoluteUrl, SITE_ORIGIN } from "./_lib/site";
import { blogSitemapPaths } from "./blog/discovery";

export default function sitemap(): MetadataRoute.Sitemap {
  const now = new Date();
  return [
    ...SITE_PAGES.map((page) => ({
      url: absoluteUrl(page.path),
      lastModified: page.lastModified === undefined ? now : new Date(page.lastModified),
      changeFrequency: page.changeFrequency,
      priority: page.priority,
    })),
    // Only posts whose review record admits them for indexing.
    ...createSitemap(SITE_ORIGIN as `https://${string}`, blogSitemapPaths()),
  ];
}

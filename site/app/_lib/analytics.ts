import {
  POSTHOG_SCHEMA_VERSION,
  type PostHogSiteDefinition,
} from "@hraness/posthog";

/**
 * Gobstopper's analytics vocabulary on the shared Hraness "small-sites"
 * PostHog project. Capture runs only on gobstopper.sh in production with a
 * public phc_ token; it is cookieless and memory-only, strips queries, and
 * accepts only the events listed here.
 */
export const INSTALL_COPIED_EVENT = "install command copied";
export const CTA_CLICKED_EVENT = "cta clicked";
export const FILM_PLAYED_EVENT = "launch film played";

export const gobstopperPostHogSite = {
  id: "gobstopper",
  canonicalDomain: "gobstopper.sh",
  allowedHosts: ["gobstopper.sh"],
  schemaVersion: POSTHOG_SCHEMA_VERSION,
  excludedPaths: ["/api", "/auth", "/account", "/dashboard", "/login", "/sign-in", "/oauth", "/callback", "/checkout", "/billing", "/invite"]
    .map(path => ({ match: "prefix" as const, path })),
  routes: [
    { match: "exact", path: "/", pageKind: "home" },
    { match: "exact", path: "/docs", pageKind: "docs" },
    { match: "exact", path: "/docs/proxy", pageKind: "docs" },
    { match: "exact", path: "/benchmarks", pageKind: "benchmarks" },
    { match: "exact", path: "/methodology", pageKind: "methodology" },
    { match: "exact", path: "/blog", pageKind: "blog_index", contentGroup: "blog" },
    { match: "prefix", path: "/blog", pageKind: "blog_post", contentGroup: "blog", captureSlug: true },
    { match: "prefix", path: "/compare", pageKind: "comparison", contentGroup: "compare", captureSlug: true },
  ],
  customEvents: ["outbound link opened", "page not found", INSTALL_COPIED_EVENT, CTA_CLICKED_EVENT, FILM_PLAYED_EVENT],
  unknownCanonicalPath: "/not-found",
} as const satisfies PostHogSiteDefinition;

/** Which install block a copy came from, by its order in the install panel. */
export const INSTALL_BLOCKS = ["cargo_install", "proxy", "sessions"] as const;

/** A bounded id for a hero action, from its link target. */
export function ctaIdFor(href: string): string | null {
  if (href === "#install") return "install";
  if (href === "/benchmarks") return "benchmarks";
  return null;
}

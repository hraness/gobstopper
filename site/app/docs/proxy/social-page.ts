import type { SocialImagePage } from "@hraness/web-discovery/social-image";

/** Copy for the /docs/proxy share card; the site declaration in app/social.ts supplies the rest. */
export const proxyGuideSocialPage = {
  description: "Set up each agent, choose settings, and fix the local service.",
  eyebrow: "Proxy guide",
  headline: "Proxy setup and troubleshooting",
} as const satisfies SocialImagePage;

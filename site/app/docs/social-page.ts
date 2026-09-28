import type { SocialImagePage } from "@hraness/web-discovery/social-image";

/** Copy for the /docs share card; the site declaration in app/social.ts supplies the rest. */
export const docsSocialPage = {
  description: "Install Gobstopper, pick a compaction strategy, and configure it per provider or session.",
  eyebrow: "Docs",
  headline: "Gobstopper documentation",
} as const satisfies SocialImagePage;

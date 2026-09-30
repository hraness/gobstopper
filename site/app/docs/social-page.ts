import type { SocialImagePage } from "@hraness/web-discovery/social-image";

/** Copy for the /docs share card; the site declaration in app/social.ts supplies the rest. */
export const docsSocialPage = {
  description: "Install, pick a strategy, and configure it.",
  eyebrow: "Documentation",
  headline: "Gobstopper documentation",
} as const satisfies SocialImagePage;

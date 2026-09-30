import { defineSocialImageSite, type SocialImagePage } from "@hraness/web-discovery/social-image";

import { productMessaging, productName } from "./messaging";
import { socialBrandMarkSvg } from "./social-mark";

/**
 * Gobstopper's one social-image declaration. Every Open Graph and Twitter card
 * on the site renders from this through the shared @hraness/web-discovery
 * template, which draws the site's sticky header (foil mark and name) over its
 * hero in the Tokyo Night palette the site sets on <html data-palette>.
 * Routes pass page copy only.
 */
export const socialSite = defineSocialImageSite({
  brand: "Gobstopper",
  brandMark: socialBrandMarkSvg,
  description: productMessaging.tagline,
  domain: "gobstopper.sh",
  name: productName,
  palette: "tokyo-night",
});

/**
 * The home card mirrors the hero: the category eyebrow over the tagline, which
 * is also the hero heading. It fits two lines, so the card keeps the default
 * product layout.
 */
export const homeSocialPage = {
  eyebrow: productMessaging.category,
} as const satisfies SocialImagePage;

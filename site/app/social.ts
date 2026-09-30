import { defineSocialImageSite } from "@hraness/web-discovery/social-image";

import { socialIconPng } from "./social-icon";

/**
 * Gobstopper's one social-image declaration. Every Open Graph and Twitter card
 * on the site renders from this through the shared @hraness/web-discovery
 * template; routes pass page copy only.
 */
export const socialSite = defineSocialImageSite({
  description: "Compacts long agent sessions into smaller copies, keeping every byte.",
  domain: "gobstopper.sh",
  icon: { kind: "app", src: socialIconPng },
  name: "Gobstopper",
  // Tokyo Night light, as the site renders it (design-kit contrast-adjusted text).
  theme: { accent: "#2E7DE9", background: "#E1E2E7", foreground: "#1C3161", muted: "#414C76" },
});

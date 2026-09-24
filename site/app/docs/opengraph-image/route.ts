import { createSocialImage } from "../../social-image";
import { siteDomain } from "../../site";

export const dynamic = "force-static";

export function GET() {
  return createSocialImage({
    description: "The Gobstopper reference: how to install it, choose a compaction strategy, and configure it per provider, per session, or with named presets.",
    domain: siteDomain,
    title: "Gobstopper documentation",
  });
}

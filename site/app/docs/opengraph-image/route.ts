import { createSocialImage } from "../../social-image";
import { siteDomain } from "../../site";
import { readmeTitle } from "../../readme.generated";

export const dynamic = "force-static";

export function GET() {
  return createSocialImage({
    description: "The complete Gobstopper README: installation, strategies, configuration, hooks, telemetry, evaluation, and design notes.",
    domain: siteDomain,
    title: `${readmeTitle} documentation`,
  });
}

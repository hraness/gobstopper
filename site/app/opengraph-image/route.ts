import { createSocialImage } from "../social-image";
import { siteDescription, siteDomain, siteTitle } from "../site";

export const dynamic = "force-static";

export function GET() {
  return createSocialImage({
    description: siteDescription,
    domain: siteDomain,
    title: siteTitle,
  });
}

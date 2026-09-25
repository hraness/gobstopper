import { createSocialImage } from "../social-image";
import { siteDescription, siteDomain, siteName, siteTitle } from "../site";

export const dynamic = "force-static";

export function GET() {
  return createSocialImage({
    description: siteDescription,
    domain: siteDomain,
    eyebrow: siteName,
    title: siteTitle,
  });
}

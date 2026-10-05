import { createSiteSocialImageResponse } from "@hraness/web-discovery/social-image";

import { proxyGuideSocialPage } from "../social-page";
import { socialSite } from "../../../social";

export const dynamic = "force-static";

export function GET() {
  return createSiteSocialImageResponse(socialSite, proxyGuideSocialPage);
}

import { createSiteSocialImageResponse } from "@hraness/web-discovery/social-image";

import { homeSocialPage, socialSite } from "../social";

export const dynamic = "force-static";

export function GET() {
  return createSiteSocialImageResponse(socialSite, homeSocialPage);
}

import { createSiteSocialImageResponse } from "@hraness/web-discovery/social-image";

import { socialSite } from "../../../social";
import { blogPosts, findPost } from "../../articles";
import { postSocialPage } from "../../discovery";

export const dynamic = "force-static";
export const dynamicParams = false;

export function generateStaticParams() {
  return blogPosts.map((post) => ({ slug: post.slug }));
}

export async function GET(_request: Request, { params }: Readonly<{ params: Promise<{ slug: string }> }>) {
  const post = findPost((await params).slug);
  if (post === undefined) return new Response("Not found", { status: 404 });
  return createSiteSocialImageResponse(socialSite, postSocialPage(post));
}

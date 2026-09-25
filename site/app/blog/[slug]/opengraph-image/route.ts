import { createSocialImage } from "../../../social-image";
import { siteDomain } from "../../../site";
import { blogPosts, findPost } from "../../articles";

export const dynamic = "force-static";
export const dynamicParams = false;

export function generateStaticParams() {
  return blogPosts.map((post) => ({ slug: post.slug }));
}

export async function GET(_request: Request, { params }: Readonly<{ params: Promise<{ slug: string }> }>) {
  const post = findPost((await params).slug);
  if (post === undefined) return new Response("Not found", { status: 404 });
  return createSocialImage({
    description: post.dek,
    domain: siteDomain,
    eyebrow: post.eyebrow,
    title: post.title,
  });
}

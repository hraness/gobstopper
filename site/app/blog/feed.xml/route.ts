import { ATOM_FEED_CONTENT_TYPE } from "@hraness/web-discovery";

import { blogAtomFeed } from "../discovery";

export const dynamic = "force-static";

export function GET() {
  return new Response(blogAtomFeed(), {
    headers: { "content-type": ATOM_FEED_CONTENT_TYPE },
  });
}

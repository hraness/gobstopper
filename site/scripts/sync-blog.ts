import { readdir } from "node:fs/promises";
import { resolve } from "node:path";

import { renderPostHtml } from "./blog-html.ts";

const siteRoot = resolve(import.meta.dir, "..");
const contentRoot = resolve(siteRoot, "content/blog");
export const generatedBlogPath = resolve(siteRoot, "app/blog/posts.generated.ts");

/** Render every post body in content/blog into the generated module the pages import. */
export async function generatedBlogModule(): Promise<string> {
  const files = (await readdir(contentRoot)).filter((name) => name.endsWith(".md")).sort();
  const posts: Record<string, ReturnType<typeof renderPostHtml>> = {};
  for (const file of files) {
    const slug = file.slice(0, -".md".length);
    if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/u.test(slug)) throw new Error(`Post file name is not a slug: ${file}`);
    posts[slug] = renderPostHtml(await Bun.file(resolve(contentRoot, file)).text());
  }
  return "// Generated from ../../content/blog by scripts/sync-blog.ts. Do not edit.\n"
    + `export const postBodies = ${JSON.stringify(posts, null, 2)} as const;\n`;
}

if (import.meta.main) {
  await Bun.write(generatedBlogPath, await generatedBlogModule());
}

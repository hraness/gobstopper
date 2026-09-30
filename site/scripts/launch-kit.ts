import { resolve } from "node:path";

import { renderSocialKitMarkdown } from "../app/launch/social-kit-markdown";

export const socialKitPath = resolve(import.meta.dir, "../../kb/launch/social-kit.md");

if (import.meta.main) {
  await Bun.write(socialKitPath, renderSocialKitMarkdown());
}

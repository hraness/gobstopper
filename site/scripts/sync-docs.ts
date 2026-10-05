import { resolve } from "node:path";

import { documentSections } from "./readme-html.ts";
import { PROXY_GUIDE_SOURCE, renderProxyGuideHtml, SERVICE_SOURCE } from "./site-documents.ts";

const siteRoot = resolve(import.meta.dir, "..");
const repositoryRoot = resolve(siteRoot, "..");

if (import.meta.main) {
  const [proxy, service] = await Promise.all([
    Bun.file(resolve(repositoryRoot, PROXY_GUIDE_SOURCE)).text(),
    Bun.file(resolve(repositoryRoot, SERVICE_SOURCE)).text(),
  ]);
  const html = renderProxyGuideHtml(proxy, service);
  await Bun.write(
    resolve(siteRoot, "app/docs/proxy/guide.generated.ts"),
    `// Generated from ../${PROXY_GUIDE_SOURCE} and ../${SERVICE_SOURCE} by scripts/sync-docs.ts. Do not edit.\n`
      + `export const proxyGuideHtml = ${JSON.stringify(html)};\n`
      + `export const proxyGuideSections = ${JSON.stringify(documentSections(html))} as const;\n`,
  );
}

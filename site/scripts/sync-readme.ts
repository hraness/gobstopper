import { basename, resolve } from "node:path";

import { documentSections, readmeLanding, renderReadmeHtml } from "./readme-html.ts";
import { publishedReadme } from "./published-readme.ts";
import { createSiteLinkResolver, SERVICE_SOURCE } from "./site-documents.ts";
import { publishedRelease } from "../app/publication.ts";

const siteRoot = resolve(import.meta.dir, "..");
const repositoryRoot = resolve(siteRoot, "..");

/** The repository is a Cargo workspace; its version lives in Cargo.toml. */
async function workspaceVersion(): Promise<string> {
  const manifest = await Bun.file(resolve(repositoryRoot, "Cargo.toml")).text();
  const version = /^\[workspace\.package\][\s\S]*?^version = "([0-9]+\.[0-9]+\.[0-9]+)"/mu
    .exec(manifest)?.[1];
  if (version === undefined) {
    throw new Error("Cargo.toml workspace version is missing or noncanonical");
  }
  return version;
}

/**
 * Vector figures the README references are copied under public/readme/ and
 * served by the site, so /docs does not depend on the repository's main branch.
 */
async function siteImages(source: string): Promise<ReadonlyMap<string, string>> {
  const images = new Map<string, string>();
  for (const match of source.matchAll(/\]\((docs\/assets\/[a-z0-9-]+\.svg)\)/gu)) {
    const path = match[1]!;
    const route = `/readme/${basename(path)}`;
    await Bun.write(resolve(siteRoot, `public${route}`), Bun.file(resolve(repositoryRoot, path)));
    images.set(path, route);
  }
  return images;
}

if (import.meta.main) {
  const source = await Bun.file(resolve(repositoryRoot, "README.md")).text();
  const service = await Bun.file(resolve(repositoryRoot, SERVICE_SOURCE)).text();
  const version = await workspaceVersion();
  const landing = readmeLanding(source);
  const html = renderReadmeHtml(
    publishedRelease === null
      ? source
      : publishedReadme(source, version, publishedRelease.version),
    { resolveSiteLink: createSiteLinkResolver(service), siteImages: await siteImages(source) },
  );
  await Bun.write(
    resolve(siteRoot, "app/readme.generated.ts"),
    "// Generated from ../README.md by scripts/sync-readme.ts. Do not edit.\n"
      + `export const readmeTitle = ${JSON.stringify(landing.title)};\n`
      + `export const readmeLead = ${JSON.stringify(landing.lead)};\n`
      + `export const readmeHtml = ${JSON.stringify(html)};\n`
      + `export const readmeSections = ${JSON.stringify(documentSections(html))} as const;\n`,
  );
}

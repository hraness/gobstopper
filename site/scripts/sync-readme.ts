import { resolve } from "node:path";

import { readmeLanding, renderReadmeHtml } from "./readme-html.ts";
import { publishedReadme } from "./published-readme.ts";
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

if (import.meta.main) {
  const source = await Bun.file(resolve(repositoryRoot, "README.md")).text();
  const version = await workspaceVersion();
  const landing = readmeLanding(source);
  const html = renderReadmeHtml(
    publishedRelease === null
      ? source
      : publishedReadme(source, version, publishedRelease.version),
  );
  await Bun.write(
    resolve(siteRoot, "app/readme.generated.ts"),
    "// Generated from ../README.md by scripts/sync-readme.ts. Do not edit.\n"
      + `export const readmeTitle = ${JSON.stringify(landing.title)};\n`
      + `export const readmeLead = ${JSON.stringify(landing.lead)};\n`
      + `export const readmeHtml = ${JSON.stringify(html)};\n`,
  );
}

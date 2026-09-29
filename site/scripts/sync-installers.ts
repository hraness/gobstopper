// Serve the release installers from the site: copy scripts/install.sh and
// scripts/install.ps1 into public/, so https://gobstopper.sh/install.sh is
// always the installer from the deployed commit.
import { copyFileSync } from "node:fs";
import { resolve } from "node:path";

const siteRoot = resolve(import.meta.dir, "..");
const repositoryRoot = resolve(siteRoot, "..");

export const installers = ["install.sh", "install.ps1"] as const;

if (import.meta.main) {
  for (const name of installers) {
    copyFileSync(resolve(repositoryRoot, "scripts", name), resolve(siteRoot, "public", name));
  }
}

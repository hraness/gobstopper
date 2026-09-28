// Inlines _film.js into every scenes/film/*.html between the film:begin and film:end
// markers. `bun media/scenes/film/sync.ts --check` exits 1 when a scene is stale.
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const dir = import.meta.dir;
const runtime = readFileSync(join(dir, "_film.js"), "utf8").trimEnd();
const block = `// film:begin (generated from _film.js by sync.ts; do not edit here)\n${runtime}\n// film:end`;
const pattern = /^\/\/ film:begin(?: \(generated[^\n]*)?$[\s\S]*?^\/\/ film:end$/m;
const check = process.argv.includes("--check");
let stale = 0;
for (const name of readdirSync(dir).filter((n) => n.endsWith(".html")).sort()) {
  const path = join(dir, name);
  const html = readFileSync(path, "utf8");
  if (runtime.includes("// film:begin") || runtime.includes("// film:end")) throw new Error("_film.js must not contain the marker lines.");
  if (!pattern.test(html)) throw new Error(`${name} has no film:begin/film:end block.`);
  const next = html.replace(pattern, () => block);
  if (next === html) continue;
  stale++;
  if (check) console.error(`${name} is out of date with _film.js`);
  else writeFileSync(path, next);
}
if (check && stale > 0) process.exit(1);
console.log(check ? "film scenes: in sync" : `film scenes: ${stale} updated`);

// Checks the committed stills against media/stills.json and their receipts:
// size, pixel dimensions, docs/blog byte identity, the exact file sets, and that
// each PNG is the one its latest receipt recorded for the current scene source.
import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { gobFilm } from "../site/app/_data/gob-film";
import { P_GRID, P_SAW, P_TB, P_TB_FULL } from "../site/app/_components/gob-figure";
import { PROV } from "./lib/provenance";
import { FILM_SECONDS, MEDIA, REPO, loadStills } from "./render";

const DOCS = join(REPO, "docs/assets");
const BLOG = join(REPO, "site/public/blog/introducing-gobstopper");
const MAX_BYTES = 600 * 1024;

const jobs = (await loadStills()).filter((job) => !job.outputs.includes("social"));
const sha = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex");
const pngSize = (bytes: Buffer): [number, number] => [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
// gob-film-card.png is a frame of the story film (media/story/publish.ts), not a still job.
const committed = (dir: string) => readdirSync(dir).filter((name) => /^gob-.*\.png$/.test(name) && name !== "gob-film-card.png").sort();
const expected = (output: "docs" | "blog") => jobs.filter((job) => job.outputs.includes(output)).map((job) => `gob-${job.id}.png`).sort();

describe("still outputs", () => {
  test("the committed file sets are exactly the jobs' outputs", () => {
    expect(committed(DOCS)).toEqual(expected("docs"));
    expect(committed(BLOG)).toEqual(expected("blog"));
    expect(expected("docs")).toHaveLength(9);
    expect(expected("blog")).toHaveLength(10);
  });

  for (const job of jobs) {
    test(`${job.id}: 2400×1350, at most 600 KB, one set of bytes, matches its receipt`, () => {
      const paths = job.outputs.map((output) => join(output === "docs" ? DOCS : BLOG, `gob-${job.id}.png`));
      const files = paths.map((path) => readFileSync(path));
      for (const bytes of files) {
        expect(bytes.subarray(1, 4).toString("latin1")).toBe("PNG");
        expect(pngSize(bytes)).toEqual([2400, 1350]);
        expect(bytes.length).toBeLessThanOrEqual(MAX_BYTES);
      }
      const hashes = new Set(files.map(sha));
      expect(hashes.size).toBe(1);

      const receipt = JSON.parse(readFileSync(join(MEDIA, "out/receipts", `still-${job.id}.json`), "utf8"));
      const latest = receipt.renders.at(-1);
      expect(latest.sceneSource).toBe(job.scene);
      expect(latest.sceneSourceSha256).toBe(sha(readFileSync(join(MEDIA, job.scene))));
      expect(latest.output.sha256).toBe([...hashes][0]);
      expect([latest.output.width, latest.output.height]).toEqual([2400, 1350]);
    });
  }
});

describe("still sources", () => {
  test("the brand mark is a byte copy of the site's", () => {
    expect(sha(readFileSync(join(MEDIA, "brand/mark.svg")))).toBe(sha(readFileSync(join(REPO, "site/public/marks/gobstopper.svg"))));
  });

  test("no scene inlines a provenance line; they come from Gob.PROV", () => {
    for (const job of jobs) {
      const source = readFileSync(join(MEDIA, job.scene), "utf8");
      for (const line of Object.values(PROV)) expect(source.includes(line.slice(0, 40))).toBe(false);
    }
    expect(PROV.tbFull.startsWith(PROV.tb)).toBe(true);
    expect(PROV.tb).toContain("45,000-token threshold (default 128,000)");
  });

  test("each provenance line is the one the site's figures print", () => {
    expect(PROV).toEqual({ tb: P_TB, tbFull: P_TB_FULL, saw: P_SAW, grid: P_GRID });
  });

  test("the README's film card states the published film's length", () => {
    // The card is a frame of the story film (media/story/publish.ts), not a still job.
    if (gobFilm !== null) expect(readFileSync(join(REPO, "README.md"), "utf8")).toContain(`Play the ${gobFilm.durationSeconds}-second Gobstopper film`);
  });
});

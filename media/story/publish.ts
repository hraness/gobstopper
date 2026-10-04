/**
 * Copies the delivered story film into site/public/media and rewrites the
 * film manifest (site/app/_data/gob-film.ts) from the delivered bytes and the
 * caption cues. Run after `bash render-all.sh gobstopper-film`.
 */
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const here = import.meta.dir, repo = join(here, "../.."), site = join(repo, "site");
const out = (name: string) => join(here, "out", name);
const pub = (name: string) => join(site, "public/media", name);
mkdirSync(pub("cuts"), { recursive: true });
copyFileSync(out("gobstopper-film-1080p.mp4"), pub("gobstopper-film-1080p.mp4"));
copyFileSync(out("gobstopper-film-poster.jpg"), pub("gobstopper-film-poster.jpg"));
copyFileSync(out("gobstopper-film.en.vtt"), pub("gobstopper-film.en.vtt"));
copyFileSync(out("gobstopper-film-1x1.mp4"), pub("cuts/gobstopper-film-1x1.mp4"));
copyFileSync(out("gobstopper-film-9x16.mp4"), pub("cuts/gobstopper-film-9x16.mp4"));
const timeline = JSON.parse(readFileSync(join(here, "build/timeline.json"), "utf8")) as { seconds: number; posterAt: number };
// The README and launch-post film card: a 2400x1350 frame of the film with a play button.
const card = join(here, "out/gob-film-card.png");
execFileSync("ffmpeg", ["-loglevel", "error", "-y", "-ss", String(timeline.posterAt), "-i", join(here, "build/master-wide.mp4"), "-frames:v", "1", "-vf", "scale=2400:1350:flags=lanczos", join(here, "out/card-frame.png")]);
execFileSync("magick", [join(here, "out/card-frame.png"), "-fill", "rgba(0,0,0,0.55)", "-draw", "circle 1200,675 1200,555", "-fill", "#c0caf5", "-draw", "polygon 1160,610 1160,740 1270,675", "-colors", "256", card]);
copyFileSync(card, join(repo, "docs/assets/gob-film-card.png"));
copyFileSync(card, join(site, "public/blog/introducing-gobstopper/gob-film-card.png"));
const video = readFileSync(pub("gobstopper-film-1080p.mp4"));
const vtt = readFileSync(pub("gobstopper-film.en.vtt"), "utf8");
const seconds = (stamp: string) => { const [h, m, s] = stamp.split(":"); return Number(h) * 3600 + Number(m) * 60 + Number(s); };
const beats = [...vtt.matchAll(/(\d{2}:\d{2}:\d{2}\.\d{3}) --> (\d{2}:\d{2}:\d{2}\.\d{3})\n([^\n]+)/gu)]
  .map((m) => ({ start: Number(seconds(m[1]!).toFixed(3)), end: Number(seconds(m[2]!).toFixed(3)), text: m[3]!.trim() }));
const manifest = join(site, "app/_data/gob-film.ts");
const source = readFileSync(manifest, "utf8");
const head = source.slice(0, source.indexOf("export const gobFilm"));
writeFileSync(manifest, `${head}export const gobFilm: GobFilm | null = {
  src: "/media/gobstopper-film-1080p.mp4",
  poster: "/media/gobstopper-film-poster.jpg",
  captions: "/media/gobstopper-film.en.vtt",
  width: 1920, height: 1080, durationSeconds: ${Math.round(timeline.seconds)},
  bytes: ${statSync(pub("gobstopper-film-1080p.mp4")).size},
  sha256: "${createHash("sha256").update(video).digest("hex")}",
  beats: [
${beats.map((b) => `    { start: ${b.start}, end: ${b.end}, text: ${JSON.stringify(b.text)} },`).join("\n")}
  ],
};
`);
console.log(JSON.stringify({ seconds: timeline.seconds, beats: beats.length, bytes: video.byteLength }));

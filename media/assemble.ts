// Assemble the launch film (SPEC section 6.4).
//
//   bun media/assemble.ts           final: 4K master, 1080p delivery, poster, captions, manifest, receipt
//   bun media/assemble.ts --draft   review cut at 1080p from the DSF 1 drafts; writes nothing under site/
//
// Inputs: media/shots.json, media/out/shots/<id>.mp4 (or .draft.mp4), media/out/score.wav.
// Every shot but the last carries a 0.4 s handle, and xfade offset k is the sum of the nominal
// lengths of shots 1..k, so the assembled timeline equals the shots.json timings exactly.

import { createHash } from "node:crypto";
import { mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const MEDIA = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(MEDIA, "..");
const OUT = join(MEDIA, "out");
const SITE_MEDIA = join(REPO, "site", "public", "media");
const MANIFEST = join(REPO, "site", "app", "_data", "gob-film.ts");

const FPS = 30;
const LENGTH = 75;
const FRAMES = LENGTH * FPS;
const XFADE = 0.4;
const MAX_BYTES = 10_000_000;
const MAX_POSTER = 300 * 1024;
const LOUDNESS = { I: -16, TP: -1.5, LRA: 11 };
/**
 * The low-bitrate AAC encode overshoots the PCM true peak by about 3 dB on the fuse chime,
 * so after the SPEC loudnorm pass a limiter holds sample peaks this far down, and step 6
 * asserts both delivered files stay at or under MAX_TRUE_PEAK dBTP.
 */
const LIMIT_DB = -5;
const MAX_TRUE_PEAK = -1;
const SOCIAL = join(OUT, "social", "gobstopper-terminal-bench-1200x630.png");

type Beat = { n: number; start: number; end: number; text: string; cue?: string };
type Shot = { id: string; start: number; end: number; beats?: Beat[] };
type ShotList = { fps: number; handle: number; shots: Shot[] };

const commands: string[][] = [];

async function run(args: string[], deadlineMs = 3_600_000): Promise<{ stdout: string; stderr: string }> {
  commands.push(args);
  const proc = Bun.spawn(args, { stdout: "pipe", stderr: "pipe" });
  const timer = setTimeout(() => proc.kill(), deadlineMs);
  const [stdout, stderr, code] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text(), proc.exited]);
  clearTimeout(timer);
  if (code !== 0) throw new Error(`${args.slice(0, 3).join(" ")} failed (${code}):\n${stderr.slice(-2000)}`);
  return { stdout, stderr };
}

async function sha(path: string): Promise<string> {
  return createHash("sha256").update(await readFile(path)).digest("hex");
}

async function bytes(path: string): Promise<number> {
  return (await stat(path)).size;
}

const rel = (p: string) => relative(REPO, p);

function vttTime(s: number): string {
  const ms = Math.round(s * 1000);
  const pad = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${pad(Math.floor(ms / 3_600_000))}:${pad(Math.floor(ms / 60_000) % 60)}:${pad(Math.floor(ms / 1000) % 60)}.${pad(ms % 1000, 3)}`;
}

export function captions(list: ShotList): string {
  const beats = list.shots.flatMap((s) => s.beats ?? []).sort((a, b) => a.n - b.n);
  if (beats.length !== 16) throw new Error(`expected 16 beats, found ${beats.length}`);
  // A beat can carry WebVTT cue settings, e.g. the end card's caption sits at the top so it
  // never stacks under the command drawn in the centre.
  return "WEBVTT\n\n" + beats.map((b) => `${b.n}\n${vttTime(b.start)} --> ${vttTime(b.end)}${b.cue ? ` ${b.cue}` : ""}\n${b.text}\n`).join("\n");
}

export function xfadeGraph(list: ShotList): { graph: string; offsets: number[] } {
  const n = list.shots.length;
  const parts = list.shots.map((_, i) => `[${i}:v]settb=AVTB,fps=${FPS},format=yuv420p[s${i}]`);
  const offsets: number[] = [];
  let acc = 0;
  let prev = "s0";
  for (let k = 1; k < n; k++) {
    acc += list.shots[k - 1]!.end - list.shots[k - 1]!.start;
    offsets.push(Number(acc.toFixed(3)));
    const label = k === n - 1 ? "vout" : `x${k}`;
    parts.push(`[${prev}][s${k}]xfade=transition=fade:duration=${XFADE}:offset=${acc.toFixed(3)}[${label}]`);
    prev = label;
  }
  return { graph: parts.join(";"), offsets };
}

async function probe(path: string, countFrames: boolean) {
  const args = ["ffprobe", "-v", "error", ...(countFrames ? ["-count_frames"] : []), "-show_entries",
    "stream=codec_type,width,height,nb_read_frames,sample_rate,channels,r_frame_rate:format=duration", "-of", "json", path];
  const { stdout } = await run(args, 300_000);
  const j = JSON.parse(stdout) as { streams: Record<string, unknown>[]; format: { duration: string } };
  const v = j.streams.find((s) => s.codec_type === "video") ?? {};
  const a = j.streams.find((s) => s.codec_type === "audio");
  return {
    width: Number(v.width), height: Number(v.height), frames: countFrames ? Number(v.nb_read_frames) : undefined,
    rate: String(v.r_frame_rate), duration: Number(j.format.duration),
    audio: a ? { sampleRate: Number(a.sample_rate), channels: Number(a.channels) } : null,
  };
}

async function integratedLoudness(path: string): Promise<{ I: number; peak: number }> {
  const { stderr } = await run(["ffmpeg", "-hide_banner", "-nostats", "-i", path, "-map", "0:a", "-af", "ebur128=peak=true", "-f", "null", "-"], 300_000);
  const summary = stderr.slice(stderr.lastIndexOf("Summary:"));
  const I = Number(/I:\s+(-?[\d.]+) LUFS/.exec(summary)?.[1]);
  const peak = Number(/Peak:\s+(-?[\d.]+) dBFS/.exec(summary)?.[1]);
  return { I, peak };
}

async function loudnorm(score: string, dest: string): Promise<Record<string, unknown>> {
  const base = `loudnorm=I=${LOUDNESS.I}:TP=${LOUDNESS.TP}:LRA=${LOUDNESS.LRA}`;
  const first = await run(["ffmpeg", "-hide_banner", "-nostats", "-i", score, "-af", `${base}:print_format=json`, "-f", "null", "-"]);
  const m = JSON.parse(first.stderr.slice(first.stderr.lastIndexOf("{"), first.stderr.lastIndexOf("}") + 1)) as Record<string, string>;
  const second = await run(["ffmpeg", "-hide_banner", "-nostats", "-y", "-i", score, "-af",
    `${base}:measured_I=${m.input_i}:measured_TP=${m.input_tp}:measured_LRA=${m.input_lra}:measured_thresh=${m.input_thresh}:offset=${m.target_offset}:linear=true:print_format=json,aresample=48000,alimiter=limit=${LIMIT_DB}dB:level=false:attack=1:release=40`,
    "-t", String(LENGTH), "-c:a", "pcm_s24le", "-ar", "48000", "-ac", "2", dest]);
  const r = JSON.parse(second.stderr.slice(second.stderr.lastIndexOf("{"), second.stderr.lastIndexOf("}") + 1)) as Record<string, string>;
  return { measured: m, result: r, normalizationType: r.normalization_type };
}

function assertClose(label: string, actual: number, expected: number, tolerance: number): void {
  if (!(Math.abs(actual - expected) <= tolerance)) throw new Error(`${label}: ${actual}, expected ${expected} ±${tolerance}`);
}

async function main(argv: string[]): Promise<void> {
  const draft = argv.includes("--draft");
  const list = JSON.parse(await readFile(join(MEDIA, "shots.json"), "utf8")) as ShotList;
  const total = list.shots.reduce((t, s) => t + (s.end - s.start), 0);
  assertClose("nominal length", total, LENGTH, 1e-9);
  if (list.fps !== FPS || list.handle !== XFADE) throw new Error("shots.json fps/handle disagree with SPEC 6.1");

  const shotFiles = list.shots.map((s) => join(OUT, "shots", `${s.id}${draft ? ".draft" : ""}.mp4`));
  const score = join(OUT, "score.wav");
  // The film release attaches the social card beside the master (SPEC 6.6), so a final
  // assemble refuses to write a SHA256SUMS that would not cover it.
  for (const f of [...shotFiles, score, ...(draft ? [] : [SOCIAL])]) if (!existsSync(f)) throw new Error(`missing input ${rel(f)}`);

  // Every shot but the last is its nominal length plus the handle; the last has no handle.
  const inputs: Record<string, unknown>[] = [];
  for (const [i, s] of list.shots.entries()) {
    const p = await probe(shotFiles[i]!, false);
    const want = s.end - s.start + (i === list.shots.length - 1 ? 0 : XFADE);
    assertClose(`${s.id} duration`, p.duration, want, 1.5 / FPS);
    const [w, h] = draft ? [1920, 1080] : [3840, 2160];
    if (p.width !== w || p.height !== h) throw new Error(`${s.id} is ${p.width}x${p.height}, expected ${w}x${h}`);
    inputs.push({ id: s.id, path: rel(shotFiles[i]!), sha256: await sha(shotFiles[i]!), duration: p.duration });
  }

  const { graph, offsets } = xfadeGraph(list);
  const shotArgs = shotFiles.flatMap((f) => ["-i", f]);
  await mkdir(join(OUT, "receipts"), { recursive: true });
  const normWav = join(OUT, "score-loudnorm.wav");
  const norm = await loudnorm(score, normWav);
  if (norm.normalizationType !== "linear") console.warn(`loudnorm fell back to ${String(norm.normalizationType)} normalization`);

  if (draft) {
    const review = join(OUT, "review", "film-draft.mp4");
    await mkdir(dirname(review), { recursive: true });
    await run(["ffmpeg", "-hide_banner", "-nostats", "-y", ...shotArgs, "-i", normWav, "-filter_complex", graph,
      "-map", "[vout]", "-map", `${shotFiles.length}:a`, "-t", String(LENGTH), "-r", String(FPS),
      "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p",
      "-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart", review]);
    const p = await probe(review, true);
    console.log(JSON.stringify({ draft: rel(review), offsets, frames: p.frames, duration: p.duration, loudness: await integratedLoudness(review), normalization: norm.normalizationType }));
    return;
  }

  // 1. Picture: 4K xfade chain.
  const video4k = join(OUT, "film-2160p-video.mp4");
  await run(["ffmpeg", "-hide_banner", "-nostats", "-y", ...shotArgs, "-filter_complex", graph, "-map", "[vout]",
    "-t", String(LENGTH), "-c:v", "libx264", "-preset", "slow", "-crf", "17", "-profile:v", "high", "-level", "5.2",
    "-pix_fmt", "yuv420p", "-r", String(FPS), video4k], 3 * 3_600_000);

  // 2. Audio mux for the 4K master.
  const master = join(OUT, "gobstopper-film-2160p.mp4");
  await run(["ffmpeg", "-hide_banner", "-nostats", "-y", "-i", video4k, "-i", normWav, "-map", "0:v", "-map", "1:a",
    "-t", String(LENGTH), "-c:v", "copy", "-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2", "-movflags", "+faststart", master]);

  // 3. 1080p delivery, stepping crf up and maxrate down until it fits.
  await mkdir(SITE_MEDIA, { recursive: true });
  const hd = join(SITE_MEDIA, "gobstopper-film-1080p.mp4");
  let crf = 23;
  let maxrate = 900;
  for (let step = 0; ; step++) {
    await run(["ffmpeg", "-hide_banner", "-nostats", "-y", "-i", video4k, "-i", normWav, "-map", "0:v", "-map", "1:a",
      "-t", String(LENGTH), "-vf", "scale=1920:1080:flags=lanczos", "-c:v", "libx264", "-preset", "slow", "-tune", "animation",
      "-crf", String(crf), "-maxrate", `${maxrate}k`, "-bufsize", `${maxrate * 2}k`, "-profile:v", "high", "-level", "4.0",
      "-pix_fmt", "yuv420p", "-r", String(FPS), "-c:a", "aac", "-b:a", "96k", "-ar", "48000", "-ac", "2", "-movflags", "+faststart", hd], 3_600_000);
    if ((await bytes(hd)) <= MAX_BYTES) break;
    if (step === 3) throw new Error(`1080p is ${await bytes(hd)} bytes after 3 re-encodes; limit ${MAX_BYTES}`);
    crf += 2;
    maxrate -= 100;
  }

  // 4. Poster at 50.0 s.
  const poster = join(SITE_MEDIA, "gobstopper-film-poster.jpg");
  for (const q of [3, 4, 5, 6]) {
    await run(["ffmpeg", "-hide_banner", "-nostats", "-y", "-ss", "50.0", "-i", hd, "-frames:v", "1", "-q:v", String(q), poster]);
    if ((await bytes(poster)) <= MAX_POSTER) break;
  }
  if ((await bytes(poster)) > MAX_POSTER) throw new Error(`poster is ${await bytes(poster)} bytes; limit ${MAX_POSTER}`);

  // 5. Captions: beat text only.
  const vtt = join(SITE_MEDIA, "gobstopper-film.en.vtt");
  await writeFile(vtt, captions(list));

  // 6. Verification.
  const p4k = await probe(master, true);
  const phd = await probe(hd, true);
  for (const [label, p, w, h] of [["2160p", p4k, 3840, 2160], ["1080p", phd, 1920, 1080]] as const) {
    assertClose(`${label} frames`, p.frames ?? 0, FRAMES, 1);
    assertClose(`${label} duration`, p.duration, LENGTH, 0.05);
    if (p.width !== w || p.height !== h) throw new Error(`${label} is ${p.width}x${p.height}`);
    if (p.audio?.sampleRate !== 48000 || p.audio.channels !== 2) throw new Error(`${label} audio is ${JSON.stringify(p.audio)}`);
  }
  const hdBytes = await bytes(hd);
  if (hdBytes > MAX_BYTES) throw new Error(`1080p is ${hdBytes} bytes`);
  const loud4k = await integratedLoudness(master);
  const loudHd = await integratedLoudness(hd);
  assertClose("2160p loudness", loud4k.I, LOUDNESS.I, 1);
  assertClose("1080p loudness", loudHd.I, LOUDNESS.I, 1);
  for (const [label, l] of [["2160p", loud4k], ["1080p", loudHd]] as const) {
    if (!(l.peak <= MAX_TRUE_PEAK)) throw new Error(`${label} true peak is ${l.peak} dBTP; limit ${MAX_TRUE_PEAK}`);
  }

  // 7. Receipt, and SHA256SUMS for the film release.
  const outputs: Record<string, { bytes: number; sha256: string }> = {};
  for (const f of [master, hd, poster, vtt]) outputs[rel(f)] = { bytes: await bytes(f), sha256: await sha(f) };
  const hdSha = outputs[rel(hd)]!.sha256;
  const sums = [master, SOCIAL];
  await writeFile(join(OUT, "SHA256SUMS"), (await Promise.all(sums.map(async (f) => `${await sha(f)}  ${f.split("/").pop()}`))).join("\n") + "\n");
  await writeFile(join(OUT, "receipts", "film.json"), JSON.stringify({
    assembledAt: new Date().toISOString(),
    inputs: { shots: inputs, score: { path: rel(score), sha256: await sha(score) }, shotsJson: { path: "media/shots.json", sha256: await sha(join(MEDIA, "shots.json")) } },
    offsets, loudnorm: norm, commands: commands.map((c) => c.map((a) => (a.startsWith(REPO) ? rel(a) : a)).join(" ")),
    probe: { "2160p": p4k, "1080p": phd }, loudness: { "2160p": loud4k, "1080p": loudHd }, outputs,
  }, null, 2) + "\n");

  // 8. Manifest: only the gobFilm value changes, never the types.
  const beats = list.shots.flatMap((s) => s.beats ?? []).sort((a, b) => a.n - b.n).map((b) => ({ start: b.start, end: b.end, text: b.text }));
  const value = [
    "export const gobFilm: GobFilm | null = {",
    '  src: "/media/gobstopper-film-1080p.mp4",',
    '  poster: "/media/gobstopper-film-poster.jpg",',
    '  captions: "/media/gobstopper-film.en.vtt",',
    "  width: 1920, height: 1080, durationSeconds: 75,",
    `  bytes: ${hdBytes},`,
    `  sha256: "${hdSha}",`,
    "  beats: [",
    ...beats.map((b) => `    { start: ${b.start.toFixed(1)}, end: ${b.end.toFixed(1)}, text: ${JSON.stringify(b.text)} },`),
    "  ],",
    "};",
  ].join("\n");
  const source = await readFile(MANIFEST, "utf8");
  const at = source.indexOf("export const gobFilm");
  if (at < 0) throw new Error("gob-film.ts has no gobFilm export");
  await writeFile(MANIFEST, source.slice(0, at) + value + "\n");

  await rm(video4k, { force: true });
  await rm(normWav, { force: true });
  console.log(JSON.stringify({ offsets, outputs, loudness: { "2160p": loud4k.I, "1080p": loudHd.I }, normalization: norm.normalizationType }));
}

if (import.meta.main) {
  main(process.argv.slice(2)).catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : error);
    process.exit(1);
  });
}

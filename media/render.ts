/**
 * The only entry point that calls Slopcamera. It builds each scene (the scene HTML plus
 * the inlined `Gob` runtime), writes a slopcamera.html-scene request, dry-runs it, takes
 * the render lock, renders, collects the PNG or MP4, writes a receipt and cleans up.
 *
 *   bun media/render.ts still <id> [--dry-run]
 *   bun media/render.ts stills [--dry-run]
 *   bun media/render.ts shot <shotId> [--draft] [--dry-run]
 *   bun media/render.ts shots [--draft] [--dry-run]
 *   bun media/render.ts lock hold --lane <name>      # hold the lock across several runs
 *   bun media/render.ts lock release --token <token>
 *   bun media/render.ts lock status
 *
 * Exactly one Chrome-owning Slopcamera render runs on the machine at a time. The lock
 * lives in the repository's shared Git directory, so every worktree sees the same one.
 */
import { createHash, randomBytes } from "node:crypto";
import { existsSync, statfsSync } from "node:fs";
import { copyFile, mkdir, open, readFile, rm, stat, unlink, writeFile } from "node:fs/promises";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { F, GAP_ROWS } from "../site/app/_lib/gobbench-format";
import { ARM_ORDER, arm, replayGrid, sawtooth, terminalBench } from "../site/app/_lib/gobbench-data";
import { PALETTES, type PaletteName } from "./lib/palette";
import { RESOURCES } from "./lib/stage";

export const MEDIA = dirname(fileURLToPath(import.meta.url));
export const REPO = resolve(MEDIA, "..");
const OUT = join(MEDIA, "out");
const BUILD = join(OUT, "build");
const RECEIPTS = join(OUT, "receipts");
const DATA_DIR = join(REPO, "site/public/benchmarks/2026-09-28");
const DATA_FILES = ["terminal-bench-results.json", "replay-grid.json", "sawtooth-series.json"] as const;
const SLOPCAMERA = process.env.SLOPCAMERA_BIN ?? join(process.env.HOME ?? "", ".bun/bin/slopcamera");
const EXPECTED_SLOPCAMERA = "3.4.0";
const FPS = 30;
const SEED = 20260928;
const MAX_HTML_BYTES = 1024 * 1024;
const MAX_PARAMETER_BYTES = 64 * 1024;
const MAX_STILL_BYTES = 600 * 1024;
const LOCK_POLL_MS = 30_000;
const LOCK_WAIT_MS = 20 * 60_000;
const RENDER_TIMEOUT_MS = 60 * 60_000;
const GIB = 1024 ** 3;

type Output = "docs" | "blog" | "social";

export interface StillJob {
  id: string;
  scene: string;
  palette: PaletteName;
  size: [number, number];
  hold: number;
  outputs: Output[];
  data?: string[];
}

/** One entry of `shots.json` (the film's shot list). Beat and cue times are absolute. */
export interface ShotJob {
  id: string;
  scene: string;
  start: number;
  end: number;
  beats?: unknown[];
  cues?: unknown[];
  /** Night unless a shot says otherwise. */
  palette?: PaletteName;
  /** Extra named data slices from `dataSlice`. */
  data?: string[];
}

export interface ShotList {
  fps: number;
  handle: number;
  lowerThird?: { text: string; from: number; to: number };
  shots: ShotJob[];
}

/** The named data slices a scene may ask for; each is small and derived from the three files. */
function dataSlice(name: string): unknown {
  switch (name) {
    case "tokens":
      return ARM_ORDER.map((id) => ({ id, ...arm(id).tokens_all_trials, resolved: arm(id).resolved, n: arm(id).n_tasks }));
    case "solved":
      return {
        arms: ARM_ORDER.map((id) => ({ id, resolved: arm(id).resolved, n: arm(id).n_tasks })),
        reported: terminalBench.paper_reference.reported,
      };
    case "sawtooth": {
      const run = sawtooth.runs.find((entry) => entry.threshold === 45000 && entry.keep_tail_percent === 0);
      if (run === undefined) throw new Error("sawtooth-series.json has no 45,000-token tail 0 run.");
      return {
        without: sawtooth.without_proxy,
        with: run.with_proxy,
        compactions: run.compaction_requests,
        threshold: run.applied_threshold_est_tokens,
      };
    }
    case "grid":
      return {
        pooled: replayGrid.pooled
          .filter((row) => row.keep_tail_percent === 0 || row.keep_tail_percent === 40)
          .map((row) => ({ threshold: row.threshold, tail: row.keep_tail_percent, cut: row.pooled_input_cut })),
        violations: replayGrid.pairing_violations_total.proxy_introduced,
        cells: replayGrid.pairing_violations_total.cells,
      };
    case "gap":
      return GAP_ROWS;
    case "churn":
      return terminalBench.churn;
    default:
      throw new Error(`Unknown data slice ${name}.`);
  }
}

/**
 * Slopcamera accepts parameter arrays of at most 128 items. A longer array travels as
 * `{ "$chunks": [[…128], […128], …] }`, and `lib/stage.ts` joins it back before the
 * scene reads `s.params`.
 */
export const MAX_PARAMETER_ARRAY = 128;
function packArrays(value: unknown): unknown {
  if (Array.isArray(value)) {
    const items = value.map(packArrays);
    if (items.length <= MAX_PARAMETER_ARRAY) return items;
    const chunks: unknown[][] = [];
    for (let i = 0; i < items.length; i += MAX_PARAMETER_ARRAY) chunks.push(items.slice(i, i + MAX_PARAMETER_ARRAY));
    if (chunks.length > MAX_PARAMETER_ARRAY) throw new Error(`A ${items.length}-item array is too long for scene parameters.`);
    return { $chunks: chunks };
  }
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, packArrays(item)]));
  }
  return value;
}

function sha256(bytes: Uint8Array | string): string {
  return createHash("sha256").update(bytes).digest("hex");
}

async function fileSha(path: string): Promise<string> {
  return sha256(new Uint8Array(await readFile(path)));
}

const ID = /^[a-z0-9][a-z0-9-]{0,63}$/u;

export async function loadStills(): Promise<StillJob[]> {
  const jobs = JSON.parse(await readFile(join(MEDIA, "stills.json"), "utf8")) as StillJob[];
  for (const job of jobs) {
    if (!ID.test(job.id)) throw new Error(`Bad still id ${job.id}`);
    if (!(job.palette in PALETTES)) throw new Error(`${job.id}: unknown palette ${job.palette}`);
    if (job.size.some((n) => !Number.isInteger(n) || n % 2 !== 0)) throw new Error(`${job.id}: size must be even integers`);
  }
  return jobs;
}

export async function loadShots(): Promise<ShotList> {
  const path = join(MEDIA, "shots.json");
  if (!existsSync(path)) throw new Error("media/shots.json does not exist yet.");
  const list = JSON.parse(await readFile(path, "utf8")) as ShotList;
  if (list.fps !== FPS) throw new Error(`shots.json fps must be ${FPS}.`);
  if (!(list.handle >= 0)) throw new Error("shots.json handle must be zero or more.");
  for (const shot of list.shots) {
    if (!ID.test(shot.id)) throw new Error(`Bad shot id ${shot.id}`);
    if (shot.palette !== undefined && !(shot.palette in PALETTES)) throw new Error(`${shot.id}: unknown palette ${shot.palette}`);
    if (!(shot.end > shot.start)) throw new Error(`${shot.id}: end must be after start`);
  }
  return list;
}

/** Bundle the runtime once per process. */
let runtime: Promise<string> | undefined;
function bundleRuntime(): Promise<string> {
  runtime ??= (async () => {
    const result = await Bun.build({
      entrypoints: [join(MEDIA, "lib/bundle.ts")],
      target: "browser",
      format: "iife",
      minify: true,
    });
    if (!result.success) throw new AggregateError(result.logs, "Bundling media/lib/bundle.ts failed.");
    const output = result.outputs[0];
    if (output === undefined) throw new Error("Bundling produced no output.");
    // An inline script must not contain its own end tag.
    return (await output.text()).replaceAll("</script", "<\\/script");
  })();
  return runtime;
}

async function buildScene(job: string, scenePath: string): Promise<{ htmlPath: string; htmlSha: string }> {
  const source = await readFile(join(MEDIA, scenePath), "utf8");
  if (!source.includes("</head>")) throw new Error(`${scenePath} needs a </head> for the runtime.`);
  const html = source.replace("</head>", `<script>${await bundleRuntime()}</script>\n</head>`);
  const bytes = Buffer.byteLength(html);
  if (bytes > MAX_HTML_BYTES) throw new Error(`${job}: built HTML is ${bytes} bytes, over 1 MB.`);
  await mkdir(BUILD, { recursive: true });
  const htmlPath = join(BUILD, `${job}.html`);
  await writeFile(htmlPath, html);
  return { htmlPath, htmlSha: sha256(html) };
}

function sceneRequest(o: {
  job: string; htmlPath: string; palette: PaletteName; width: number; height: number; dsf: number;
  durationUs: number; timeOffsetSeconds: number; data: string[]; extra?: Record<string, unknown>;
  layoutZoom?: number;
}) {
  const data: Record<string, unknown> = { ...o.extra };
  for (const name of o.data) data[name] = dataSlice(name);
  const parameters = {
    palette: o.palette, F, data: packArrays(data), timeOffsetSeconds: o.timeOffsetSeconds,
    ...(o.layoutZoom !== undefined && o.layoutZoom !== 1 ? { layoutZoom: o.layoutZoom } : {}),
  };
  const parameterBytes = Buffer.byteLength(JSON.stringify(parameters));
  if (parameterBytes > MAX_PARAMETER_BYTES) throw new Error(`${o.job}: parameters are ${parameterBytes} bytes, over 64 KB.`);
  return {
    kind: "slopcamera.html-scene",
    schemaVersion: 1,
    name: o.job,
    document: { path: relative(MEDIA, o.htmlPath) },
    canvas: { width: o.width, height: o.height, deviceScaleFactor: o.dsf },
    timing: { durationUs: o.durationUs, fps: FPS },
    seed: SEED,
    libraries: ["motion"],
    resources: RESOURCES.map((r) => ({ path: r.path, name: r.name, urlPath: r.urlPath, mediaType: r.mediaType })),
    parameters,
    background: PALETTES[o.palette].background,
  };
}

// ---------------------------------------------------------------------------------------
// Render lock

function lockPath(): string {
  if (process.env.GOB_RENDER_LOCK) return process.env.GOB_RENDER_LOCK;
  const common = Bun.spawnSync(["git", "-C", REPO, "rev-parse", "--path-format=absolute", "--git-common-dir"]);
  const dir = common.stdout.toString().trim();
  if (common.exitCode !== 0 || dir === "") throw new Error("Cannot find the shared Git directory for the render lock.");
  return join(dir, "gob-render.lock");
}

interface LockRecord { lane: string; job: string; pid: number; token: string; held: boolean; startedAt: string }

async function readLock(path: string): Promise<LockRecord | undefined> {
  try {
    return JSON.parse(await readFile(path, "utf8")) as LockRecord;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  }
}

async function createLock(path: string, record: LockRecord): Promise<boolean> {
  try {
    const handle = await open(path, "wx", 0o600);
    try { await handle.writeFile(JSON.stringify(record) + "\n"); } finally { await handle.close(); }
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "EEXIST") return false;
    throw error;
  }
}

/**
 * Take the lock for one run, or join a session lock this process was handed through
 * GOB_RENDER_TOKEN. Waits up to 20 minutes, polling every 30 s, and never removes a lock
 * it does not own.
 */
async function acquire(lane: string, job: string, log: (s: string) => void): Promise<() => Promise<void>> {
  const path = lockPath();
  const held = process.env.GOB_RENDER_TOKEN;
  const deadline = Date.now() + LOCK_WAIT_MS;
  for (;;) {
    const current = await readLock(path);
    if (current !== undefined && held !== undefined && current.token === held) return async () => undefined;
    if (current === undefined) {
      const token = randomBytes(16).toString("hex");
      const record = { lane, job, pid: process.pid, token, held: false, startedAt: new Date().toISOString() };
      if (await createLock(path, record)) {
        return async () => {
          const now = await readLock(path);
          if (now?.token === token) await unlink(path);
        };
      }
      continue;
    }
    if (Date.now() >= deadline) {
      throw new Error(`The render lock is held by lane ${current.lane} (${current.job}) since ${current.startedAt}; gave up after 20 minutes.`);
    }
    log(JSON.stringify({ state: "waiting-for-lock", holder: current.lane, job: current.job }));
    await Bun.sleep(LOCK_POLL_MS);
  }
}

async function lockCommand(args: string[]): Promise<void> {
  const path = lockPath();
  const [action, ...rest] = args;
  const option = (name: string) => {
    const index = rest.indexOf(name);
    return index >= 0 ? rest[index + 1] : undefined;
  };
  if (action === "status") {
    const current = await readLock(path);
    console.log(JSON.stringify(current === undefined ? { state: "free" } : { state: "held", lane: current.lane, job: current.job, startedAt: current.startedAt, held: current.held }));
    return;
  }
  if (action === "hold") {
    const lane = option("--lane");
    if (lane === undefined || !ID.test(lane)) throw new Error("lock hold needs --lane <name>.");
    const token = randomBytes(16).toString("hex");
    const deadline = Date.now() + LOCK_WAIT_MS;
    while (!(await createLock(path, { lane, job: "session", pid: process.pid, token, held: true, startedAt: new Date().toISOString() }))) {
      if (Date.now() >= deadline) throw new Error("The render lock stayed busy for 20 minutes.");
      await Bun.sleep(LOCK_POLL_MS);
    }
    console.log(JSON.stringify({ state: "held", token, hint: "export GOB_RENDER_TOKEN=<token> for the runs, then lock release --token <token>" }));
    return;
  }
  if (action === "release") {
    const token = option("--token");
    const current = await readLock(path);
    if (current === undefined) { console.log(JSON.stringify({ state: "free" })); return; }
    if (token === undefined || current.token !== token) throw new Error("The lock belongs to another run; it was not released.");
    await unlink(path);
    console.log(JSON.stringify({ state: "released" }));
    return;
  }
  throw new Error("lock needs hold, release or status.");
}

// ---------------------------------------------------------------------------------------
// Running Slopcamera

let versionCache: string | undefined;
function slopcameraVersion(): string {
  if (versionCache !== undefined) return versionCache;
  const result = Bun.spawnSync([SLOPCAMERA, "--version"]);
  const version = result.stdout.toString().trim();
  if (result.exitCode !== 0 || version === "") throw new Error(`Cannot run ${SLOPCAMERA} --version.`);
  if (version !== EXPECTED_SLOPCAMERA) throw new Error(`Expected Slopcamera ${EXPECTED_SLOPCAMERA}, found ${version}.`);
  versionCache = version;
  return version;
}

async function slopcamera(requestPath: string, dryRun: boolean, logPath: string): Promise<Record<string, unknown>> {
  const args = [SLOPCAMERA, "html", "render", "--input", relative(MEDIA, requestPath), "--json"];
  if (dryRun) args.push("--dry-run");
  const logFile = await open(logPath, "w", 0o600);
  let code: number;
  let stdout: string;
  try {
    const child = Bun.spawn(args, {
      cwd: MEDIA,
      env: { ...process.env, SLOPCAMERA_REPOSITORY_ROOT: MEDIA },
      stdout: "pipe",
      stderr: logFile.fd,
      timeout: RENDER_TIMEOUT_MS,
    });
    [code, stdout] = await Promise.all([child.exited, new Response(child.stdout).text()]);
  } finally {
    await logFile.close();
  }
  if (code !== 0) throw new Error(`slopcamera ${dryRun ? "dry run" : "render"} exited ${code}; see ${relative(REPO, logPath)}. stdout: ${stdout.slice(0, 2000)}`);
  return JSON.parse(stdout) as Record<string, unknown>;
}

/** Find the rendered MP4 in the CLI's JSON result. */
function videoPath(result: Record<string, unknown>): string {
  const found: string[] = [];
  const walk = (value: unknown) => {
    if (typeof value === "string" && value.endsWith(".mp4")) found.push(value);
    else if (Array.isArray(value)) value.forEach(walk);
    else if (value !== null && typeof value === "object") Object.values(value).forEach(walk);
  };
  walk(result);
  const video = found.find((p) => p.endsWith("/video.mp4")) ?? found[0];
  if (video === undefined) throw new Error("The render result names no MP4.");
  return video.startsWith("/") ? video : join(MEDIA, video);
}

/** The per-job scene directory the CLI writes, removed once the output is collected. */
function jobDirectory(video: string): string | undefined {
  const dir = dirname(video);
  const generated = join(MEDIA, "artifacts/slopcamera/generated/html-scenes");
  return dir.startsWith(generated + "/") ? dir : undefined;
}

const STILL_MIN_FREE_GIB = 2;

function freeGiB(path: string): number {
  const s = statfsSync(path);
  return (s.bavail * s.bsize) / GIB;
}

function ffmpeg(args: string[]): void {
  const result = Bun.spawnSync(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", ...args]);
  if (result.exitCode !== 0) throw new Error(`ffmpeg failed: ${result.stderr.toString().slice(0, 2000)}`);
}

async function inputHashes(): Promise<{ data: Record<string, string>; fonts: Record<string, string> }> {
  const data: Record<string, string> = {};
  for (const file of DATA_FILES) data[file] = await fileSha(join(DATA_DIR, file));
  const fonts: Record<string, string> = {};
  for (const r of RESOURCES) fonts[r.path] = await fileSha(join(MEDIA, r.path));
  return { data, fonts };
}

async function appendReceipt(name: string, entry: Record<string, unknown>): Promise<void> {
  await mkdir(RECEIPTS, { recursive: true });
  const path = join(RECEIPTS, `${name}.json`);
  let receipt: { kind: string; renders: unknown[] } = { kind: "gobstopper.media-render", renders: [] };
  if (existsSync(path)) receipt = JSON.parse(await readFile(path, "utf8"));
  receipt.renders.push(entry);
  await writeFile(path, JSON.stringify(receipt, null, 2) + "\n");
}

// ---------------------------------------------------------------------------------------
// Jobs

const STILL_DESTINATIONS: Record<Exclude<Output, "social">, (id: string) => string> = {
  docs: (id) => join(REPO, "docs/assets", `gob-${id}.png`),
  blog: (id) => join(REPO, "site/public/blog/introducing-gobstopper", `gob-${id}.png`),
};

/**
 * A still. Slopcamera captures frames at the canvas's CSS size whatever the device scale
 * factor, so the canvas is the output size (2400×1350, or 1200×630 for the social card)
 * and the scene's layout (`size` in stills.json) is zoomed to fill it. The PNG comes from
 * the lossless captured frame, not the H.264 video, so it has no chroma subsampling.
 */
async function renderStill(job: StillJob, dryRun: boolean, log: (s: string) => void): Promise<void> {
  const name = `still-${job.id}`;
  const [layoutWidth, layoutHeight] = job.size;
  const social = job.outputs.includes("social");
  const [width, height] = social ? [layoutWidth, layoutHeight] : [2400, 1350];
  const layoutZoom = width / layoutWidth;
  if (Math.abs(height / layoutHeight - layoutZoom) > 1e-9) throw new Error(`${name}: size ${layoutWidth}×${layoutHeight} does not scale evenly to ${width}×${height}.`);
  const { htmlPath, htmlSha } = await buildScene(name, job.scene);
  const requestPath = join(BUILD, `${name}.scene.json`);
  await writeFile(requestPath, JSON.stringify(sceneRequest({
    job: name, htmlPath, palette: job.palette, width, height, dsf: 1, layoutZoom,
    durationUs: 34_000, timeOffsetSeconds: job.hold, data: job.data ?? [],
  }), null, 2) + "\n");
  const plan = await slopcamera(requestPath, true, join(BUILD, `${name}.plan.log`));
  if (dryRun) { log(JSON.stringify({ job: name, state: "planned", plan })); return; }

  // A still peaks at tens of megabytes (one short video, one lossless frame, the browser profile).
  if (freeGiB(REPO) < STILL_MIN_FREE_GIB) throw new Error(`Less than ${STILL_MIN_FREE_GIB} GiB free; not rendering.`);
  const version = slopcameraVersion();
  const release = await acquire(process.env.GOB_RENDER_LANE ?? "l3", name, log);
  const started = Date.now();
  let result: Record<string, unknown>;
  try {
    result = await slopcamera(requestPath, false, join(BUILD, `${name}.log`));
  } finally {
    await release();
  }
  const video = videoPath(result);
  const jobDir = jobDirectory(video);
  const png = join(OUT, social ? "social" : "stills", social ? "gobstopper-terminal-bench-1200x630.png" : `gob-${job.id}.png`);
  let size: number;
  let outputSha: string;
  const copies: Record<string, string> = {};
  try {
    if (jobDir === undefined) throw new Error(`${name}: the render is outside the media artifacts directory.`);
    const frame = join(jobDir, "render/frames/frame-00000000.png");
    if (!existsSync(frame)) throw new Error(`${name}: no captured frame at ${relative(REPO, frame)}.`);
    await mkdir(dirname(png), { recursive: true });
    ffmpeg(["-i", frame, "-frames:v", "1", "-pix_fmt", "rgb24", "-pred", "mixed", png]);
    const [pw, ph] = pngSize(await readFile(png));
    if (pw !== width || ph !== height) throw new Error(`${name}: frame is ${pw}×${ph}, expected ${width}×${height}.`);
    size = (await stat(png)).size;
    if (size > MAX_STILL_BYTES) throw new Error(`${name}: ${size} bytes, over 600 KB.`);
    outputSha = await fileSha(png);
    for (const output of job.outputs) {
      if (output === "social") continue;
      const destination = STILL_DESTINATIONS[output](job.id);
      await mkdir(dirname(destination), { recursive: true });
      await copyFile(png, destination);
      const copySha = await fileSha(destination);
      if (copySha !== outputSha) throw new Error(`${destination} is not byte-identical to the render.`);
      copies[output] = relative(REPO, destination);
    }
  } finally {
    if (jobDir !== undefined) await rm(jobDir, { recursive: true, force: true });
  }

  await appendReceipt(name, {
    job: name, id: job.id, slopcamera: version, renderedAt: new Date().toISOString(),
    seconds: Math.round((Date.now() - started) / 1000),
    canvas: { width, height, deviceScaleFactor: 1, layoutWidth, layoutHeight, layoutZoom }, holdSeconds: job.hold,
    sceneHtmlSha256: htmlSha, sceneSource: job.scene, sceneSourceSha256: await fileSha(join(MEDIA, job.scene)),
    ...(await inputHashes()),
    output: { width, height, bytes: size, sha256: outputSha, source: "captured frame 0", copies },
  });
  log(JSON.stringify({ job: name, state: "rendered", seconds: Math.round((Date.now() - started) / 1000), bytes: size, png: relative(REPO, png) }));
}

/** Width and height from a PNG's IHDR chunk. */
function pngSize(bytes: Buffer): [number, number] {
  if (bytes.length < 24 || bytes.readUInt32BE(12) !== 0x49484452) throw new Error("Not a PNG.");
  return [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
}

/**
 * A film shot. Every shot but the last renders its nominal length plus the handle, and
 * the scene receives its beats, cues and timing as `parameters.data.shot`.
 */
async function renderShot(list: ShotList, job: ShotJob, draft: boolean, dryRun: boolean, log: (s: string) => void): Promise<void> {
  const name = `shot-${job.id}${draft ? "-draft" : ""}`;
  const last = list.shots[list.shots.length - 1]?.id === job.id;
  const seconds = job.end - job.start + (last ? 0 : list.handle);
  const palette = job.palette ?? "night";
  const { htmlPath, htmlSha } = await buildScene(name, job.scene);
  const requestPath = join(BUILD, `${name}.scene.json`);
  // Frames are captured at the canvas's CSS size, so a 4K final is a 3840×2160 canvas with
  // the 1920×1080 layout zoomed 2×; a draft is the layout at 1×.
  const layoutZoom = draft ? 1 : 2;
  const width = 1920 * layoutZoom;
  const height = 1080 * layoutZoom;
  const shot = { id: job.id, start: job.start, end: job.end, handle: last ? 0 : list.handle, last, beats: job.beats ?? [], cues: job.cues ?? [] };
  await writeFile(requestPath, JSON.stringify(sceneRequest({
    job: name, htmlPath, palette, width, height, dsf: 1, layoutZoom,
    durationUs: Math.round(seconds * 1_000_000), timeOffsetSeconds: 0, data: job.data ?? [],
    extra: { shot, lowerThird: list.lowerThird ?? null },
  }), null, 2) + "\n");
  const plan = await slopcamera(requestPath, true, join(BUILD, `${name}.plan.log`));
  if (dryRun) { log(JSON.stringify({ job: name, state: "planned", plan })); return; }

  const free = freeGiB(REPO);
  if (!draft && free < 20) throw new Error(`Only ${free.toFixed(1)} GiB free; 4K shots need 20 GiB.`);
  const version = slopcameraVersion();
  const release = await acquire(process.env.GOB_RENDER_LANE ?? "l4", name, log);
  const started = Date.now();
  let result: Record<string, unknown>;
  try {
    result = await slopcamera(requestPath, false, join(BUILD, `${name}.log`));
  } finally {
    await release();
  }
  const video = videoPath(result);
  const destination = join(OUT, "shots", `${job.id}${draft ? ".draft" : ""}.mp4`);
  await mkdir(dirname(destination), { recursive: true });
  await copyFile(video, destination);
  const outputSha = await fileSha(destination);
  const jobDir = jobDirectory(video);
  if (jobDir !== undefined) await rm(jobDir, { recursive: true, force: true });
  await appendReceipt(`shot-${job.id}`, {
    job: name, id: job.id, draft, slopcamera: version, renderedAt: new Date().toISOString(),
    seconds: Math.round((Date.now() - started) / 1000),
    canvas: { width, height, deviceScaleFactor: 1, layoutWidth: 1920, layoutHeight: 1080, layoutZoom }, durationSeconds: seconds,
    sceneHtmlSha256: htmlSha, sceneSource: job.scene, sceneSourceSha256: await fileSha(join(MEDIA, job.scene)),
    ...(await inputHashes()),
    output: { path: relative(REPO, destination), bytes: (await stat(destination)).size, sha256: outputSha },
  });
  log(JSON.stringify({ job: name, state: "rendered", seconds: Math.round((Date.now() - started) / 1000), mp4: relative(REPO, destination) }));
}

export async function main(argv: string[], log: (s: string) => void = console.log): Promise<void> {
  const [command, ...rest] = argv;
  const flags = new Set(rest.filter((a) => a.startsWith("--")));
  const positional = rest.filter((a) => !a.startsWith("--"));
  for (const flag of flags) if (flag !== "--dry-run" && flag !== "--draft") throw new Error(`Unknown flag ${flag}`);
  const dryRun = flags.has("--dry-run");
  const draft = flags.has("--draft");
  switch (command) {
    case "lock":
      return lockCommand(rest);
    case "still": {
      const id = positional[0];
      const job = (await loadStills()).find((entry) => entry.id === id);
      if (job === undefined) throw new Error(`No still ${id}. Choose from stills.json.`);
      return renderStill(job, dryRun, log);
    }
    case "stills":
      for (const job of await loadStills()) await renderStill(job, dryRun, log);
      return;
    case "shot": {
      const id = positional[0];
      const list = await loadShots();
      const job = list.shots.find((entry) => entry.id === id);
      if (job === undefined) throw new Error(`No shot ${id}. Choose from shots.json.`);
      return renderShot(list, job, draft, dryRun, log);
    }
    case "shots": {
      const list = await loadShots();
      for (const job of list.shots) await renderShot(list, job, draft, dryRun, log);
      return;
    }
    default:
      throw new Error("Usage: bun media/render.ts still <id> | stills | shot <id> [--draft] | shots [--draft] | lock hold|release|status  [--dry-run]");
  }
}

if (import.meta.main) await main(process.argv.slice(2));

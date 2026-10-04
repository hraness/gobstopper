import { describe, expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { readdir, readFile, stat } from "node:fs/promises";
import { join } from "node:path";
import type { ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

import { ARM_LABEL, ARM_ORDER, arm, pair, replayGrid, sawtooth, terminalBench } from "../app/_lib/gobbench-data";
import { F, GAP_ROWS, fewer, millions, pct, range, signed, thousands, usd, wilson } from "../app/_lib/gobbench-format";
import {
  ANATOMY, FUSE, RING, SLAB, TAIL, type SlabKind, areaPath, linePath, linear, resendColumns, stackHeight, stepPath, ticks,
} from "../app/_lib/gob-geometry";
import { type GobFilm as GobFilmManifest, gobFilm } from "../app/_data/gob-film";
import { GOB_FILM_SCOPE, gobFilmHtml } from "../app/_lib/gobbench-film-html";
import { GobAnatomy } from "../app/_components/gob-anatomy";
import { GobChurn } from "../app/_components/gob-churn";
import { GobFilm } from "../app/_components/gob-film";
import { GobFuse } from "../app/_components/gob-fuse";
import { GobGap } from "../app/_components/gob-gap";
import { GobGrid } from "../app/_components/gob-grid";
import { GobResend } from "../app/_components/gob-resend";
import { GobRoute } from "../app/_components/gob-route";
import { GobSawtooth } from "../app/_components/gob-sawtooth";
import { GobSolved } from "../app/_components/gob-solved";
import { GobTail } from "../app/_components/gob-tail";
import { GobTokens } from "../app/_components/gob-tokens";
import Home from "../app/page";
import Benchmarks from "../app/benchmarks/page";

const site = join(import.meta.dir, "..");
const read = async (path: string): Promise<string> => await readFile(join(site, path), "utf8");
const DATA_DIR = "public/benchmarks/2026-09-28";
const visible = (html: string): string => html.replace(/<[^>]+>/gu, " ").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");

describe("launch formatter", () => {
  test("formats every launch number from the data package (§2.3)", () => {
    expect(F).toEqual({
      solved: { tail0: "61", no_proxy: "60", tail40: "59" },
      rate: { tail0: "68.5%", no_proxy: "67.4%", tail40: "66.3%" },
      ci: { tail0: "58.3–77.2", no_proxy: "57.1–76.3", tail40: "56.0–75.3" },
      input: { tail0: "84.3M", no_proxy: "118.6M", tail40: "118.5M" },
      cache: { tail0: "68.7M", no_proxy: "102.6M", tail40: "98.2M" },
      uncached: { tail0: "15.6M", no_proxy: "15.9M", tail40: "20.3M" },
      output: { tail0: "2.64M", no_proxy: "2.71M", tail40: "3.97M" },
      cost: { tail0: "$5.72", no_proxy: "$6.82", tail40: "$7.97" },
      perTask: { tail0: "$0.064", no_proxy: "$0.077", tail40: "$0.090" },
      inputFewer: "29%",
      cacheFewer: "33%",
      costLower: "16%",
      costLowerCI: "−32% to +2%",
      oldVsNew: "+39%",
      oldVsNone: "+17%",
      newVsOld: "−28%",
      oldVsNewCI: "−46.5% to −1.6%",
      oldVsNewAbs: "39%",
      oldVsNoneAbs: "17%",
      newVsOldAbs: "28%",
      churn: { all: "43", none: "15", split: "31" },
      gapTotal: "$2.25",
      gapTopShare: "105%",
      replay: { t32: "78%", t64: "73%", t128: "61%", t256: "38%" },
      replayLargest: { from: "595K", to: "41K" },
      // 12,948,162 estimated tokens: the raw data gives 12.9M, not the spec's expected 13.0M.
      saw: { requests: "383", peak: "491K", cumFrom: "116.7M", cumTo: "12.9M", cut: "89%", compactions: "50" },
    });
  });

  test("computes Wilson intervals from resolved and n_tasks, never the stored bounds", () => {
    expect(F.ci.tail0).toBe("58.3–77.2");
    const [lo, hi] = wilson(61, 89);
    expect(lo).toBeCloseTo(0.58296, 4);
    expect(hi).toBeCloseTo(0.77249, 4);
    expect(() => wilson(90, 89)).toThrow();
  });

  test("uses a real minus sign and en dash and never signs a zero", () => {
    expect(signed(-0.161)).toBe("−16%");
    expect(signed(0.393)).toBe("+39%");
    expect(signed(0.001)).toBe("0%");
    expect(range(0.5, 0.75)).toBe("50.0–75.0");
    expect(usd(-0.1235, 2)).toBe("−$0.12");
    expect(usd(5.7204, 2)).toBe("$5.72");
    expect(pct(0.6854)).toBe("68.5%");
    expect(millions(84305420)).toBe("84.3M");
    expect(thousands(595109)).toBe("595K");
    expect(fewer(-0.2889)).toBe("29%");
    expect(JSON.stringify(F)).not.toMatch(/[—-]/u);
  });

  test("agrees with the ratios stored in the data package", () => {
    const newVsNone = pair("tail0", "no_proxy");
    const inputRatio = arm("tail0").tokens_all_trials.total_input / arm("no_proxy").tokens_all_trials.total_input - 1;
    expect(inputRatio).toBeCloseTo(newVsNone.token_ratio_minus1.total_input, 3);
    for (const id of ARM_ORDER) {
      const tokens = arm(id).tokens_all_trials;
      expect(tokens.cache_read + tokens.uncached_input).toBe(tokens.total_input);
    }
    const gap = terminalBench.cost_concentration[0]!;
    expect(gap.minuend).toBe("tail40");
    expect(gap.subtrahend).toBe("tail0");
    expect(Math.abs(GAP_ROWS.slice(0, -1).reduce((sum, row) => sum + row.deltaUsd, 0) / gap.total_gap_usd - gap.top_share_of_gap)).toBeLessThan(0.0006);
  });

  test("builds the cost-gap rows with the other tasks netting slightly negative", () => {
    expect(GAP_ROWS.map((row) => `${row.task} ${row.deltaUsd > 0 ? "+" : ""}${usd(row.deltaUsd, 2)}`)).toEqual([
      "video-processing +$1.02",
      "winning-avg-corewars +$0.52",
      "path-tracing-reverse +$0.29",
      "path-tracing +$0.29",
      "schemelike-metacircular-eval +$0.25",
      "other 84 tasks −$0.12",
    ]);
  });

  test("names the arms exactly and never as CliffCompaction", () => {
    expect(ARM_ORDER).toEqual(["tail0", "no_proxy", "tail40"]);
    expect(ARM_LABEL).toEqual({
      tail0: "Gobstopper, tail 0",
      tail40: "Gobstopper, tail 40 (old default)",
      no_proxy: "Claude Code, no proxy",
    });
    for (const id of ARM_ORDER) expect(arm(id).label).toBe(ARM_LABEL[id]);
    expect(Object.values(ARM_LABEL).join(" ")).not.toContain("CliffCompaction");
  });
});

describe("launch geometry", () => {
  test("keeps the brand ring and slab constants", () => {
    expect(RING).toEqual({ box: 26, r: [13, 8, 5] });
    expect(SLAB).toEqual({ h: 1, gap: 0.14 });
    expect(stackHeight(3)).toBeCloseTo(3.28, 10);
  });

  test("stacks the diagrams as specified", () => {
    const columns = resendColumns(5);
    expect(columns.map((column) => column.length)).toEqual([1, 2, 3, 4, 5]);
    expect(columns.every((column) => column[0]!.kind === "task" && column.slice(1).every((slab) => slab.kind === "turn"))).toBe(true);
    expect(FUSE.before.map((slab) => slab.kind)).toEqual(["task", ...Array<SlabKind>(8).fill("turn")]);
    expect(FUSE.after.map((slab) => slab.kind)).toEqual(["task", "summary", "recent", "recent", "recent"]);
    expect(stackHeight(FUSE.before.length)).toBeGreaterThan(FUSE.lidAt);
    expect(stackHeight(FUSE.after.length)).toBeLessThan(FUSE.lidAt);
    expect(TAIL.tail0.map((slab) => slab.kind)).toEqual(["task", "summary", "recent", "recent", "recent"]);
    expect(TAIL.tail40.map((slab) => slab.kind)).toEqual(["task", "summary", "kept", "kept", "recent", "recent", "recent"]);
    expect(stackHeight(TAIL.tail40.length)).toBeLessThan(TAIL.lidAt + 1);
    expect(ANATOMY.blocks.reduce((sum, block) => sum + block.w, 0)).toBeCloseTo(0.57, 10);
  });

  test("draws paths in a 1000 box with y inverted", () => {
    expect(linePath([0, 50, 100], 100)).toBe("M0,1000 L500,500 L1000,0");
    expect(areaPath([0, 100], 100)).toBe("M0,1000 L1000,0 L1000,1000 L0,1000 Z");
    expect(stepPath([50, 50, 25], 100)).toBe("M0,500 H500 H1000 V750");
    expect(linear(0, 10, 0, 1)(5)).toBe(0.5);
    expect(() => linePath([1], 1)).toThrow();
  });

  test("picks round ticks from zero", () => {
    expect(ticks(120_000_000, 4)).toEqual([0, 40_000_000, 80_000_000, 120_000_000]);
    expect(ticks(120_000_000, 3)).toEqual([0, 60_000_000, 120_000_000]);
    expect(ticks(500_000, 6)).toEqual([0, 100_000, 200_000, 300_000, 400_000, 500_000]);
    expect(ticks(500_000, 3)).toEqual([0, 250_000, 500_000]);
    expect(ticks(1, 5)).toEqual([0, 0.25, 0.5, 0.75, 1]);
  });
});

describe("launch data files", () => {
  const files: Readonly<Record<string, readonly string[]>> = {
    "terminal-bench-results.json": [
      "arms", "churn", "cost_concentration", "pairs", "paper_reference", "per_task", "proxy_activity",
      "schema_version", "source",
    ],
    "replay-grid.json": [
      "by_provider", "corpus", "largest_session_at_32k_tail0", "pairing_violations_total", "pooled",
      "schema_version", "source",
    ],
    "sawtooth-series.json": [
      "request", "runs", "schema_version", "schematic", "session_profile", "source", "without_proxy",
    ],
  };

  const FORBIDDEN_KEYS = new Set([
    "samples", "rows", "sample", "session_id", "path", "snapshot", "sha256", "manifest", "manifest_sha256",
    "missed_probes", "argv", "environment",
  ]);

  function keys(value: unknown, into: Set<string>): Set<string> {
    if (Array.isArray(value)) for (const item of value) keys(item, into);
    else if (typeof value === "object" && value !== null) {
      for (const [key, item] of Object.entries(value)) {
        into.add(key);
        keys(item, into);
      }
    }
    return into;
  }

  test("publishes exactly the three aggregate files with allowlisted top-level keys", async () => {
    expect((await readdir(join(site, DATA_DIR))).sort()).toEqual(Object.keys(files).sort());
    for (const [file, allowed] of Object.entries(files)) {
      const parsed = JSON.parse(await read(`${DATA_DIR}/${file}`)) as Record<string, unknown>;
      expect(Object.keys(parsed).sort()).toEqual([...allowed]);
      expect([...keys(parsed, new Set())].filter((key) => FORBIDDEN_KEYS.has(key))).toEqual([]);
    }
  });

  test("serves the same values the site imports", () => {
    expect(terminalBench.arms).toHaveLength(3);
    expect(sawtooth.schematic).toBe(false);
    expect(sawtooth.without_proxy).toHaveLength(sawtooth.request.length);
    expect(replayGrid.pairing_violations_total.proxy_introduced).toBe(0);
  });

  test("the film manifest, when set, names the three fixed media paths and times its captions", async () => {
    // The files themselves (bytes, hash) are checked in source.test.ts.
    if (gobFilm === null) return;
    expect(gobFilm.src).toBe("/media/gobstopper-film-1080p.mp4");
    expect(gobFilm.sha256).toMatch(/^[0-9a-f]{64}$/u);
    expect((await stat(join(site, "public", gobFilm.poster))).size).toBeLessThanOrEqual(300 * 1024);
    const vtt = await read(join("public", gobFilm.captions));
    // One beat per caption cue.
    expect(gobFilm.beats).toHaveLength(vtt.split(" --> ").length - 1);
    const stamp = (seconds: number) => {
      const ms = Math.round(seconds * 1000);
      const pad = (n: number, w = 2) => String(n).padStart(w, "0");
      return `${pad(Math.floor(ms / 3_600_000))}:${pad(Math.floor(ms / 60_000) % 60)}:${pad(Math.floor(ms / 1000) % 60)}.${pad(ms % 1000, 3)}`;
    };
    expect(vtt.startsWith("WEBVTT\n\n")).toBe(true);
    for (const beat of gobFilm.beats) {
      expect(vtt).toMatch(new RegExp(`${stamp(beat.start)} --> ${stamp(beat.end)}[^\n]*\n${beat.text.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&")}\n`, "u"));
    }
  });

  const PRIVATE: readonly RegExp[] = [
    /vck_/u, /192\.168\./u, /\b10\.0\./u, /gateway\.env/u, /\.jsonl/u, /~\/\.claude/u, /~\/\.codex/u,
    // The two private proxy ports as standalone numbers; the same digits inside token counts are fine.
    /base_url/u, /(?<![\d.])837[01](?![\d])/u, /\/Users\//u, /\/home\//u, /\/private\//u, /\/tmp\//u, /rollout-/u,
    /session-\d{4}/u, /AI_GATEWAY/u, /GATEWAY_API_KEY/u,
    /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/iu,
  ];

  test("keeps keys, hosts, ports and session names out of the data and the figures", async () => {
    const components = (await readdir(join(site, "app/_components"))).filter((name) => /^gob-.*\.tsx$/u.test(name));
    const sources = [
      ...Object.keys(files).map((file) => `${DATA_DIR}/${file}`),
      ...components.map((name) => `app/_components/${name}`),
      "app/_lib/gobbench-data.ts",
      "app/_lib/gobbench-format.ts",
      "app/_lib/gob-geometry.ts",
      "app/_data/gob-film.ts",
    ];
    for (const path of sources) {
      const text = await read(path);
      const hits = PRIVATE.filter((pattern) => pattern.test(text)).map(String);
      expect({ path, hits }).toEqual({ path, hits: [] });
    }
  });
});

describe("launch figures render", () => {
  const figures: readonly { readonly name: string; readonly element: ReactElement; readonly chart: boolean }[] = [
    { name: "D-resend", element: <GobResend />, chart: false },
    { name: "D-fuse", element: <GobFuse />, chart: false },
    { name: "D-anatomy", element: <GobAnatomy />, chart: false },
    { name: "D-route", element: <GobRoute />, chart: false },
    { name: "D-tail", element: <GobTail />, chart: false },
    { name: "C-tokens home", element: <GobTokens variant="home" />, chart: true },
    { name: "C-tokens full", element: <GobTokens variant="full" />, chart: true },
    { name: "C-solved", element: <GobSolved />, chart: true },
    { name: "C-sawtooth home", element: <GobSawtooth variant="home" />, chart: true },
    { name: "C-sawtooth full", element: <GobSawtooth variant="full" />, chart: true },
    { name: "C-grid", element: <GobGrid />, chart: true },
    { name: "C-gap", element: <GobGap />, chart: true },
    { name: "C-churn", element: <GobChurn />, chart: true },
  ];

  for (const { name, element, chart } of figures) {
    test(`${name} renders an accessible, token-coloured figure`, () => {
      const html = renderToStaticMarkup(element);
      const text = visible(html);
      expect(html).not.toMatch(/#[0-9a-f]{3,8}\b/iu);
      expect(html).not.toContain("hraness-marketing-data-table");
      expect(html).toContain('role="img"');
      const labelled = /aria-labelledby="([^"]+)"/u.exec(html)?.[1];
      expect(labelled).toBeDefined();
      const alt = new RegExp(`id="${labelled}">([^<]{40,})<`, "u").exec(html)?.[1];
      expect(alt).toBeDefined();
      expect(html.includes("<details")).toBe(chart);
      expect(text).not.toMatch(/\bbetter\b/iu);
      expect(html).toContain("<figcaption");
      for (const label of Object.values(ARM_LABEL)) expect(label).not.toContain("CliffCompaction");
      if (chart && !name.startsWith("C-sawtooth") && name !== "C-grid" && name !== "C-gap" && name !== "C-churn") {
        for (const id of ARM_ORDER) expect(text).toContain(ARM_LABEL[id]);
      }
    });
  }

  test("the tokens chart shows no dollars on the homepage and cost on the benchmarks page", () => {
    expect(renderToStaticMarkup(<GobTokens variant="home" />)).not.toContain("$");
    expect(visible(renderToStaticMarkup(<GobTokens variant="full" />))).toContain(F.perTask.tail0);
  });

  test("the film embed renders nothing while the manifest is empty", () => {
    expect(renderToStaticMarkup(<GobFilm film={null} />)).toBe("");
    expect(gobFilmHtml(null)).toBe("");
  });

  const FILM_FIXTURE: GobFilmManifest = {
    src: "/media/gobstopper-film-1080p.mp4",
    poster: "/media/gobstopper-film-poster.jpg",
    captions: "/media/gobstopper-film.en.vtt",
    width: 1920, height: 1080, durationSeconds: 75,
    bytes: 1, sha256: "0".repeat(64),
    beats: [{ start: 0, end: 4, text: "Your agent's \"history\" & <tools>" }, { start: 4, end: 8, text: "Second beat" }],
  };

  test("the film embed is one accessible video that loads nothing until played", () => {
    const html = renderToStaticMarkup(<GobFilm film={FILM_FIXTURE} />);
    expect(html.match(/<video\b/gu)).toHaveLength(1);
    const video = /<video\b[^>]*>/u.exec(html)?.[0] ?? "";
    expect(video).toContain('preload="none"');
    expect(video).toContain('poster="/media/gobstopper-film-poster.jpg"');
    expect(video).toContain("controls");
    for (const banned of ["autoplay", "autoPlay", "muted", "loop"]) expect(video).not.toContain(banned);
    expect(html).toMatch(/<track\b[^>]*kind="captions"[^>]*src="\/media\/gobstopper-film\.en\.vtt"/u);
    expect(html).toContain('aria-describedby="film-text"');
    expect(html).toContain('id="film-text"');
    expect(html.match(/<li>/gu)).toHaveLength(FILM_FIXTURE.beats.length);
  });

  test("the film's text alternative ends with its scope and the qualifiers the pictures carry", () => {
    const html = renderToStaticMarkup(<GobFilm film={FILM_FIXTURE} />);
    const details = /<details id="film-text">[\s\S]*<\/details>/u.exec(html)?.[0] ?? "";
    const scope = /<p class="gob-film__scope">([^<]*)<\/p><\/details>$/u.exec(details)?.[1] ?? "";
    for (const fact of ["89 tasks", "1 trial per arm", "GLM 5.3 Flash", "45K threshold (default 128K)", "within single-trial noise", "provider-reported", "Vercel AI Gateway"]) {
      expect(scope).toContain(fact);
    }
    // Once the film's shot list is in the repository, the scope line is its lower-third word for word.
    const shots = new URL("../../media/shots.json", import.meta.url);
    if (existsSync(shots)) expect(readFileSync(shots, "utf8")).toContain(JSON.stringify(GOB_FILM_SCOPE));
  });

  test("the blog's film string is byte for byte the React embed", () => {
    expect(gobFilmHtml(FILM_FIXTURE)).toBe(renderToStaticMarkup(<GobFilm film={FILM_FIXTURE} />));
    if (gobFilm !== null) expect(gobFilmHtml(gobFilm)).toBe(renderToStaticMarkup(<GobFilm film={gobFilm} />));
  });

  test("each figure's alt text is read once, through aria-labelledby", () => {
    for (const html of [renderToStaticMarkup(<Home />), renderToStaticMarkup(<Benchmarks />)]) {
      for (const match of html.matchAll(/aria-labelledby="([^"]+)"[^>]*role="img"/gu)) {
        const id = match[1] ?? "";
        const target = new RegExp(`<p[^>]*id="${id}"[^>]*>`, "u").exec(html)?.[0] ?? "";
        expect(target).toContain('aria-hidden="true"');
      }
    }
  });

  test("the film embed renders the listed film with captions and its text", () => {
    if (gobFilm === null) return;
    const html = renderToStaticMarkup(<GobFilm film={gobFilm} />);
    expect(html).toContain(`<source src="${gobFilm.src}" type="video/mp4"/>`);
    expect(html).toContain('aria-describedby="film-text"');
    for (const beat of gobFilm.beats) expect(html).toContain(beat.text);
  });
});

describe("launch pages", () => {
  test("never signs a number that a word already gives a direction", () => {
    // "+39% more" reads the direction twice; prose uses the unsigned F.*Abs values.
    for (const html of [renderToStaticMarkup(<Home />), renderToStaticMarkup(<Benchmarks />), renderToStaticMarkup(<GobTail />)]) {
      expect(visible(html)).not.toMatch(/[+\u2212-]\d+(?:\.\d+)?% (?:more|less|fewer|lower|higher|cheaper)/u);
    }
  });

  test("the homepage places the Terminal-Bench result before How it works, without dollars, and the film only once it exists", () => {
    const html = renderToStaticMarkup(<Home />);
    // The homepage uses the chart figure; its detailed table is behind "Show the numbers".
    expect(html).toContain('id="fig-tokens"');
    const bench = html.indexOf('id="terminal-bench"');
    const how = html.indexOf('id="how"');
    expect(how).toBeGreaterThan(-1);
    expect(bench).toBeGreaterThan(how);
    const section = html.slice(bench, html.indexOf('id="film"', bench));
    expect(section).toContain('href="/benchmarks#terminal-bench-2026-09-28"');
    expect(section).not.toContain("$");
    expect(section).toContain(`${F.inputFewer} fewer input tokens`);
    expect(html.slice(how)).toContain("gobstopper proxy run -- claude");
    if (gobFilm === null) {
      expect(html).not.toContain('id="film"');
      expect(html).not.toContain("<video");
    } else {
      expect(html.match(/<video\b/gu)).toHaveLength(1);
      expect(html).toContain(gobFilmHtml(gobFilm));
    }
  });

  test("the homepage embeds the film once when the manifest lists it (§7)", () => {
    const html = renderToStaticMarkup(<Home />);
    if (gobFilm === null) {
      expect(html).not.toContain('id="film"');
      expect(html).not.toContain("<video");
      return;
    }
    expect(html.match(/id="film"/gu)).toHaveLength(1);
    const videos = html.match(/<video\b[^>]*>/gu) ?? [];
    expect(videos).toHaveLength(1);
    const video = videos[0]!;
    for (const attribute of ['preload="none"', "controls", "playsInline", `poster="${gobFilm.poster}"`]) {
      expect(video.toLowerCase()).toContain(attribute.toLowerCase());
    }
    for (const attribute of ["autoplay", "muted", "loop"]) expect(video.toLowerCase()).not.toContain(attribute);
    const tracks = html.match(/<track\b[^>]*>/gu) ?? [];
    expect(tracks).toHaveLength(1);
    expect(tracks[0]).toContain('kind="captions"');
    expect(tracks[0]).toContain(`src="${gobFilm.captions}"`);
    expect(tracks[0]).toContain("default");
    const text = /<details id="film-text">([\s\S]*?)<\/details>/u.exec(html)?.[1] ?? "";
    expect(text.match(/<li>/gu)).toHaveLength(gobFilm.beats.length);
  });

  test("the benchmarks page leads with the Terminal-Bench run and keeps the archived anchors", () => {
    const html = renderToStaticMarkup(<Benchmarks />);
    const toc = [...html.matchAll(/href="#([a-z0-9-]+)"/gu)].map((match) => match[1]);
    expect(html).toContain('id="terminal-bench-2026-09-28"');
    expect(toc.find((id) => id !== "main")).toBe("terminal-bench-2026-09-28");
    expect(html).toContain('id="archived-recovery-2026-09-20"');
    for (const id of ["fig-solved", "fig-tokens-full", "fig-churn", "fig-gap", "fig-grid", "fig-sawtooth-full", "fig-tail"]) {
      expect(html).toContain(`id="${id}"`);
    }
    for (const file of ["terminal-bench-results.json", "replay-grid.json", "sawtooth-series.json"]) {
      expect(html).toContain(`href="/benchmarks/2026-09-28/${file}"`);
    }
  });
});

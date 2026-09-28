// Film invariants (SPEC sections 6.1-6.4). Run: bun test media/film.test.ts
import { describe, expect, test } from "bun:test";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { captions, xfadeGraph } from "./assemble";

const MEDIA = import.meta.dir;
const list = JSON.parse(readFileSync(join(MEDIA, "shots.json"), "utf8"));
const beats = list.shots.flatMap((s: { beats: unknown[] }) => s.beats) as { n: number; start: number; end: number; text: string }[];
const cues = list.shots.flatMap((s: { id: string; start: number; end: number; cues: { t: number; type: string }[] }) =>
  s.cues.map((c) => ({ ...c, shot: s }))) as { t: number; type: string; shot: { id: string; start: number; end: number } }[];

describe("shot list", () => {
  test("seven shots tile 0-75 s at 30 fps with a 0.4 s handle", () => {
    expect(list.fps).toBe(30);
    expect(list.handle).toBe(0.4);
    expect(list.shots.length).toBe(7);
    let t = 0;
    for (const s of list.shots) {
      expect(s.start).toBe(t);
      expect(s.end - s.start).toBeGreaterThanOrEqual(5);
      expect(s.end - s.start).toBeLessThanOrEqual(15);
      t = s.end;
    }
    expect(t).toBe(75);
  });

  test("sixteen beats, contiguous, each inside its shot", () => {
    expect(beats.map((b) => b.n)).toEqual(Array.from({ length: 16 }, (_, i) => i + 1));
    for (let i = 1; i < beats.length; i++) expect(beats[i]!.start).toBe(beats[i - 1]!.end);
    expect(beats[0]!.start).toBe(0);
    expect(beats.at(-1)!.end).toBe(75);
    for (const s of list.shots) for (const b of s.beats) {
      expect(b.start).toBeGreaterThanOrEqual(s.start);
      expect(b.end).toBeLessThanOrEqual(s.end);
    }
  });

  test("lower-third runs 26-70 s with the benchmark scope, and leaves the replayed session", () => {
    expect(list.lowerThird).toMatchObject({ from: 26, to: 70 });
    for (const fact of ["Terminal-Bench 2.1", "89 tasks", "1 trial per arm", "GLM 5.3 Flash", "45K threshold", "default 128K"]) {
      expect(list.lowerThird.text).toContain(fact);
    }
    const s4 = list.shots.find((s: { id: string }) => s.id === "s4-sawtooth");
    expect(list.lowerThird.hidden).toEqual([[s4.start, s4.end]]);
  });

  test("the fixed sound cues sit on their SPEC 6.4 times", () => {
    const at = (type: string) => cues.filter((c) => c.type === type).map((c) => c.t);
    expect(at("place")).toEqual([12.8]);
    expect(at("chime")).toEqual([18.0]);
    expect(at("whoosh")).toEqual([22.0]);
    expect([...at("twonote-rise"), ...at("twonote-resolve")]).toEqual([42.0, 71.8]);
    expect(at("thud")).toEqual([51.6]);
    expect(at("resolve")).toEqual([57.0]);
    expect(cues.filter((c) => c.type === "tick" && c.shot.id === "s7-close").map((c) => c.t)).toEqual([65.2, 66.6, 68.0]);
    expect(at("fadeout")).toEqual([74.0]);
    expect((cues.find((c) => c.type === "swell") as unknown as { peak: number }).peak).toBe(11.6);
  });

  test("every cue sits inside its shot and names a voice the score knows", () => {
    const score = readFileSync(join(MEDIA, "score.py"), "utf8");
    for (const c of cues) {
      expect(c.t).toBeGreaterThanOrEqual(c.shot.start);
      expect(c.t).toBeLessThan(c.shot.end);
      expect(score).toContain(`kind == "${c.type}"`);
    }
  });

  test("every cue lands on a frame", () => {
    for (const c of cues) expect(Math.abs(c.t * 30 - Math.round(c.t * 30))).toBeLessThan(0.02);
  });
});

describe("assembly", () => {
  test("xfade offsets are the cumulative nominal lengths", () => {
    const { offsets, graph } = xfadeGraph(list);
    expect(offsets).toEqual(list.shots.slice(0, -1).map((s: { end: number }) => s.end));
    expect(graph.match(/xfade=transition=fade:duration=0\.4/g)?.length).toBe(6);
  });

  test("captions carry the beat text only, one cue per beat", () => {
    const vtt = captions(list);
    expect(vtt.startsWith("WEBVTT\n\n")).toBe(true);
    expect(vtt.match(/ --> /g)?.length).toBe(16);
    expect(vtt).toContain("00:00:00.000 --> 00:00:04.000\nYour coding agent resends everything.");
    // The end card draws the command in the centre, so its caption sits at the top.
    expect(vtt).toContain("00:01:10.000 --> 00:01:15.000 line:10%\ngobstopper proxy run -- claude");
    expect(vtt).not.toContain("GLM");
  });
});

// The heading windows come from _film.js itself (its timing block), not a copy.
const filmJs = readFileSync(join(MEDIA, "scenes", "film", "_film.js"), "utf8");
const timing = filmJs.slice(filmJs.indexOf("// timing:begin"), filmJs.indexOf("// timing:end"));
const { beatWindow, XFADE } = new Function(`${timing}; return { beatWindow, XFADE };`)() as {
  XFADE: number;
  beatWindow: (shot: unknown, i: number, lastShot: boolean) => { in: { at: number; dur: number }; out: { end: number; dur: number } | null };
};

describe("headings across cuts", () => {
  // Linear opacity is an upper bound on any ease-in/ease-out between the same endpoints.
  const opacity = (w: ReturnType<typeof beatWindow>, t: number) => {
    const up = Math.min(1, Math.max(0, (t - w.in.at) / w.in.dur));
    const down = w.out === null ? 1 : Math.min(1, Math.max(0, (w.out.end - t) / w.out.dur));
    return Math.min(up, down);
  };

  test("two headings are never visible together, inside a cross-fade or between beats", () => {
    expect(XFADE).toBe(list.handle);
    const windows = list.shots.flatMap((s: { beats: unknown[]; end: number }) =>
      s.beats.map((_, i) => beatWindow(s, i, s.end >= 75)));
    for (let f = 0; f <= 75 * 30; f++) {
      const t = f / 30;
      const sum = windows.reduce((n: number, w: ReturnType<typeof beatWindow>) => n + opacity(w, t), 0);
      expect(sum).toBeLessThanOrEqual(1.05);
    }
    // At every cut the outgoing heading is gone before the dissolve and the incoming one waits
    // for its second half.
    for (const s of list.shots.slice(1)) {
      const prev = list.shots[list.shots.indexOf(s) - 1];
      expect(beatWindow(prev, prev.beats.length - 1, false).out!.end).toBeLessThanOrEqual(s.start);
      expect(beatWindow(s, 0, s.end >= 75).in.at).toBeGreaterThanOrEqual(s.start + XFADE / 2);
    }
  });
});

describe("film scenes", () => {
  const scene = (id: string) => readFileSync(join(MEDIA, "scenes", "film", `${id}.html`), "utf8");

  test("the default-cost shot names the benchmark threshold and the default one", () => {
    const s6 = scene("s6-default");
    expect(s6).toContain("45K threshold (default 128K)");
    expect(s6).toContain("total cost");
    expect(s6).not.toContain("(the new default)");
  });

  test("the sawtooth shot says it is a replay, not the benchmark", () => {
    const s4 = scene("s4-sawtooth");
    expect(s4).toContain("replayed offline");
    expect(s4).toContain("Replay, not the benchmark");
    expect(s4).toContain("build f4db57e");
    // The applied threshold is a lid: solid, never dashed.
    expect(s4).not.toContain('"stroke-dasharray": "8 7"');
  });

  test("the results shot uses the SPEC 1.1 key wording and labels its intervals", () => {
    const s5 = scene("s5-results");
    expect(s5).toContain('"cache reads (context re-sent)"');
    expect(s5).toContain('"new input (not cached)"');
    expect(s5).toContain("95% intervals");
  });

  test("task counts come from the data, not typed into labels", () => {
    for (const id of ["s1-resend", "s5-results"]) {
      const body = scene(id).slice(scene(id).indexOf("// film:end"));
      expect(body).not.toMatch(/\b89\b/);
    }
  });

  // Final shots render at layout zoom 2, where client rects come back doubled.
  test("measure layout with st.measure, never client rects", () => {
    const dir = join(import.meta.dir, "scenes", "film");
    const files = readdirSync(dir).filter((f) => f.endsWith(".html") || f.endsWith(".js"));
    expect(files.length).toBeGreaterThanOrEqual(8);
    for (const f of files) expect(readFileSync(join(dir, f), "utf8")).not.toContain("getBoundingClientRect");
  });
});

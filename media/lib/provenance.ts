// SPEC §3 provenance lines, printed identically by the site figures and the stills.
// media/stills.test.ts checks each against site/app/_components/gob-figure.tsx.
// Stills read them as `Gob.PROV`; media/stills.test.ts checks no scene inlines one.
const TB =
  "Terminal-Bench 2.1 · 89 tasks · one trial per arm · Claude Code 2.1.283 with GLM 5.3 Flash via Vercel AI Gateway · Gobstopper v0.7.2, 45,000-token threshold (default 128,000) · September 27–28, 2026";

export const PROV = Object.freeze({
  /** P-TB */
  tb: TB,
  /** P-TB-full: P-TB plus the build caveat. */
  tbFull: `${TB} · 21 of 89 tail-0 trials may have run an earlier build`,
  /** P-SAW */
  saw: "Estimated tokens (about 4 characters per token, plus 20,000 assumed), not billed · one recorded Claude Code session, 383 requests · replay, build f4db57e (the v0.7.2 request engine), calibration on",
  /** P-GRID */
  grid: "Estimates, not billed · 24 recorded sessions (12 Claude Code, 12 Codex), 665M tokens · main fdeb099 · September 26, 2026",
});

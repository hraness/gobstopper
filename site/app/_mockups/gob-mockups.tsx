"use client";

import { AgentSession, MockupRoot, TerminalFrame, WindowLights, type AgentTurn, type TerminalLine } from "@hraness/design-kit/mockups";
import { ModeShowcase, StepThrough, type ThroughStep } from "@hraness/design-kit/mockups/client";

import { publishedRelease } from "../publication";
import { commands, cli, elide } from "./fixtures";
import { meterSeries, METER_POINTS, type MeterPoint } from "./meter-data";

const n = (value: number) => value.toLocaleString("en-US");

/* ------------------------------------------------------------------ */
/* Token meter: one recorded session, with and without the proxy        */
/* ------------------------------------------------------------------ */

type MeterMode = "off" | "on";

function Meter({ mode, point }: Readonly<{ mode: MeterMode; point: MeterPoint }>) {
  const upto = point.request;
  const series = meterSeries.slice(0, upto);
  const top = meterSeries.reduce((max, row) => Math.max(max, row.without), 0);
  const current = series[series.length - 1]!;
  const sent = mode === "on" ? current.with : current.without;
  const width = 600;
  const height = 150;
  const x = (index: number) => (index / (meterSeries.length - 1)) * width;
  const y = (tokens: number) => height - (tokens / top) * height;
  const line = series.map((row, index) => `${index === 0 ? "M" : "L"}${x(index).toFixed(1)},${y(mode === "on" ? row.with : row.without).toFixed(1)}`).join("");
  const ghost = mode === "on" ? series.map((row, index) => `${index === 0 ? "M" : "L"}${x(index).toFixed(1)},${y(row.without).toFixed(1)}`).join("") : null;
  const describe = mode === "on"
    ? `Illustration of a coding session through gobstopper proxy at request ${upto} of ${meterSeries.length}: the estimated request size climbs to the threshold, drops back when older turns are summarized, and climbs again. This request is about ${n(sent)} estimated tokens.`
    : `Illustration of the same coding session without a proxy at request ${upto} of ${meterSeries.length}: every request resends the whole session, so the estimated size only grows. This request is about ${n(sent)} estimated tokens.`;
  return (
    <MockupRoot className="gob-meter" describe={describe} kind="terminal">
      <div className="hkm-window">
        <div aria-hidden="true" className="hkm-title-bar">
          <WindowLights />
          <span className="hkm-title">{mode === "on" ? "claude, through gobstopper proxy" : "claude"}</span>
          <span />
        </div>
        <div className="hkm-page gob-meter__body">
          <div className="gob-meter__readout">
            <span className="gob-meter__label">Request {upto} of {meterSeries.length}</span>
            <span className="gob-meter__value" data-mode={mode}>~{n(sent)}</span>
            <span className="gob-meter__label">estimated tokens sent with this request</span>
          </div>
          <svg aria-hidden="true" className="gob-meter__chart" preserveAspectRatio="none" viewBox={`0 0 ${width} ${height}`}>
            <line className="gob-meter__threshold" x1="0" x2={width} y1={y(current.threshold)} y2={y(current.threshold)} />
            {ghost === null ? null : <path className="gob-meter__ghost" d={ghost} />}
            <path className="gob-meter__line" d={line} data-mode={mode} />
            <circle className="gob-meter__dot" cx={x(upto - 1)} cy={y(sent)} data-mode={mode} r="5" />
          </svg>
          <div className="gob-meter__foot">
            <span>threshold after calibration: ~{n(current.threshold)}</span>
            <span>{mode === "on" ? `summarized ${current.compactions} times so far` : "no proxy: nothing is ever summarized"}</span>
          </div>
        </div>
      </div>
    </MockupRoot>
  );
}

/** One still of the meter, for a launch beat that shows a single state. */
export function GobMeter({ mode, point = "end" }: Readonly<{ mode: MeterMode; point?: MeterPoint["id"] }>) {
  return <Meter mode={mode} point={METER_POINTS.find((entry) => entry.id === point) ?? METER_POINTS[2]!} />;
}

export function GobMeterShowcase({ initialMode = "on" }: Readonly<{ initialMode?: MeterMode }>) {
  return (
    <ModeShowcase<"session", MeterMode, string>
      caption="One recorded Claude Code session replayed at the 128,000-token default. Sizes estimate four characters per token."
      height={300}
      initial={{ mode: initialMode, option: METER_POINTS[1]!.id }}
      label={() => "Illustration: request size in one coding session"}
      modeLabel="Session"
      modes={[
        { id: "off", label: "Without Gobstopper" },
        { id: "on", label: "With Gobstopper" },
      ]}
      optionLabel="Point in the session"
      options={METER_POINTS.map((point) => ({ id: point.id, label: point.label }))}
      surfaces={[{
        id: "session",
        label: "Coding session",
        render: ({ mode, option }) => <Meter mode={mode} point={METER_POINTS.find((entry) => entry.id === option) ?? METER_POINTS[1]!} />,
      }]}
    />
  );
}

/* ------------------------------------------------------------------ */
/* Elide: a saved session before and after                              */
/* ------------------------------------------------------------------ */

type ElideMode = "original" | "copy";

function toolTurn(row: (typeof elide.rows)[number], mode: ElideMode): AgentTurn {
  const label = row.tool === "Bash" ? `Run ${row.arg}` : `Read ${row.arg}`;
  if (mode === "copy" && row.after !== null) return { role: "tool", tool: label, text: row.after, status: "warn" };
  return { role: "tool", tool: label, text: `${row.first.replace(/\t/gu, " ")} … (${n(row.bytes)} bytes)`, status: "ok" };
}

function ElideSession({ mode }: Readonly<{ mode: ElideMode }>) {
  const head = elide.rows.slice(0, 4);
  const tail = elide.rows.slice(4);
  const turns: AgentTurn[] = [
    { role: "user", text: elide.prompt },
    { role: "agent", text: "I'll run the checkout tests, then read the totals and coupon code." },
    ...head.map((row) => toolTurn(row, mode)),
    { role: "agent", text: mode === "copy" ? `… ${elide.total - head.length - tail.length} more tool calls …` : `… ${elide.total - head.length - tail.length} more tool calls, each with its full output …` },
    ...tail.map((row) => toolTurn(row, mode)),
    { role: "agent", text: elide.finish },
  ];
  return (
    <AgentSession
      agent="generic-cli"
      describe={mode === "copy"
        ? `Illustration of the compacted copy of a made-up coding session: ${elide.elided} of ${elide.total} old tool outputs are replaced by a one-line stub that gives their size; recent outputs and every message stay word for word.`
        : `Illustration of the original made-up coding session: ${elide.total} tool outputs, each with its full text.`}
      height={420}
      title={mode === "copy" ? "lanternshop, compacted copy" : "lanternshop, original session"}
      turns={turns}
    />
  );
}

export function GobElideShowcase() {
  return (
    <ModeShowcase<"session", ElideMode>
      height={420}
      initial={{ mode: "copy" }}
      label={() => "Illustration: a saved session before and after elide"}
      modeLabel="Show"
      modes={[
        { id: "original", label: "Original" },
        { id: "copy", label: "Compacted copy" },
      ]}
      surfaces={[{ id: "session", label: "Saved session", render: ({ mode }) => <ElideSession mode={mode} /> }]}
    />
  );
}

/* ------------------------------------------------------------------ */
/* Vault: preview, copy, search, undo                                   */
/* ------------------------------------------------------------------ */

function run(command: string, output: readonly string[], highlight?: (line: string) => TerminalLine["tone"]): TerminalLine[] {
  return [
    { kind: "input", text: command },
    ...output.map((text): TerminalLine => ({ kind: "output", text, tone: highlight?.(text) })),
  ];
}

function VaultTerminal({ describe, lines }: Readonly<{ describe: string; lines: readonly TerminalLine[] }>) {
  return <TerminalFrame describe={describe} fade={false} height={300} lines={lines} title="~/code/lanternshop" />;
}

const vaultSteps: readonly ThroughStep[] = [
  {
    id: "find",
    label: "Find",
    render: () => <VaultTerminal describe="Illustration: gobstopper detect lists one made-up Claude Code session at 130,060 context tokens." lines={run(commands.detect, cli.detect)} />,
  },
  {
    id: "preview",
    label: "Preview",
    render: () => <VaultTerminal describe="Illustration: gobstopper plan previews an elide that would take the session from 130,060 to about 39,818 tokens." lines={run(commands.plan, cli.plan, (line) => (line.includes("->") ? "ok" : undefined))} />,
  },
  {
    id: "copy",
    label: "Copy",
    render: () => <VaultTerminal describe="Illustration: gobstopper apply writes a new compacted session, records a recovery snapshot, and prints the resume command." lines={run(commands.apply, cli.apply, (line) => (line.startsWith("recovery snapshot") || line.startsWith("resume") ? "ok" : undefined))} />,
  },
  {
    id: "search",
    label: "Search",
    render: () => <VaultTerminal describe="Illustration: gobstopper search-snapshot finds 40 records in the saved original that mention a failing test." lines={run(commands.search, cli.search, (line) => (line.includes("matched_records") ? "ok" : undefined))} />,
  },
  {
    id: "undo",
    label: "Undo",
    render: () => <VaultTerminal describe="Illustration: gobstopper undo restores the original bytes as a new session and prints its resume command." lines={run(commands.undo, cli.undo, (line) => (line.startsWith("claude --resume") ? "ok" : undefined))} />,
  },
];

export function GobVaultSteps({ initial }: Readonly<{ initial?: ThroughStep["id"] }>) {
  return (
    <StepThrough
      initial={initial}
      label="Saved-session steps"
      minWidth={560}
      steps={vaultSteps}
    />
  );
}

/* ------------------------------------------------------------------ */
/* Proxy start: static, for the post's first beat                       */
/* ------------------------------------------------------------------ */

export function GobProxyStart() {
  return (
    <TerminalFrame
      describe="Illustration: gobstopper proxy run starts a local proxy with a 128,000-token threshold and three recent turns kept, then launches the agent pointed at it."
      lines={[
        ...run(commands.proxyRun, cli.proxyRun.slice(0, 2)),
        { kind: "comment", text: "the agent runs as usual; the proxy stops when it exits" },
      ]}
      title="~/code/lanternshop"
    />
  );
}

/* ------------------------------------------------------------------ */
/* Install: static, for the status beat                                 */
/* ------------------------------------------------------------------ */

export function GobInstall() {
  return (
    <TerminalFrame
      describe="Illustration: the one-line install script for macOS and Linux, then gobstopper --version printing the current release."
      fade={false}
      lines={[
        { kind: "input", text: commands.install },
        { kind: "comment", text: "installer output not shown" },
        // `gobstopper --version` prints "gobstopper <version>"; the version is the published release.
        ...(publishedRelease === null ? [] : run(commands.version, [`gobstopper ${publishedRelease.version}`])),
      ]}
      title="~"
    />
  );
}

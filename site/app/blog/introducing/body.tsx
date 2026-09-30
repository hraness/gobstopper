import type { LaunchBeat } from "@hraness/design-kit/launch";
import { LaunchBeats } from "@hraness/design-kit/react/server";

import { GobFilm } from "../../_components/gob-film";
import { gobFilm } from "../../_data/gob-film";
import {
  GobElideShowcase,
  GobInstall,
  GobMeter,
  GobMeterShowcase,
  GobProxyStart,
  GobVaultSteps,
} from "../../_mockups/gob-mockups";
import { launchBeats } from "../../launch/beats";
import { GO_DEEPER } from "./links";

/** The code-built surfaces a beat may name as `{ kind: "mockup", id }`. */
export const BEAT_MOCKUPS = ["meter", "elide", "vault", "proxy", "install"] as const;
export type BeatMockup = (typeof BEAT_MOCKUPS)[number];

function isBeatMockup(id: string): id is BeatMockup {
  return (BEAT_MOCKUPS as readonly string[]).includes(id);
}

function BeatVisual({ beat }: Readonly<{ beat: LaunchBeat }>) {
  const visual = beat.visual;
  switch (visual.kind) {
    case "diagram":
      // Pre-rendered 2x diagrams served as-is, like the Markdown posts; next/image adds nothing here.
      // eslint-disable-next-line @next/next/no-img-element
      return <img alt={beat.alt} decoding="async" height={675} loading="lazy" src={visual.src} width={1200} />;
    case "clip":
      throw new Error(`Beat ${beat.id} names a clip; this post shows mockups and diagrams.`);
    case "mockup": {
      if (!isBeatMockup(visual.id)) throw new Error(`Beat ${beat.id} names an unknown mockup "${visual.id}".`);
      switch (visual.id) {
        case "meter":
          // The first meter beat is interactive; the vision beat shows the no-proxy still.
          return visual.state["mode"] === "off" ? <GobMeter mode="off" /> : <GobMeterShowcase initialMode="on" />;
        case "elide":
          return <GobElideShowcase />;
        case "vault":
          return <GobVaultSteps />;
        case "proxy":
          return <GobProxyStart />;
        case "install":
          return <GobInstall />;
      }
    }
  }
}

export function IntroducingBody() {
  return (
    <>
      <GobFilm film={gobFilm} />
      <LaunchBeats beats={launchBeats} detailLabel="The details" renderVisual={(beat) => <BeatVisual beat={beat} />} />
      <h2 id="go-deeper">Go deeper</h2>
      <ul>
        {GO_DEEPER.map((link) => (
          <li key={link.href}>
            <a href={link.href}>{link.label}</a>
          </li>
        ))}
      </ul>
    </>
  );
}

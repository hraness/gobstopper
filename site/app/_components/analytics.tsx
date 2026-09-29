"use client";

import { useEffect } from "react";
import { capturePostHogEvent } from "@hraness/posthog/client";
import { PostHogAnalytics } from "@hraness/posthog/react";

import {
  CTA_CLICKED_EVENT,
  FILM_PLAYED_EVENT,
  INSTALL_BLOCKS,
  INSTALL_COPIED_EVENT,
  ctaIdFor,
  gobstopperPostHogSite,
} from "../_lib/analytics";

/** The install block a copy selection starts in, or null outside the install panel. */
function installBlockFor(node: Node | null): string | null {
  const element = node instanceof Element ? node : node?.parentElement ?? null;
  const block = element?.closest("#install pre");
  if (!block) return null;
  const blocks = Array.from(document.querySelectorAll("#install pre"));
  return INSTALL_BLOCKS[blocks.indexOf(block)] ?? null;
}

/**
 * Mounts PostHog and the three launch events. Capture calls are inert until
 * the adapter initializes, which it does only in production on gobstopper.sh
 * with a public token. No text the visitor copies is sent, only which block.
 */
export function Analytics({ apiKey }: Readonly<{ apiKey: string | undefined }>) {
  useEffect(() => {
    const onCopy = () => {
      const block = installBlockFor(document.getSelection()?.anchorNode ?? null);
      if (block !== null) capturePostHogEvent(gobstopperPostHogSite, INSTALL_COPIED_EVENT, { block });
    };
    const onClick = (event: MouseEvent) => {
      if (!(event.target instanceof Element)) return;
      const link = event.target.closest<HTMLAnchorElement>("[data-hraness-marketing='hero'] a[href]");
      const cta = link ? ctaIdFor(link.getAttribute("href") ?? "") : null;
      if (cta !== null) capturePostHogEvent(gobstopperPostHogSite, CTA_CLICKED_EVENT, { cta, placement: "hero" });
    };
    let filmPlayed = false;
    const onPlay = (event: Event) => {
      if (filmPlayed || !(event.target instanceof Element) || !event.target.closest("#film-player")) return;
      filmPlayed = capturePostHogEvent(gobstopperPostHogSite, FILM_PLAYED_EVENT);
    };
    document.addEventListener("copy", onCopy);
    document.addEventListener("click", onClick);
    // Media play events do not bubble, so listen in the capture phase.
    document.addEventListener("play", onPlay, true);
    return () => {
      document.removeEventListener("copy", onCopy);
      document.removeEventListener("click", onClick);
      document.removeEventListener("play", onPlay, true);
    };
  }, []);
  return <PostHogAnalytics apiKey={apiKey} site={gobstopperPostHogSite} />;
}

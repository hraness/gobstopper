import { launchBeatAnchor } from "@hraness/design-kit/react/server";

import { gobFilm } from "../../_data/gob-film";
import { gobFilmHtml } from "../../_lib/gobbench-film-html";
import { launchBeats } from "../../launch/beats";
import { GO_DEEPER } from "./links";

function escapeHtml(text: string): string {
  return text.replace(/[&<>"']/gu, (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#x27;" })[char]!);
}

/**
 * The introduction as plain HTML for the Atom feed: the film, then each beat's
 * heading, post, and visual (a diagram image, or the illustration's
 * description, since the interactive mockups do not run in a feed reader).
 */
export function introducingHtml(): string {
  const beats = launchBeats.map((beat) => {
    const visual = beat.visual.kind === "diagram"
      ? `<figure><img src="${escapeHtml(beat.visual.src)}" alt="${escapeHtml(beat.alt)}"/></figure>`
      : `<p><em>${escapeHtml(beat.alt)}</em></p>`;
    const detail = beat.detailHref === undefined ? "" : `<p><a href="${escapeHtml(beat.detailHref)}">The details</a></p>`;
    return `<h2 id="${launchBeatAnchor(beat)}">${escapeHtml(beat.headline)}</h2><p>${escapeHtml(beat.post)}</p>${visual}${detail}`;
  });
  const deeper = `<h2 id="go-deeper">Go deeper</h2><ul>${GO_DEEPER.map((link) => `<li><a href="${escapeHtml(link.href)}">${escapeHtml(link.label)}</a></li>`).join("")}</ul>`;
  return gobFilmHtml(gobFilm) + beats.join("") + deeper;
}

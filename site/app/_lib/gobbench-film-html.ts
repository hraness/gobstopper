import type { GobFilm } from "../_data/gob-film";

/**
 * The film embed as a plain HTML string, byte for byte what `<GobFilm>` renders.
 * The blog build (`{{film}}` in scripts/blog-html.ts) uses it so both pages share one
 * accessible markup: native controls, preload="none", a captions track, a poster, no
 * autoplay, and the beats as the text alternative. Returns "" while the manifest is empty.
 */
const escape = (text: string): string =>
  text.replace(/&/gu, "&amp;").replace(/</gu, "&lt;").replace(/>/gu, "&gt;").replace(/"/gu, "&quot;").replace(/'/gu, "&#x27;");

/** The film's lower-third, word for word (media/shots.json), and what it leaves unsaid. */
export const GOB_FILM_SCOPE =
  "Terminal-Bench 2.1 · 89 tasks · 1 trial per arm · GLM 5.3 Flash · 45K threshold (default 128K) · Sept 27–28, 2026";
export const GOB_FILM_QUALIFIER =
  "Resolution is within single-trial noise. Cost is provider-reported, metered through Vercel AI Gateway, for this model, at a 45K threshold (default 128K).";

export function gobFilmHtml(film: GobFilm | null): string {
  if (film === null) return "";
  const beats = film.beats.map((beat) => `<li>${escape(beat.text)}</li>`).join("");
  return (
    `<figure class="gob-film" id="film-player">` +
    `<video aria-describedby="film-text" controls="" height="${film.height}" playsInline="" poster="${escape(film.poster)}" preload="none" width="${film.width}">` +
    `<source src="${escape(film.src)}" type="video/mp4"/>` +
    `<track default="" kind="captions" label="English" src="${escape(film.captions)}" srcLang="en"/>` +
    `</video>` +
    `<p class="gob-film__reduced">The film is also available as text below.</p>` +
    `<details id="film-text"><summary>Read the film&#x27;s text</summary><ol>${beats}</ol>` +
    `<p class="gob-film__scope">${escape(GOB_FILM_SCOPE)}. ${escape(GOB_FILM_QUALIFIER)}</p></details>` +
    `</figure>`
  );
}

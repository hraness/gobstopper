import type { GobFilm } from "../_data/gob-film";

/**
 * The film embed as a plain HTML string, byte for byte what `<GobFilm>` renders.
 * The blog build (`{{film}}` in scripts/blog-html.ts) uses it so both pages share one
 * accessible markup: native controls, preload="none", a captions track, a poster, no
 * autoplay, and the beats as the text alternative. Returns "" while the manifest is empty.
 */
const escape = (text: string): string =>
  text.replace(/&/gu, "&amp;").replace(/</gu, "&lt;").replace(/>/gu, "&gt;").replace(/"/gu, "&quot;").replace(/'/gu, "&#x27;");

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
    `<details id="film-text"><summary>Read the film&#x27;s text</summary><ol>${beats}</ol></details>` +
    `</figure>`
  );
}

import type { GobFilm as GobFilmManifest } from "../_data/gob-film";
import { GOB_FILM_QUALIFIER, GOB_FILM_SCOPE } from "../_lib/gobbench-film-html";

/**
 * The film embed: native controls, nothing loads but the poster and captions until the
 * viewer presses play, never autoplay, muted or loop. The beats, with the film's scope line, are the text alternative.
 */
export function GobFilm({ film }: { readonly film: GobFilmManifest | null }) {
  if (film === null) return null;
  return (
    <figure className="gob-film" id="film-player">
      <video aria-describedby="film-text" controls height={film.height} playsInline poster={film.poster} preload="none" width={film.width}>
        <source src={film.src} type="video/mp4" />
        <track default kind="captions" label="English" src={film.captions} srcLang="en" />
      </video>
      <p className="gob-film__reduced">The film is also available as text below.</p>
      <details id="film-text">
        <summary>Read the film&apos;s text</summary>
        <ol>{film.beats.map((beat) => <li key={`${beat.start}-${beat.text}`}>{beat.text}</li>)}</ol>
        <p className="gob-film__scope">{`${GOB_FILM_SCOPE}. ${GOB_FILM_QUALIFIER}`}</p>
      </details>
    </figure>
  );
}

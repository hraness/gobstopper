/**
 * The one template token a post may place as a block: a line holding only
 * `{{film}}`. `scripts/blog-html.ts` keeps it through the build, and
 * `postHtml` fills it at runtime from `app/_data/gob-film.ts`, so the film
 * appears without regenerating the post bodies.
 */
export const FILM_TOKEN = "{{film}}";

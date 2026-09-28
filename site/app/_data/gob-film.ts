export type GobFilmBeat = { readonly start: number; readonly end: number; readonly text: string };
export type GobFilm = {
  readonly src: "/media/gobstopper-film-1080p.mp4";
  readonly poster: "/media/gobstopper-film-poster.jpg";
  readonly captions: "/media/gobstopper-film.en.vtt";
  readonly width: 1920; readonly height: 1080; readonly durationSeconds: 75;
  readonly bytes: number; readonly sha256: string;
  readonly beats: readonly GobFilmBeat[];
};
export const gobFilm: GobFilm | null = null;

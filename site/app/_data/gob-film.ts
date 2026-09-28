export type GobFilmBeat = { readonly start: number; readonly end: number; readonly text: string };
export type GobFilm = {
  readonly src: "/media/gobstopper-film-1080p.mp4";
  readonly poster: "/media/gobstopper-film-poster.jpg";
  readonly captions: "/media/gobstopper-film.en.vtt";
  readonly width: 1920; readonly height: 1080; readonly durationSeconds: 75;
  readonly bytes: number; readonly sha256: string;
  readonly beats: readonly GobFilmBeat[];
};
export const gobFilm: GobFilm | null = {
  src: "/media/gobstopper-film-1080p.mp4",
  poster: "/media/gobstopper-film-poster.jpg",
  captions: "/media/gobstopper-film.en.vtt",
  width: 1920, height: 1080, durationSeconds: 75,
  bytes: 2716873,
  sha256: "5e9d9873f2b9867c49819df0ebd4a4184159b129c03f803fdad551483abd3b17",
  beats: [
    { start: 0.0, end: 4.0, text: "Your coding agent resends everything." },
    { start: 4.0, end: 8.0, text: "Every step. The whole session." },
    { start: 8.0, end: 12.0, text: "Long sessions pay for it again." },
    { start: 12.0, end: 16.0, text: "Gobstopper sits in between." },
    { start: 16.0, end: 21.0, text: "Keep the start. Keep the last three." },
    { start: 21.0, end: 26.0, text: "Long printouts out. Files stay on disk." },
    { start: 26.0, end: 31.0, text: "No model writes the summary." },
    { start: 31.0, end: 36.0, text: "Same prefix, so the cache keeps matching." },
    { start: 36.0, end: 41.0, text: "Tasks solved: 61 vs 60." },
    { start: 41.0, end: 46.0, text: "Input tokens sent: 29% fewer." },
    { start: 46.0, end: 51.0, text: "Almost all of it: cache reads." },
    { start: 51.0, end: 56.0, text: "Our old default cost more." },
    { start: 56.0, end: 61.0, text: "So v0.7.3 changed the default." },
    { start: 61.0, end: 65.0, text: "Your session files are never edited." },
    { start: 65.0, end: 70.0, text: "One trial. One model. Read the limits." },
    { start: 70.0, end: 75.0, text: "gobstopper proxy run -- claude" },
  ],
};

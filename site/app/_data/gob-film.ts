export type GobFilmBeat = { readonly start: number; readonly end: number; readonly text: string };
export type GobFilm = {
  readonly src: "/media/gobstopper-film-1080p.mp4";
  readonly poster: "/media/gobstopper-film-poster.jpg";
  readonly captions: "/media/gobstopper-film.en.vtt";
  readonly width: 1920; readonly height: 1080; readonly durationSeconds: number;
  readonly bytes: number; readonly sha256: string;
  readonly beats: readonly GobFilmBeat[];
};
export const gobFilm: GobFilm | null = {
  src: "/media/gobstopper-film-1080p.mp4",
  poster: "/media/gobstopper-film-poster.jpg",
  captions: "/media/gobstopper-film.en.vtt",
  width: 1920, height: 1080, durationSeconds: 34,
  bytes: 4987489,
  sha256: "344aaf43acc8ce3e4ec868c62571647de26b0930370d129e7ace157d4d6eb575",
  beats: [
    { start: 0.25, end: 4.55, text: "On every step, your coding agent resends the whole session." },
    { start: 4.85, end: 9.55, text: "That includes output that mattered once." },
    { start: 9.85, end: 13.55, text: "Gobstopper" },
    { start: 13.85, end: 18.75, text: "Gobstopper trims the old printouts before they reach the model." },
    { start: 19.05, end: 23.35, text: "In one recorded benchmark, it solved about as many tasks and sent fewer tokens." },
    { start: 23.65, end: 27.89, text: "Your session files are never edited." },
    { start: 28.24, end: 34.2, text: "Ask your agent: “Install Gobstopper from gobstopper.sh”" },
  ],
};

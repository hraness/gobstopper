# Gobstopper media

This directory renders the launch images and the 75-second film for the September 28, 2026 Terminal-Bench 2.1 results. Every frame is an HTML scene rendered by Slopcamera. The numbers come from the three public data files in `site/public/benchmarks/2026-09-28/`, formatted by the same code the website uses, so an image cannot print a number the site does not.

## Directory map

```
media/
  README.md            this file
  render.ts            the only command that calls Slopcamera
  stills.json          one entry per still image
  shots.json           the film's shot list
  lib/palette.ts       Paper (stills) and Night (film, film card, social card) colours
  lib/stage.ts         in-page helpers: fonts, colours, slabs, the threshold line, rings, text, timing
  lib/bundle.ts        the script inlined into every scene as the global `Gob`
  brand/fonts/         Nebula Sans and Geist Mono, with their licences
  brand/mark.svg       copy of site/public/marks/gobstopper.svg
  scenes/stills/       one HTML scene per still
  scenes/film/         one HTML scene per film shot
  score.py             the film's generated soundtrack
  assemble.ts          joins the shots, adds the soundtrack, and encodes the film
  out/                 build output, ignored by Git except out/receipts/
```

## Prerequisites

- Slopcamera 3.4.0 at `~/.bun/bin/slopcamera` (`slopcamera --version` prints `3.4.0`). Set `SLOPCAMERA_BIN` to use another path.
- ffmpeg 7.1.x on `PATH`.
- Bun 1.3.14.
- `uv`, for the soundtrack.

Run commands from the repository root.

## Commands

```bash
bun media/render.ts still <id>          # one still from stills.json
bun media/render.ts stills              # every still, one after another
bun media/render.ts shot <id> [--draft] # one film shot; --draft renders 1920×1080 instead of 3840×2160
bun media/render.ts shots [--draft]     # every shot, one after another
```

Add `--dry-run` to any of them to build the scene and ask Slopcamera to validate it without opening a browser.

Each job:

1. Inlines `lib/bundle.ts` into a copy of the scene at `out/build/<job>.html`.
2. Writes the Slopcamera request to `out/build/<job>.scene.json`. Its `parameters` carry the palette, the formatted number strings and the data series the scene draws.
3. Validates the request with a dry run.
4. Waits for the render lock, renders, and releases the lock.
5. Collects the output. A still becomes a 2400×1350 PNG under 600 KB, copied byte for byte to `docs/assets/gob-<id>.png` and `site/public/blog/introducing-gobstopper/gob-<id>.png` as `stills.json` lists. The social card becomes `out/social/gobstopper-terminal-bench-1200x630.png`. A shot becomes `out/shots/<id>.mp4`.
6. Appends a record to `out/receipts/<still|shot>-<id>.json`: the Slopcamera version, the SHA-256 of the scene, data files, fonts and output, and the render time.
7. Deletes Slopcamera's working directory for the job.

Slopcamera captures each frame at the canvas's CSS size, whatever `deviceScaleFactor` says. So the canvas is always the output size: 2400×1350 for a still, 3840×2160 for a final shot. The scene still lays out at 1600×900 or 1920×1080, and `lib/stage.ts` zooms that layout to fill the canvas. Text is set and rasterized at full size, not upscaled. Scenes measure elements with `s.measure(node)`, which returns layout pixels, and never with `getBoundingClientRect()`, which returns zoomed ones.

A still's PNG comes from Slopcamera's lossless captured frame, not from the H.264 video, so it keeps full colour resolution.

To rebuild everything:

```bash
bun media/render.ts stills && bun media/render.ts shots && uv run --with numpy --with scipy python3 media/score.py && bun media/assemble.ts
```

## One render at a time

Each render starts its own Chrome. Only one may run on the machine at a time, including renders from other worktrees of this repository.

`render.ts` enforces this with a lock file in the repository's shared Git directory (`git rev-parse --git-common-dir`, file `gob-render.lock`), so every worktree sees the same lock. Set `GOB_RENDER_LOCK` to use another path. A render that finds the lock taken checks again every 30 seconds and gives up after 20 minutes. It never removes a lock it did not create.

To keep the lock across a series of renders:

```bash
bun media/render.ts lock hold --lane <name>     # prints a token
GOB_RENDER_TOKEN=<token> bun media/render.ts stills
bun media/render.ts lock release --token <token>
bun media/render.ts lock status
```

Render sequentially and leave generous timeouts; the machine is shared.

## Disk space

- Check free space with `df -h /` before rendering. `render.ts` refuses a still with less than 2 GiB free and a full-size shot with less than 20 GiB free.
- `render.ts` deletes each job's `artifacts/slopcamera/generated/html-scenes/<job>` directory after it collects the output. Delete `media/artifacts/` and `media/out/build/` after a session.

## Privacy

- Scenes show no transcript text, prompts, tool output, file paths, session names or ids.
- Example conversation lines in the diagrams are invented for the illustration.
- Numbers come only from the three public data files.

## Fonts

`brand/fonts/` holds Nebula Sans (Book, Medium, Semibold) and Geist Mono (variable weight), copied from hraness/design-kit. Both are under the SIL Open Font License 1.1; the licence texts sit next to the fonts. `brand/fonts/PROVENANCE.txt` records the source commit and the SHA-256 of each file.

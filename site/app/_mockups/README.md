# Mockups

Code-built illustrations of Gobstopper's real surfaces, used on the homepage
and in the launch post (`/blog/introducing-gobstopper`). Each one is labelled
an illustration where it renders.

- `gob-mockups.tsx`: the token meter, the elide before/after toggle, the
  saved-session step-through, the proxy start and the install terminal.
- `meter-data.ts`: sampled from
  `public/benchmarks/2026-09-28/sawtooth-series.json` (run "Gobstopper,
  tail 0, 128K threshold"): one recorded Claude Code session replayed offline,
  estimated tokens, not billed tokens.
- `fixtures.ts`: real output of a release build of this repository run
  against a synthetic session. The user, project, and files are made up.

## Recapturing the CLI fixtures

```sh
mkdir -p /tmp/gobfx && cd /tmp/gobfx
python3 <repo>/site/app/_mockups/fixture-session/generate.py
cargo build --release --manifest-path <repo>/Cargo.toml --target-dir /tmp/gobfx/target
export HOME=/tmp/gobfx/home GOB=/tmp/gobfx/target/release/gobstopper
$GOB detect
$GOB plan 5f0c2a9e --strategy elide --trigger 100000
$GOB apply 5f0c2a9e --strategy elide --trigger 100000
$GOB search-snapshot <snapshot> --query "(fail)" --limit 2
$GOB undo 5f0c2a9e
```

Paste the output into `fixtures.ts` with the home directory shortened to `~`.
The snapshot hash and session ids change on every capture; the tests check
that the numbers in the fixtures agree with each other.

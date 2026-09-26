# Releases

Gobstopper releases are cut by hand as immutable GitHub Releases. Each one carries a macOS arm64 `gobstopper` binary and a `SHA256SUMS` file. The page follows [`RELEASES.md`](https://github.com/hraness/.github/blob/main/RELEASES.md) in hraness/.github, and [`scripts/release_notes.py`](../scripts/release_notes.py) builds it.

## Check a download

Download the binary and its checksums, then check both the checksum and GitHub's signed release attestation. Replace `v0.0.0` with the tag you are installing:

```sh
gh release download v0.0.0 --repo hraness/gobstopper --pattern gobstopper --pattern SHA256SUMS
shasum -a 256 -c SHA256SUMS
gh release verify v0.0.0 --repo hraness/gobstopper
gh release verify-asset v0.0.0 gobstopper --repo hraness/gobstopper
```

`gh release verify` confirms that the release, its tag and its files have not changed since it was published. `gh release verify-asset` confirms that the file you downloaded is the one attached to that release. The release page lists the full source commit; `cargo install --git https://github.com/hraness/gobstopper --tag v0.0.0 --locked gobstopper` builds the same source on any platform.

## Cut a release

1. In the version bump pull request, set `version` under `[workspace.package]` in `Cargo.toml` and turn the `## Unreleased` section of `CHANGELOG.md` into `## vX.Y.Z - YYYY-MM-DD`. The section needs a summary paragraph and at least one bullet. Start a new empty `## Unreleased` section only when there is something to put in it.
2. After it merges, tag the merge commit and push the tag: `git tag -a vX.Y.Z -m vX.Y.Z <commit> && git push origin vX.Y.Z`.
3. In a clean checkout of the tag on macOS arm64, build and hash the binary:

   ```sh
   cargo build --release --locked
   cp target/release/gobstopper gobstopper
   shasum -a 256 gobstopper > SHA256SUMS
   ```

4. Render the page. The script fails when the changelog section is missing, empty, or still says Unreleased, or when the tag does not match `Cargo.toml`:

   ```sh
   python3 scripts/release_notes.py render --tag vX.Y.Z --commit "$(git rev-parse vX.Y.Z^{commit})" \
     --sha256sums SHA256SUMS > notes.md
   ```

5. Publish with the rendered notes, never GitHub's generated notes:

   ```sh
   gh release create vX.Y.Z --verify-tag --title "$(python3 scripts/release_notes.py title --tag vX.Y.Z)" \
     --notes-file notes.md gobstopper SHA256SUMS
   ```

6. Check the published page against the release record. The check fails when the notes were edited by hand or the identity record does not match the tag, commit and checksums:

   ```sh
   gh release view vX.Y.Z --repo hraness/gobstopper --json body > published.json
   python3 scripts/release_notes.py check --tag vX.Y.Z --commit "$(git rev-parse vX.Y.Z^{commit})" \
     --sha256sums SHA256SUMS --body-json published.json
   ```

## The page

The title is `Gobstopper vX.Y.Z`. The body is the changelog summary, `## Changes` with the changelog bullets, then `## Install` and `## Verify`, which the script generates from the tag, source commit and `SHA256SUMS`. The last bytes of the body are an HTML comment that the rendered page hides:

```text
<!-- gobstopper-release {"assets":{"gobstopper":"<sha256>"},"commit":"<40-hex>","schema":1,"tag":"vX.Y.Z"} -->
```

`check` reads this record from the last `<!-- gobstopper-release ` marker, requires the body to end with `-->`, and compares everything above it byte for byte with a fresh render. To correct a published page, change the changelog section and the page in the same pull request, then re-render and run `gh release edit vX.Y.Z --notes-file notes.md`.

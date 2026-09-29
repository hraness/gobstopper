# Releases

Gobstopper releases are immutable GitHub Releases that the [release workflow](../.github/workflows/release.yml) builds and publishes when a `vX.Y.Z` tag is pushed. Each release carries one archive per platform, a `.sha256` file beside each archive, and a `SHA256SUMS` file:

| Platform | Archive | Built on |
| --- | --- | --- |
| macOS (Apple silicon) | `gobstopper-X.Y.Z-darwin-aarch64.tar.gz` | `macos-15` |
| Linux x86_64 | `gobstopper-X.Y.Z-linux-x86_64.tar.gz` | `ubuntu-22.04` (glibc 2.35) |
| Linux arm64 | `gobstopper-X.Y.Z-linux-aarch64.tar.gz` | `ubuntu-22.04-arm` (glibc 2.35) |
| Windows x86_64 | `gobstopper-X.Y.Z-windows-x86_64.zip` | `windows-2025` |

Each archive holds exactly one file, `gobstopper` or `gobstopper.exe`. The workflow signs a build provenance attestation for each archive with [`actions/attest-build-provenance`](https://github.com/actions/attest-build-provenance). The Windows binary is not Authenticode-signed.

The page follows [`RELEASES.md`](https://github.com/hraness/.github/blob/main/RELEASES.md) in hraness/.github, and [`scripts/release_notes.py`](../scripts/release_notes.py) builds it.

## Install

The site serves the installers from [`scripts/install.sh`](../scripts/install.sh) and [`scripts/install.ps1`](../scripts/install.ps1) at the deployed commit:

```sh
curl -fsSL https://gobstopper.sh/install.sh | sh                      # macOS and Linux
```

```powershell
irm https://gobstopper.sh/install.ps1 | iex                           # Windows, in PowerShell
```

Both install the latest release unless `GOBSTOPPER_VERSION` names one, download the archive for the platform, refuse it unless its SHA-256 matches the release's `.sha256` file and it holds only the binary, check that the binary reports the requested version, and install it for the current user only:

- `install.sh` writes `~/.local/bin/gobstopper`. `GOBSTOPPER_INSTALL_PREFIX` changes the prefix. It never edits shell profiles; it prints the line to add when the directory is not on `PATH`.
- `install.ps1` writes `%LOCALAPPDATA%\Programs\gobstopper\bin\gobstopper.exe` without administrator rights and adds that directory to the user `PATH` unless `GOBSTOPPER_ADD_PATH=no`. `GOBSTOPPER_INSTALL_PREFIX` changes the prefix.

Tests point both at a loopback server with `GOBSTOPPER_RELEASE_BASE_URL=http://127.0.0.1:<port>`; they refuse any other override.

### Windows

The Windows build runs the read-only commands (`detect`, `plan`, `report`, `explain`, `verify`, `mcp`) and the proxy. Claude Code session usage reads as unknown there, because the usage scan binds to Unix inode and ctime identity. Commands that write through the vault refuse with an error naming the missing platform guarantee (directory sync, directory locking, bounded event log I/O, bounded plugin process custody), and `proxy install` refuses because it installs a macOS LaunchAgent. CI runs the Windows test suite with the tests for those Unix-only features excluded; see `[profile.windows]` in [`.config/nextest.toml`](../.config/nextest.toml).

## Check a download

Download an archive and its checksum, then check the checksum, the build provenance attestation and GitHub's release attestation. Replace `0.0.0` with the version you are installing:

```sh
gh release download v0.0.0 --repo hraness/gobstopper --pattern 'gobstopper-0.0.0-linux-x86_64.tar.gz*'
shasum -a 256 -c gobstopper-0.0.0-linux-x86_64.tar.gz.sha256
gh attestation verify gobstopper-0.0.0-linux-x86_64.tar.gz --repo hraness/gobstopper
gh release verify v0.0.0 --repo hraness/gobstopper
gh release verify-asset v0.0.0 gobstopper-0.0.0-linux-x86_64.tar.gz --repo hraness/gobstopper
```

`gh attestation verify` confirms that the archive was built by this repository's release workflow from the tagged commit. `gh release verify` confirms that the release, its tag and its files have not changed since it was published. `gh release verify-asset` confirms that the file you downloaded is the one attached to that release. `cargo install --git https://github.com/hraness/gobstopper --tag v0.0.0 --locked gobstopper` builds the same source on any platform.

## Cut a release

1. In the version bump pull request, set `version` under `[workspace.package]` in `Cargo.toml` and turn the `## Unreleased` section of `CHANGELOG.md` into `## vX.Y.Z - YYYY-MM-DD`. The section needs a summary paragraph and at least one bullet. Start a new empty `## Unreleased` section only when there is something to put in it.
2. After it merges, tag the merge commit and push the tag: `git tag -a vX.Y.Z -m vX.Y.Z <commit> && git push origin vX.Y.Z`.
3. The release workflow checks the tag against `Cargo.toml` and `CHANGELOG.md`, builds and packages each platform with [`scripts/build-release.sh`](../scripts/build-release.sh) or [`scripts/build-release.ps1`](../scripts/build-release.ps1), installs each archive with the hosted installer from a loopback server, attests the archives, writes `SHA256SUMS`, renders the page with `scripts/release_notes.py`, publishes the release, and checks the published page against the release record. Nothing needs a person after the tag push.
4. Check the assets from any machine:

   ```sh
   gh release download vX.Y.Z --repo hraness/gobstopper --dir release-check
   (cd release-check && shasum -a 256 -c SHA256SUMS)
   for archive in release-check/gobstopper-*; do case "$archive" in *.sha256) ;; *) gh attestation verify "$archive" --repo hraness/gobstopper ;; esac; done
   ```

5. Update `site/published-release.json` to the new version in a follow-up pull request.

To render or check a page by hand, pass the release's `SHA256SUMS`:

```sh
python3 scripts/release_notes.py render --tag vX.Y.Z --commit "$(git rev-parse vX.Y.Z^{commit})" --sha256sums SHA256SUMS > notes.md
gh release view vX.Y.Z --repo hraness/gobstopper --json body > published.json
python3 scripts/release_notes.py check --tag vX.Y.Z --commit "$(git rev-parse vX.Y.Z^{commit})" \
  --sha256sums SHA256SUMS --body-json published.json
```

## The page

The title is `Gobstopper vX.Y.Z`. The body is the changelog summary, `## Changes` with the changelog bullets, then `## Install` and `## Verify`, which the script generates from the tag, source commit and `SHA256SUMS`. The last bytes of the body are an HTML comment that the rendered page hides:

```text
<!-- gobstopper-release {"assets":{"gobstopper-X.Y.Z-darwin-aarch64.tar.gz":"<sha256>",...},"commit":"<40-hex>","schema":1,"tag":"vX.Y.Z"} -->
```

`check` reads this record from the last `<!-- gobstopper-release ` marker, requires the body to end with `-->`, and compares everything above it byte for byte with a fresh render. To correct a published page, change the changelog section and the page in the same pull request, then re-render and run `gh release edit vX.Y.Z --notes-file notes.md`.

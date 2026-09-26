#!/usr/bin/env python3
"""Render and check a Gobstopper GitHub Release page.

The page follows RELEASES.md in hraness/.github: a summary and `## Changes`
copied from the version's CHANGELOG.md section, `## Install` and `## Verify`
generated from the release record (tag, source commit, SHA256SUMS), and a
machine identity record as a trailing HTML comment that forms the final bytes.

    python3 scripts/release_notes.py render --tag v0.4.2 --commit SHA \
        --sha256sums SHA256SUMS > notes.md
    python3 scripts/release_notes.py check --tag v0.4.2 --commit SHA \
        --sha256sums SHA256SUMS --body-json published.json

Reads local files only and writes the page to stdout.
"""

import argparse
import json
from pathlib import Path
import re
import sys


PRODUCT = "Gobstopper"
REPOSITORY = "hraness/gobstopper"
BINARY = "gobstopper"
PLATFORM = "macOS arm64"
GUIDE = "docs/release.md"
IDENTITY_MARKER = "<!-- gobstopper-release "
IDENTITY_SCHEMA = 1

ROOT = Path(__file__).resolve().parents[1]
TAG = re.compile(r"v(\d+\.\d+\.\d+)")
COMMIT = re.compile(r"[0-9a-f]{40}")
HEADING = re.compile(r"## v?(\d+\.\d+\.\d+)(?: - (\d{4}-\d{2}-\d{2}))?")


class ReleaseNotesError(ValueError):
    pass


def version_of(tag):
    match = TAG.fullmatch(tag)
    if not match:
        raise ReleaseNotesError(f"tag {tag!r} is not vX.Y.Z")
    return match.group(1)


def title(tag):
    version_of(tag)
    return f"{PRODUCT} {tag}"


def changelog_section(text, tag):
    """Return (summary, bullets) from the tag's CHANGELOG.md section."""
    version = version_of(tag)
    lines = text.replace("\r\n", "\n").split("\n")
    start = None
    for index, line in enumerate(lines):
        if not line.startswith("## "):
            continue
        stripped = line.rstrip()
        if re.fullmatch(rf"## v?{re.escape(version)}\b.*", stripped) and "unreleased" in stripped.lower():
            raise ReleaseNotesError(f"CHANGELOG.md section for {tag} still says Unreleased")
        match = HEADING.fullmatch(stripped)
        if match and match.group(1) == version:
            if start is not None:
                raise ReleaseNotesError(f"CHANGELOG.md has two sections for {tag}")
            start = index + 1
    if start is None:
        raise ReleaseNotesError(f"CHANGELOG.md has no section for {tag}")
    end = next((i for i in range(start, len(lines)) if lines[i].startswith("## ")), len(lines))
    body = "\n".join(lines[start:end]).strip()
    if not body:
        raise ReleaseNotesError(f"CHANGELOG.md section for {tag} is empty")
    if re.search(r"\bunreleased\b", body, re.I):
        raise ReleaseNotesError(f"CHANGELOG.md section for {tag} still says Unreleased")
    first_bullet = re.search(r"^- ", body, re.M)
    if first_bullet is None:
        raise ReleaseNotesError(f"CHANGELOG.md section for {tag} has no change bullets")
    summary = body[: first_bullet.start()].strip()
    bullets = body[first_bullet.start():].strip()
    if not summary:
        raise ReleaseNotesError(f"CHANGELOG.md section for {tag} has no summary paragraph")
    for line in bullets.split("\n"):
        if line and not (line.startswith("- ") or line.startswith("  ")):
            raise ReleaseNotesError(f"CHANGELOG.md section for {tag} has text after its bullets: {line!r}")
    return summary, bullets


def parse_sha256sums(text):
    digests = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        match = re.fullmatch(r"([0-9a-f]{64}) [ *](\S+)", line)
        if not match:
            raise ReleaseNotesError(f"SHA256SUMS line is not '<sha256>  <file>': {line!r}")
        if match.group(2) in digests:
            raise ReleaseNotesError(f"SHA256SUMS lists {match.group(2)} twice")
        digests[match.group(2)] = match.group(1)
    if BINARY not in digests:
        raise ReleaseNotesError(f"SHA256SUMS does not list {BINARY}")
    return digests


def identity(tag, commit, digests):
    version_of(tag)
    if not COMMIT.fullmatch(commit):
        raise ReleaseNotesError(f"commit {commit!r} is not a full 40-character SHA")
    record = {"assets": dict(sorted(digests.items())), "commit": commit,
              "schema": IDENTITY_SCHEMA, "tag": tag}
    return IDENTITY_MARKER + json.dumps(record, sort_keys=True, separators=(",", ":")) + " -->"


def render_notes(changelog, tag, commit, digests, guide_ref=None):
    """The visible page: everything above the identity record."""
    summary, bullets = changelog_section(changelog, tag)
    if not COMMIT.fullmatch(commit):
        raise ReleaseNotesError(f"commit {commit!r} is not a full 40-character SHA")
    guide_ref = guide_ref or tag
    base = f"https://github.com/{REPOSITORY}"
    checksums = "\n".join(f"- `{name}`: `{digest}`" for name, digest in sorted(digests.items()))
    return f"""{summary}

## Changes

{bullets}

## Install

The attached `{BINARY}` binary is built for {PLATFORM}:

```sh
gh release download {tag} --repo {REPOSITORY} --pattern {BINARY} --pattern SHA256SUMS
shasum -a 256 -c SHA256SUMS
chmod +x {BINARY}
```

On other platforms, build this version from source with Cargo:

```sh
cargo install --git {base} --tag {tag} --locked {BINARY}
```

## Verify

`SHA256SUMS` on this release lists the SHA-256 of each attached file:

{checksums}

Source commit: [`{commit}`]({base}/commit/{commit})

GitHub signs an attestation for this release. Check it and a downloaded binary with:

```sh
gh release verify {tag} --repo {REPOSITORY}
gh release verify-asset {tag} {BINARY} --repo {REPOSITORY}
```

[How to check a {PRODUCT} release]({base}/blob/{guide_ref}/{GUIDE}#check-a-download)

"""


def render_body(changelog, tag, commit, digests, guide_ref=None):
    return render_notes(changelog, tag, commit, digests, guide_ref) + identity(tag, commit, digests)


def split_body(body):
    """Split a published body into (notes, identity record dict)."""
    if not body.endswith("-->"):
        raise ReleaseNotesError("release body does not end with the identity record")
    start = body.rfind(IDENTITY_MARKER)
    if start < 0:
        raise ReleaseNotesError("release body has no identity record")
    payload = body[start + len(IDENTITY_MARKER): -len(" -->")]
    if not body.endswith(" -->") or "-->" in payload:
        raise ReleaseNotesError("identity record is malformed")
    try:
        record = json.loads(payload)
    except json.JSONDecodeError as error:
        raise ReleaseNotesError(f"identity record is not JSON: {error}") from None
    if not isinstance(record, dict) or record.get("schema") != IDENTITY_SCHEMA:
        raise ReleaseNotesError("identity record has an unknown schema")
    return body[:start], record


def check_body(body, changelog, tag, commit, digests, guide_ref=None):
    """Raise unless the body is exactly the rendered page for this release."""
    notes, record = split_body(body)
    expected = identity(tag, commit, digests)
    if body[len(notes):] != expected:
        raise ReleaseNotesError(f"identity record differs from the release record: {json.dumps(record, sort_keys=True)}")
    if notes != render_notes(changelog, tag, commit, digests, guide_ref):
        raise ReleaseNotesError("release notes differ from the rendered CHANGELOG.md section and generated sections")
    return record


def cargo_version(root):
    text = (root / "Cargo.toml").read_text()
    match = re.search(r'^\[workspace\.package\][^\[]*?^version = "([^"]+)"', text, re.M | re.S)
    if not match:
        raise ReleaseNotesError("Cargo.toml has no [workspace.package] version")
    return match.group(1)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("action", choices=["render", "check", "title"])
    parser.add_argument("--tag", required=True)
    parser.add_argument("--commit")
    parser.add_argument("--sha256sums", type=Path)
    parser.add_argument("--changelog", type=Path, default=ROOT / "CHANGELOG.md")
    parser.add_argument("--root", type=Path, default=ROOT, help="tree whose Cargo.toml version must match the tag")
    parser.add_argument("--guide-ref", help=f"git ref for the {GUIDE} link (default: the tag)")
    parser.add_argument("--body", type=Path, help="published release body, exact bytes (check)")
    parser.add_argument("--body-json", type=Path, help="`gh release view TAG --json body` output (check)")
    args = parser.parse_args(argv)
    try:
        if args.action == "title":
            print(title(args.tag))
            return 0
        if not args.commit or not args.sha256sums:
            parser.error("render and check need --commit and --sha256sums")
        version = cargo_version(args.root)
        if version != version_of(args.tag):
            raise ReleaseNotesError(f"tag {args.tag} does not match Cargo.toml version {version}")
        changelog = args.changelog.read_text()
        digests = parse_sha256sums(args.sha256sums.read_text())
        if args.action == "render":
            sys.stdout.write(render_body(changelog, args.tag, args.commit, digests, args.guide_ref))
            return 0
        if bool(args.body) == bool(args.body_json):
            parser.error("check needs one of --body or --body-json")
        if args.body:
            body = args.body.read_bytes().decode()
        else:
            body = json.loads(args.body_json.read_bytes().decode()).get("body")
            if not isinstance(body, str):
                raise ReleaseNotesError("--body-json has no body string")
        check_body(body, changelog, args.tag, args.commit, digests, args.guide_ref)
        print(f"{title(args.tag)}: release body matches")
        return 0
    except ReleaseNotesError as error:
        print(f"release_notes: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())

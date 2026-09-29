"""Release page rendering and identity checks, offline."""

import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest

import release_notes as rn


ROOT = Path(__file__).resolve().parents[1]
COMMIT = "faf03fe4f4e895ea1aae7cbeb8d8c2725fd7e959"
DIGESTS = {
    "gobstopper-1.2.3-darwin-aarch64.tar.gz": "6c7f2f9de853991012b3ffcd49ae086ea6c4a7d5b73d69af8f706b3169e7435c",
    "gobstopper-1.2.3-linux-x86_64.tar.gz": "1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708",
    "gobstopper-1.2.3-linux-aarch64.tar.gz": "2e3f4a5b6c7d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f6071",
    "gobstopper-1.2.3-windows-x86_64.zip": "3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f80",
}
SUMS = "".join(f"{digest}  {name}\n" for name, digest in DIGESTS.items())
CHANGELOG = """# Changelog

## Unreleased

Next thing.

- Something not shipped yet.

## v1.2.3 - 2026-09-25

`watch` skips idle sessions.

- `watch --max-age SECS` sets the window.
- `report --all` reads every session,
  whatever its age.

## 1.2.2

Older release.

- Older change.
"""


class SectionTests(unittest.TestCase):
    def test_reads_the_tagged_section_only(self):
        summary, bullets = rn.changelog_section(CHANGELOG, "v1.2.3")
        self.assertEqual(summary, "`watch` skips idle sessions.")
        self.assertEqual(bullets, "- `watch --max-age SECS` sets the window.\n"
                                  "- `report --all` reads every session,\n  whatever its age.")
        self.assertEqual(rn.changelog_section(CHANGELOG, "v1.2.2"), ("Older release.", "- Older change."))

    def test_missing_section_fails(self):
        with self.assertRaisesRegex(rn.ReleaseNotesError, "no section for v9.9.9"):
            rn.changelog_section(CHANGELOG, "v9.9.9")

    def test_empty_section_fails(self):
        with self.assertRaisesRegex(rn.ReleaseNotesError, "is empty"):
            rn.changelog_section("## v1.0.0\n\n\n## v0.9.0\n\nx\n\n- y\n", "v1.0.0")

    def test_unreleased_section_fails(self):
        for text in ("## v1.0.0 - Unreleased\n\nx\n\n- y\n",
                     "## v1.0.0 (unreleased)\n\nx\n\n- y\n",
                     "## v1.0.0\n\nUnreleased.\n\n- y\n"):
            with self.subTest(text=text), self.assertRaisesRegex(rn.ReleaseNotesError, "Unreleased"):
                rn.changelog_section(text, "v1.0.0")

    def test_unreleased_heading_is_not_a_version(self):
        with self.assertRaisesRegex(rn.ReleaseNotesError, "no section"):
            rn.changelog_section("## Unreleased\n\nx\n\n- y\n", "v1.0.0")

    def test_summary_and_bullets_required(self):
        with self.assertRaisesRegex(rn.ReleaseNotesError, "no change bullets"):
            rn.changelog_section("## v1.0.0\n\nOnly prose.\n", "v1.0.0")
        with self.assertRaisesRegex(rn.ReleaseNotesError, "no summary"):
            rn.changelog_section("## v1.0.0\n\n- Only a bullet.\n", "v1.0.0")
        with self.assertRaisesRegex(rn.ReleaseNotesError, "text after its bullets"):
            rn.changelog_section("## v1.0.0\n\nx\n\n- y\n\nTrailing prose.\n", "v1.0.0")

    def test_duplicate_section_fails(self):
        with self.assertRaisesRegex(rn.ReleaseNotesError, "two sections"):
            rn.changelog_section("## v1.0.0\n\nx\n\n- y\n\n## 1.0.0\n\nx\n\n- y\n", "v1.0.0")

    def test_repository_changelog_has_the_current_version(self):
        version = rn.cargo_version(ROOT)
        summary, bullets = rn.changelog_section((ROOT / "CHANGELOG.md").read_text(), f"v{version}")
        self.assertTrue(summary and bullets.startswith("- "))


class BodyTests(unittest.TestCase):
    def body(self, **overrides):
        args = dict(changelog=CHANGELOG, tag="v1.2.3", commit=COMMIT, digests=DIGESTS)
        args.update(overrides)
        return rn.render_body(**args)

    def test_title(self):
        self.assertEqual(rn.title("v1.2.3"), "Gobstopper v1.2.3")

    def test_rendered_body_shape(self):
        body = self.body()
        headings = [line for line in body.splitlines() if line.startswith("## ")]
        self.assertEqual(headings, ["## Changes", "## Install", "## Verify"])
        self.assertTrue(body.startswith("`watch` skips idle sessions.\n\n## Changes\n\n- `watch --max-age"))
        self.assertIn("curl -fsSL https://gobstopper.sh/install.sh | GOBSTOPPER_VERSION=1.2.3 sh", body)
        self.assertIn('$env:GOBSTOPPER_VERSION = "1.2.3"; irm https://gobstopper.sh/install.ps1 | iex', body)
        self.assertIn("gh attestation verify gobstopper-1.2.3-linux-x86_64.tar.gz --repo hraness/gobstopper", body)
        self.assertIn("cargo install --git https://github.com/hraness/gobstopper --tag v1.2.3 --locked gobstopper", body)
        for name, digest in DIGESTS.items():
            self.assertIn(f"`{name}`: `{digest}`", body)
        self.assertIn(f"https://github.com/hraness/gobstopper/commit/{COMMIT}", body)
        self.assertIn("/blob/v1.2.3/docs/release.md#check-a-download", body)
        self.assertNotIn("latest", body.lower())
        self.assertNotIn("Unreleased", body)
        self.assertNotIn("1.2.2", body)
        for banned in ("What's Changed", "Full Changelog", "Generated with", "Automated release",
                       "Canonical GitHub release for"):
            self.assertNotIn(banned, body)
        self.assertTrue(body.endswith(" -->"))
        self.assertEqual(body.count(rn.IDENTITY_MARKER), 1)
        # The identity record is the only place the record appears; nothing visible copies it.
        self.assertNotIn('"schema"', body[: body.rfind(rn.IDENTITY_MARKER)])

    def test_guide_ref_override(self):
        self.assertIn(f"/blob/{COMMIT}/docs/release.md", self.body(guide_ref=COMMIT))

    def test_identity_parses_and_checks(self):
        body = self.body()
        notes, record = rn.split_body(body)
        self.assertEqual(record, {"assets": DIGESTS, "commit": COMMIT, "schema": 1, "tag": "v1.2.3"})
        self.assertEqual(notes + rn.identity("v1.2.3", COMMIT, DIGESTS), body)
        self.assertEqual(rn.check_body(body, CHANGELOG, "v1.2.3", COMMIT, DIGESTS), record)

    def test_identity_parsed_from_last_marker(self):
        quoted = "Quoted `<!-- gobstopper-release {} -->` in prose.\n\n- y\n"
        changelog = "## v1.2.3\n\n" + quoted
        body = rn.render_body(changelog, "v1.2.3", COMMIT, DIGESTS)
        _, record = rn.split_body(body)
        self.assertEqual(record["tag"], "v1.2.3")
        rn.check_body(body, changelog, "v1.2.3", COMMIT, DIGESTS)

    def test_tampered_notes_detected(self):
        body = self.body()
        for tampered in (body.replace("sets the window", "sets the windows"),
                         "Hand-added line.\n" + body,
                         body.replace("\n", "\r\n", 1),
                         body.replace("## Install", "## What's Changed\n\n## Install")):
            with self.subTest(), self.assertRaisesRegex(rn.ReleaseNotesError, "release notes differ"):
                rn.check_body(tampered, CHANGELOG, "v1.2.3", COMMIT, DIGESTS)

    def test_tampered_identity_detected(self):
        body = self.body()
        with self.assertRaisesRegex(rn.ReleaseNotesError, "identity record differs"):
            rn.check_body(body.replace(COMMIT, "0" * 40), CHANGELOG, "v1.2.3", COMMIT, DIGESTS)
        with self.assertRaisesRegex(rn.ReleaseNotesError, "identity record differs"):
            rn.check_body(body, CHANGELOG, "v1.2.3", COMMIT, dict(DIGESTS, **{"gobstopper-1.2.3-windows-x86_64.zip": "0" * 64}))

    def test_body_must_end_with_identity(self):
        body = self.body()
        for broken, expected in ((body + "\n", "does not end"),
                                 (body + "\nTrailing text", "does not end"),
                                 (body[: body.rfind(rn.IDENTITY_MARKER)] + "<!-- other -->", "no identity"),
                                 (body[: body.rfind(rn.IDENTITY_MARKER)] + rn.IDENTITY_MARKER + "{bad} -->", "not JSON"),
                                 (body[: body.rfind(rn.IDENTITY_MARKER)] + rn.IDENTITY_MARKER + '{"schema":2} -->', "unknown schema")):
            with self.subTest(expected=expected), self.assertRaisesRegex(rn.ReleaseNotesError, expected):
                rn.check_body(broken, CHANGELOG, "v1.2.3", COMMIT, DIGESTS)

    def test_record_inputs_validated(self):
        with self.assertRaisesRegex(rn.ReleaseNotesError, "full 40-character"):
            self.body(commit="faf03fe")
        with self.assertRaisesRegex(rn.ReleaseNotesError, "not vX.Y.Z"):
            self.body(tag="1.2.3")
        missing = "".join(line + "\n" for line in SUMS.splitlines() if "windows" not in line)
        with self.assertRaisesRegex(rn.ReleaseNotesError, "does not list gobstopper-1.2.3-windows-x86_64.zip"):
            rn.parse_sha256sums(missing, "v1.2.3")
        with self.assertRaisesRegex(rn.ReleaseNotesError, "not release archives: other"):
            rn.parse_sha256sums(SUMS + f"{'a' * 64}  other\n", "v1.2.3")
        with self.assertRaisesRegex(rn.ReleaseNotesError, "does not list gobstopper-1.2.4"):
            rn.parse_sha256sums(SUMS, "v1.2.4")
        with self.assertRaisesRegex(rn.ReleaseNotesError, "SHA256SUMS line"):
            rn.parse_sha256sums("nonsense\n", "v1.2.3")
        self.assertEqual(rn.parse_sha256sums(SUMS, "v1.2.3"), DIGESTS)


class CommandTests(unittest.TestCase):
    def run_main(self, *argv):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = rn.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def test_render_then_check_round_trip(self):
        with tempfile.TemporaryDirectory() as directory:
            d = Path(directory)
            (d / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\nedition = "2021"\n')
            (d / "CHANGELOG.md").write_text(CHANGELOG)
            (d / "SHA256SUMS").write_text(SUMS)
            common = ["--tag", "v1.2.3", "--commit", COMMIT, "--sha256sums", str(d / "SHA256SUMS"),
                      "--changelog", str(d / "CHANGELOG.md"), "--root", str(d)]
            code, body, _ = self.run_main("render", *common)
            self.assertEqual(code, 0)
            (d / "body.json").write_text(json.dumps({"body": body}) + "\n")
            code, out, err = self.run_main("check", *common, "--body-json", str(d / "body.json"))
            self.assertEqual((code, err), (0, ""))
            self.assertIn("Gobstopper v1.2.3: release body matches", out)
            (d / "body.json").write_text(json.dumps({"body": body.replace("sets the window", "sets the range")}))
            code, _, err = self.run_main("check", *common, "--body-json", str(d / "body.json"))
            self.assertEqual(code, 1)
            self.assertIn("release notes differ", err)
            code, _, err = self.run_main("render", *common[:1], "v1.2.4", *common[2:])
            self.assertEqual(code, 1)
            self.assertIn("does not match Cargo.toml version 1.2.3", err)


if __name__ == "__main__":
    unittest.main()

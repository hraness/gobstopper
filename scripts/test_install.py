"""scripts/install.sh against a loopback release server, offline."""

import functools
import hashlib
import http.server
import io
import os
from pathlib import Path
import platform
import subprocess
import tarfile
import tempfile
import threading
import unittest


ROOT = Path(__file__).resolve().parents[1]
INSTALL = ROOT / "scripts" / "install.sh"
VERSION = "9.8.7"


def host_platform():
    system, machine = platform.system(), platform.machine().lower()
    if system == "Darwin" and machine in ("arm64", "aarch64"):
        return "darwin-aarch64"
    if system == "Linux" and machine in ("x86_64", "amd64"):
        return "linux-x86_64"
    if system == "Linux" and machine in ("aarch64", "arm64"):
        return "linux-aarch64"
    return None


PLATFORM = host_platform()
ASSET = f"gobstopper-{VERSION}-{PLATFORM}.tar.gz"


def archive(members):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mode = 0o755
            tar.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


FAKE = f"#!/bin/sh\necho 'gobstopper {VERSION}'\n".encode()


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


@unittest.skipIf(PLATFORM is None, "no release platform for this host")
class InstallShTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.dir = Path(self.temp.name)
        self.release = self.dir / "release"
        self.release.mkdir()
        self.prefix = self.dir / "prefix"
        handler = functools.partial(Quiet, directory=str(self.release))
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.temp.cleanup()

    def publish(self, data, digest=None):
        (self.release / ASSET).write_bytes(data)
        digest = digest or hashlib.sha256(data).hexdigest()
        (self.release / f"{ASSET}.sha256").write_text(f"{digest}  {ASSET}\n")

    def install(self, **overrides):
        env = {"PATH": os.environ["PATH"], "HOME": str(self.dir), "TMPDIR": str(self.dir),
               "GOBSTOPPER_VERSION": VERSION, "GOBSTOPPER_RELEASE_BASE_URL": self.base,
               "GOBSTOPPER_INSTALL_PREFIX": str(self.prefix)}
        env.update(overrides)
        env = {key: value for key, value in env.items() if value is not None}
        return subprocess.run(["sh", str(INSTALL)], env=env, capture_output=True, text=True, timeout=60)

    def test_installs_the_checked_binary_into_the_prefix(self):
        self.publish(archive({"gobstopper": FAKE}))
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        binary = self.prefix / "bin" / "gobstopper"
        self.assertEqual(binary.read_bytes(), FAKE)
        self.assertTrue(os.access(binary, os.X_OK))
        self.assertIn(f"Installed {binary}", result.stdout)
        self.assertIn("is not on your PATH", result.stdout)
        # A second install replaces the binary in place.
        self.assertEqual(self.install().returncode, 0)
        self.assertEqual(sorted(p.name for p in (self.prefix / "bin").iterdir()), ["gobstopper"])

    def test_checksum_mismatch_installs_nothing(self):
        self.publish(archive({"gobstopper": FAKE}), digest="0" * 64)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum mismatch", result.stderr)
        self.assertFalse((self.prefix / "bin" / "gobstopper").exists())

    def test_archive_with_other_members_is_refused(self):
        self.publish(archive({"gobstopper": FAKE, "extra": b"x"}))
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must contain only gobstopper", result.stderr)
        self.assertFalse((self.prefix / "bin").exists())

    def test_binary_reporting_another_version_is_refused(self):
        self.publish(archive({"gobstopper": b"#!/bin/sh\necho 'gobstopper 1.0.0'\n"}))
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected 'gobstopper 9.8.7'", result.stderr)

    def test_inputs_are_validated(self):
        self.publish(archive({"gobstopper": FAKE}))
        for overrides, message in (
            ({"GOBSTOPPER_RELEASE_BASE_URL": "http://example.com:80"}, "loopback"),
            ({"GOBSTOPPER_VERSION": "latest"}, "exact release version"),
            ({"GOBSTOPPER_VERSION": None}, "needs GOBSTOPPER_VERSION"),
            ({"GOBSTOPPER_INSTALL_PREFIX": "relative"}, "absolute path"),
        ):
            with self.subTest(message=message):
                result = self.install(**overrides)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def test_missing_release_build_names_the_release(self):
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"releases/tag/v{VERSION}", result.stderr)


if __name__ == "__main__":
    unittest.main()

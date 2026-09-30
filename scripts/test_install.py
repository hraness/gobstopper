"""scripts/install.sh against a loopback release server, offline."""

import functools
import hashlib
import http.server
import io
import os
from pathlib import Path
import platform
import shlex
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


FAKE = f"#!/bin/sh\n[ -z \"${{FIXTURE_EXECUTION_LOG:-}}\" ] || echo executed >> \"$FIXTURE_EXECUTION_LOG\"\necho 'gobstopper {VERSION}'\n".encode()


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
        self.version = VERSION
        self.platform = PLATFORM
        self.signature_result = "valid"
        self.execution_log = self.dir / "executed"
        self.signature_log = self.dir / "codesign-called"
        self.stubs = self.dir / "stubs"
        self.stubs.mkdir()
        verifier = self.stubs / "codesign"
        requirement = 'anchor apple generic and identifier "dev.hraness.gobstopper" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "8AAP53VTW3"'
        verifier.write_text('#!/bin/sh\nset -eu\n'
            '[ "$#" = 6 ] && [ "$1" = --verify ] && [ "$2" = --strict ] && [ "$3" = --all-architectures ] && [ "$4" = --test-requirement ] || exit 91\n'
            '[ "$5" = ' + shlex.quote('=' + requirement) + ' ] || exit 92\n'
            '[ ! -e "$FIXTURE_EXECUTION_LOG" ] || exit 93\n'
            'echo verified > "$FIXTURE_SIGNATURE_LOG"\n'
            '[ "$FIXTURE_SIGNATURE_RESULT" = valid ]\n')
        verifier.chmod(0o755)
        self.installer = self.dir / "install.sh"
        # Only a private test copy substitutes Apple's system verifier.
        self.installer.write_text(INSTALL.read_text().replace('/usr/bin/codesign', shlex.quote(str(verifier))))
        handler = functools.partial(Quiet, directory=str(self.release))
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.temp.cleanup()

    def publish(self, data, digest=None):
        asset = f"gobstopper-{self.version}-{self.platform}.tar.gz"
        (self.release / asset).write_bytes(data)
        digest = digest or hashlib.sha256(data).hexdigest()
        (self.release / f"{asset}.sha256").write_text(f"{digest}  {asset}\n")

    def mac(self):
        self.platform = "darwin-aarch64"
        uname = self.stubs / "uname"
        uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Darwin ;; -m) echo arm64 ;; *) exit 1 ;; esac\n')
        uname.chmod(0o755)

    def install(self, **overrides):
        # Clear only this fixture's previous execution marker between installs.
        self.execution_log.unlink(missing_ok=True)
        env = {"PATH": str(self.stubs) + os.pathsep + os.environ["PATH"], "HOME": str(self.dir), "TMPDIR": str(self.dir),
               "GOBSTOPPER_VERSION": self.version, "GOBSTOPPER_RELEASE_BASE_URL": self.base,
               "GOBSTOPPER_INSTALL_PREFIX": str(self.prefix),
               "FIXTURE_EXECUTION_LOG": str(self.execution_log), "FIXTURE_SIGNATURE_LOG": str(self.signature_log),
               "FIXTURE_SIGNATURE_RESULT": self.signature_result}
        env.update(overrides)
        env = {key: value for key, value in env.items() if value is not None}
        return subprocess.run(["sh", str(self.installer)], env=env, capture_output=True, text=True, timeout=60)

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

    def test_loopback_cannot_replace_a_managed_native_install(self):
        self.publish(archive({"gobstopper": FAKE}))
        binary = self.prefix / "bin" / "gobstopper"
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"preserve managed executable")
        state = binary.parent / ".hraness-cli-update-gobstopper"
        state.mkdir()
        (state / "activity.lock").write_bytes(b"")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("native update coordination", result.stderr)
        self.assertEqual(binary.read_bytes(), b"preserve managed executable")

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

    def test_mac_signature_is_verified_before_candidate_execution(self):
        self.mac()
        self.publish(archive({"gobstopper": FAKE}))
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.signature_log.exists())
        self.assertTrue(self.execution_log.exists())

    def test_rejected_mac_signatures_never_execute_or_replace_existing_binary(self):
        self.mac()
        self.publish(archive({"gobstopper": FAKE}))
        existing = self.prefix / "bin" / "gobstopper"
        existing.parent.mkdir(parents=True)
        existing.write_bytes(b"preserved old installation")
        for verdict in ("unsigned", "adhoc", "wrong-team", "wrong-identifier", "wrong-certificate", "tampered"):
            with self.subTest(verdict=verdict):
                self.signature_result = verdict
                result = self.install()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("required Apple Developer ID signature", result.stderr)
                self.assertFalse(self.execution_log.exists())
                self.assertEqual(existing.read_bytes(), b"preserved old installation")

    def test_historical_mac_release_preserves_its_unsigned_install_contract(self):
        self.mac()
        self.version = "0.7.5"
        self.signature_result = "unsigned"
        self.publish(archive({"gobstopper": b"#!/bin/sh\necho 'gobstopper 0.7.5'\n"}))
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.signature_log.exists())

    def test_first_signed_mac_version_cannot_use_historical_exception(self):
        self.mac()
        self.version = "0.7.6"
        self.signature_result = "unsigned"
        self.publish(archive({"gobstopper": b"#!/bin/sh\necho 'gobstopper 0.7.6'\n"}))
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("required Apple Developer ID signature", result.stderr)

    def test_symlink_archive_is_refused_before_execution(self):
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
            member = tarfile.TarInfo("gobstopper")
            member.type = tarfile.SYMTYPE
            member.linkname = "/bin/sh"
            tar.addfile(member)
        self.publish(buffer.getvalue())
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("regular file", result.stderr)
        self.assertFalse(self.execution_log.exists())


if __name__ == "__main__":
    unittest.main()

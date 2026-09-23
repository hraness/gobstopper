import hashlib
import io
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch

import tools


class ToolPinsTests(unittest.TestCase):
    def pin(self, data=b"verified", limit=20):
        return {"sha256": hashlib.sha256(data).hexdigest(), "max_bytes": limit,
                "url": "https://github.com/owner/repo/releases/download/v1/asset"}

    def test_platform_selection_is_closed(self):
        for target in ("linux-x86_64", "darwin-arm64"):
            for tool in ("tlc", "java", "kani", "lean"):
                self.assertEqual(len(tools.selected(tool, target)["sha256"]), 64)
        with self.assertRaises(ValueError):
            tools.selected("kani", "unknown")

    def test_checksum_limit_timeout_and_truncation_fail(self):
        for data, pin, deadline in (
            (b"untrusted", self.pin(), time.monotonic() + 5),
            (b"verified", self.pin(limit=7), time.monotonic() + 5),
            (b"verifi", self.pin(), time.monotonic() + 5),
            (b"verified", self.pin(), time.monotonic() - 1),
        ):
            with self.assertRaises((ValueError, TimeoutError)):
                tools.copy_verified(io.BytesIO(data), io.BytesIO(), pin, deadline)
        self.assertEqual(tools.copy_verified(io.BytesIO(b"verified"), io.BytesIO(),
                                             self.pin(), time.monotonic() + 5), 8)

    def test_trickling_stream_uses_single_read_and_checks_deadline_after_read(self):
        class Trickling:
            def read(self, _size):
                raise AssertionError("an accumulating read has no total deadline")
            def read1(self, _size):
                return b"v"
        with patch.object(tools.time, "monotonic", side_effect=[1, 3]):
            with self.assertRaises(TimeoutError):
                tools.copy_verified(Trickling(), io.BytesIO(), self.pin(), 2)

    def test_failure_never_publishes_and_existing_output_never_downloads(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "archive"
            response = io.BytesIO(b"wrong")
            response.geturl = lambda: "https://release-assets.githubusercontent.com/file"
            with patch.object(tools.urllib.request, "urlopen", return_value=response):
                with self.assertRaises(ValueError):
                    tools.fetch(self.pin(), output)
            self.assertEqual(list(Path(directory).iterdir()), [])
            output.write_bytes(b"existing")
            with patch.object(tools.urllib.request, "urlopen") as request:
                with self.assertRaises(FileExistsError):
                    tools.fetch(self.pin(), output)
                request.assert_not_called()
            self.assertEqual(output.read_bytes(), b"existing")


if __name__ == "__main__":
    unittest.main()

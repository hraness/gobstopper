import importlib.util
from pathlib import Path
import tempfile
import unittest
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]


class BrowserAcquisitionTests(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("browser_acquisition", ROOT / "scripts" / "browser_acquisition.py")
        self.helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.helper)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "sources.list.d").mkdir()

    def put(self, name, content):
        path = self.root / name
        path.write_bytes(content.encode())
        return path

    def test_azure_dependency_source_is_normalized_without_changing_signatures(self):
        original = "Types: deb\r\nURIs: http://azure.archive.ubuntu.com/ubuntu/\r\nSuites: noble noble-updates noble-security\r\nComponents: main restricted universe multiverse\r\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg"
        path = self.put("sources.list.d/ubuntu.sources", original)
        expected = original.replace("http://azure.archive.ubuntu.com/ubuntu/", "https://archive.ubuntu.com/ubuntu/")
        self.assertEqual(self.helper.plan(self.root), [(path, expected)])
        self.assertEqual(path.read_bytes(), original.encode())
        path.write_bytes(expected.encode())
        self.assertEqual(self.helper.plan(self.root), [])

    def test_legacy_options_comments_and_third_party_sources(self):
        original = "# deb http://azure.archive.ubuntu.com/ubuntu noble main\ndeb [arch=amd64 signed-by=/keys/key.gpg] http://azure.archive.ubuntu.com/ubuntu noble main # http://azure.archive.ubuntu.com/ubuntu\ndeb-src https://azure.archive.ubuntu.com/ubuntu/ noble universe\ndeb https://packages.example.org/ubuntu noble main\n"
        path = self.put("sources.list", original)
        expected = original.replace("] http://azure.archive.ubuntu.com/ubuntu", "] https://archive.ubuntu.com/ubuntu").replace("deb-src https://azure.archive.ubuntu.com/ubuntu/", "deb-src https://archive.ubuntu.com/ubuntu/")
        self.assertEqual(self.helper.plan(self.root), [(path, expected)])

    def test_continuations_exact_hosts_and_inactive_stanzas(self):
        original = "URIs:http://azure.archive.ubuntu.com/ubuntu\n https://azure.archive.ubuntu.com/ubuntu/ https://security.ubuntu.com/ubuntu # http://azure.archive.ubuntu.com/ubuntu\nX-Note: http://azure.archive.ubuntu.com/ubuntu\n\nURIs: http://azure.archive.ubuntu.com/ubuntu-extra https://azure.archive.ubuntu.com.evil/ubuntu\n"
        path = self.put("sources.list.d/ubuntu.sources", original)
        expected = original.replace("URIs:http://azure.archive.ubuntu.com/ubuntu\n", "URIs:https://archive.ubuntu.com/ubuntu\n").replace(" https://azure.archive.ubuntu.com/ubuntu/ ", " https://archive.ubuntu.com/ubuntu/ ")
        self.assertEqual(self.helper.plan(self.root), [(path, expected)])
        for value in ("no", "FALSE", "off", "without", "disable", "0", "00", "-0x00", "+0"):
            path.write_text(f"URIs: http://azure.archive.ubuntu.com/ubuntu mirror+file:/etc/apt/apt-mirrors.txt\nEnabled:\n {value}\n")
            self.assertEqual(self.helper.plan(self.root), [])

    def test_runner_mirror_metadata_preserved_and_arbitrary_paths_not_followed(self):
        source = self.put("sources.list", "deb mirror+file:/etc/apt/apt-mirrors.txt noble main\n")
        original = "# http://azure.archive.ubuntu.com/ubuntu\r\nhttp://azure.archive.ubuntu.com/ubuntu/\tpriority:1 arch:amd64\r\nhttps://security.ubuntu.com/ubuntu/\tpriority:2\r\n"
        path = self.put("apt-mirrors.txt", original)
        expected = original.replace("http://azure.archive.ubuntu.com/ubuntu/\t", "https://archive.ubuntu.com/ubuntu/\t")
        self.assertEqual(self.helper.plan(self.root), [(path, expected)])
        path.write_bytes(expected.encode())
        self.assertEqual(self.helper.plan(self.root), [])
        source.write_text("deb mirror+file:/etc/passwd noble main\ndeb mirror+file:/etc/apt/../apt-mirrors.txt noble main\n")
        path.write_bytes(original.encode())
        self.assertEqual(self.helper.plan(self.root), [])

    def test_invalid_files_fail_before_any_write(self):
        original = "deb http://azure.archive.ubuntu.com/ubuntu noble main\n"
        path = self.put("sources.list", original)
        linked = self.root / "sources.list.d" / "linked.sources"
        linked.symlink_to(path)
        with self.assertRaises(ValueError):
            self.helper.plan(self.root)
        self.assertEqual(path.read_text(), original)
        linked.unlink()
        linked.mkdir()
        with self.assertRaises(ValueError):
            self.helper.plan(self.root)
        linked.rmdir()
        path.write_text(original + "deb mirror+file:/etc/apt/apt-mirrors.txt noble main\n")
        with self.assertRaises(ValueError):
            self.helper.plan(self.root)
        mirror = self.put("elsewhere", "http://azure.archive.ubuntu.com/ubuntu\n")
        (self.root / "apt-mirrors.txt").symlink_to(mirror)
        with self.assertRaises(ValueError):
            self.helper.plan(self.root)
        with self.assertRaises(ValueError):
            self.helper.rewrite(original, ".txt")

    def test_directory_symlinks_and_non_utf8_fail_closed(self):
        linked = self.root / "linked"
        linked.symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.helper.plan(linked)
        (self.root / "sources.list.d").rmdir()
        (self.root / "sources.list.d").symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.helper.plan(self.root)
        (self.root / "sources.list.d").unlink()
        (self.root / "sources.list").write_bytes(b"\xff")
        with self.assertRaises(UnicodeDecodeError):
            self.helper.plan(self.root)

    def test_deb822_runner_reference_and_enabled_sources(self):
        original = "Types: deb\nURIs: mirror+file:/etc/apt/apt-mirrors.txt\n https://azure.archive.ubuntu.com/ubuntu\nEnabled: yes\nSuites: noble-security\nSigned-By: /keys/ubuntu.gpg\n"
        source = self.put("sources.list.d/ubuntu.sources", original)
        mirrors = self.put("apt-mirrors.txt", "http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n")
        self.assertEqual(self.helper.plan(self.root), [
            (source, original.replace("https://azure.archive.ubuntu.com/ubuntu", "https://archive.ubuntu.com/ubuntu")),
            (mirrors, "https://archive.ubuntu.com/ubuntu/\tpriority:1\n"),
        ])
        for value in ("yes", "1", "0x1", "-1", "default"):
            source.write_text(original.replace("Enabled: yes", f"Enabled: {value}"))
            self.assertEqual(len(self.helper.plan(self.root)), 2)

    def test_workflow_acquisition_and_required_test_discovery(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        production = (ROOT / ".github/workflows/verify-production.yml").read_text()
        blocks = []
        for workflow in (ci, production):
            block = workflow.split("      - name: Provision locked Chromium\n", 1)[1].split("      - name:", 1)[0].split("      #", 1)[0]
            blocks.append(block)
            for command in (
                "timeout-minutes: 5", "sudo python3 ../scripts/browser_acquisition.py",
                'Acquire::http::Timeout "20";', 'Acquire::https::Timeout "20";',
                'Acquire::Retries "1";', 'sudo test ! -e "$config"',
                'sudo test ! -L "$config"', "trap 'sudo rm -f \"$config\"' EXIT",
                "bunx --no-install playwright-core install --with-deps chromium",
            ):
                self.assertIn(command, block)
            self.assertIn("key: playwright-${{ runner.os }}-1.61.1", workflow)
        self.assertEqual(blocks[0], blocks[1])
        python_job = ci.split("  python:\n", 1)[1].split("  windows:\n", 1)[0]
        self.assertIn("if: needs.changes.outputs.rust == 'true'", python_job)
        self.assertIn("python3 -m unittest discover -s scripts -p 'test_*.py' -v", python_job)
        rust_filter = ci.split("            rust:\n", 1)[1].split("            benchmarks:\n", 1)[0]
        self.assertIn("- '**'", rust_filter)
        self.assertNotIn("!scripts/", rust_filter)
        required = ci.split("  required:\n", 1)[1]
        self.assertIn("windows, python, macos-signing", required)
        self.assertIn('gate "$result" "$RUST"', required)
        self.assertNotIn("actions/cache@", production)


if __name__ == "__main__":
    unittest.main()

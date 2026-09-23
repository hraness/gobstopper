import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest

SPEC = importlib.util.spec_from_file_location('retention_reader_under_test', Path(__file__).with_name('retention-audit.py'))
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)


class RetentionReaderTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='gobstopper-independent-reader-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in ('manifests', 'chunks', 'records', 'objects'):
            (self.root / name).mkdir()

    def store(self, folder, data):
        digest = hashlib.sha256(data).hexdigest()
        (self.root / folder / digest).write_bytes(data)
        return digest

    def manifest(self, value):
        return self.store('manifests', json.dumps(value).encode())

    def test_versions_reconstruct_exact_bytes_and_hold_directory_custody(self):
        data = b'{"text":"first"}\n{"text":"second"}'
        chunk = self.store('chunks', data)
        v3 = self.manifest({'schema_version': 3, 'source_sha256': hashlib.sha256(data).hexdigest(),
                            'bytes': len(data), 'chunks': [chunk]})
        records = [self.store('records', line) for line in data.split(b'\n')]
        v2 = self.manifest({'schema_version': 2, 'records': records, 'trailing_newline': False,
                            'source_sha256': hashlib.sha256(data).hexdigest()})
        v0 = self.manifest({'records': records, 'trailing_newline': False})
        full = self.store('objects', data)
        exclusive = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
        self.addCleanup(os.close, exclusive)
        with AUDIT.vault_reader(self.root) as reader:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(exclusive, fcntl.LOCK_EX | fcntl.LOCK_NB)
            for sha in (v3, v2, v0, full):
                self.assertEqual(reader(sha), data)
        fcntl.flock(exclusive, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with self.assertRaises(RuntimeError):
            reader(v3)

    def test_source_mismatch_unknown_versions_bad_references_and_types_fail(self):
        data = b'content'
        chunk = self.store('chunks', data)
        base = {'schema_version': 3, 'source_sha256': hashlib.sha256(data).hexdigest(),
                'bytes': len(data), 'chunks': [chunk]}
        variants = [dict(base, source_sha256='a' * 64), dict(base, schema_version=4),
                    dict(base, schema_version='3'), dict(base, bytes=True),
                    dict(base, chunks=['../outside']), dict(base, chunks=[1]),
                    {'schema_version': 2, 'records': [], 'trailing_newline': 'false'}]
        with AUDIT.vault_reader(self.root) as reader:
            for value in variants:
                self.assertIsNone(reader(self.manifest(value)))
            self.assertIsNone(reader('../outside'))

    def test_index_uses_one_bounded_strict_image(self):
        entry = {'ts': 1, 'sha256': 'a' * 64, 'provider': 'codex', 'path': '/synthetic/source',
                 'session_id': 'synthetic', 'strategy': None, 'bytes': 1}
        raw = json.dumps(entry).encode() + b'\n'
        (self.root / 'index.jsonl').write_bytes(raw)
        with AUDIT.vault_reader(self.root) as reader:
            image, entries = reader.index()
            self.assertEqual(image, raw)
            self.assertEqual(entries, [entry])
            for invalid in (raw[:-1], raw + b'{torn}\n', b'\n', raw[:-1] + b'\r' + raw,
                            json.dumps(dict(entry, bytes=True)).encode() + b'\n'):
                (self.root / 'index.jsonl').write_bytes(invalid)
                with self.assertRaises(ValueError):
                    reader.index()

    def test_duplicate_fields_and_non_utf8_json_are_refused(self):
        data = b'content'
        chunk = self.store('chunks', data)
        base = json.dumps({'schema_version': 3, 'source_sha256': hashlib.sha256(data).hexdigest(),
                           'bytes': len(data), 'chunks': [chunk]})
        duplicate = base.replace('"source_sha256":', '"source_sha256":"' + 'a' * 64 + '","source_sha256":')
        with AUDIT.vault_reader(self.root) as reader:
            for raw in (duplicate.encode(), base.encode('utf-16'), b'[]', b'{"bytes":NaN}'):
                self.assertIsNone(reader(self.store('manifests', raw)))
            entry = {'ts': 1, 'sha256': chunk, 'provider': 'codex', 'path': '/synthetic/source',
                     'session_id': 'synthetic', 'bytes': len(data)}
            duplicate_index = json.dumps(entry).replace('"sha256":', '"sha256":"' + 'a' * 64 + '","sha256":')
            (self.root / 'index.jsonl').write_text(duplicate_index + '\n')
            with self.assertRaises(ValueError):
                reader.index()

    def test_symlink_and_special_file_are_refused_without_blocking(self):
        data = b'legacy'
        sha = hashlib.sha256(data).hexdigest()
        outside = self.root / 'outside'
        outside.write_bytes(data)
        (self.root / 'objects' / sha).symlink_to(outside)
        with AUDIT.vault_reader(self.root) as reader:
            self.assertIsNone(reader(sha))
            (self.root / 'objects' / sha).unlink()
            os.mkfifo(self.root / 'objects' / sha, 0o600)
            self.assertIsNone(reader(sha))

    def test_pairs_preserve_append_order_and_do_not_cross_stores(self):
        entries = []
        for path, ts, sha, strategy in [('/real/a', 999, 'a', 'elide'), ('/real/b', 1000, 'b', None),
                                         ('/real/a', 1, 'c', None)]:
            entries.append({'ts': ts, 'sha256': sha * 64, 'provider': 'codex', 'path': path,
                            'session_id': 'same', 'strategy': strategy, 'bytes': 1})
        selected = AUDIT.pairs(entries, lambda _: b'', [])
        self.assertEqual(len(selected), 1)
        self.assertEqual(selected[0]['before']['sha256'], 'a' * 64)
        self.assertEqual(selected[0]['after']['sha256'], 'c' * 64)

    def test_explicit_vault_materializes_inputs_without_default_vault_lookup(self):
        entries = []
        for label, data in [('elide', b'{"type":"session_meta","payload":{"id":"synthetic"}}\n'),
                            (None, b'{"type":"session_meta","payload":{"id":"synthetic"}}\n{"type":"compacted"}\n')]:
            chunk = self.store('chunks', data)
            sha = self.manifest({'schema_version': 3, 'chunks': [chunk], 'bytes': len(data),
                                 'source_sha256': hashlib.sha256(data).hexdigest()})
            entries.append({'ts': 1, 'sha256': sha, 'provider': 'codex', 'path': '/synthetic/session',
                            'session_id': 'synthetic', 'strategy': label, 'bytes': len(data)})
        (self.root / 'index.jsonl').write_text(''.join(json.dumps(e) + '\n' for e in entries))
        capture = self.root / 'argv.json'
        fake = self.root / 'fake-gobstopper'
        fake.write_text('#!' + sys.executable + '\nimport json,sys\nfrom pathlib import Path\n'
                        + 'Path(' + repr(str(capture)) + ').write_text(json.dumps(sys.argv))\nraise SystemExit(1)\n')
        fake.chmod(0o700)
        output = self.root / 'audit-output'
        result = subprocess.run([sys.executable, str(Path(AUDIT.__file__)), '--binary', str(fake),
                                 '--output', str(output), '--vault-root', str(self.root)],
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        argv = json.loads(capture.read_text())
        self.assertEqual(argv[1], 'eval-study')
        self.assertEqual(Path(argv[2]), output / 'pair-0' / 'before.jsonl')
        self.assertFalse(any(arg.startswith('vault:') for arg in argv))
        with AUDIT.vault_reader(self.root) as reader:
            self.assertEqual(Path(argv[2]).read_bytes(), reader(entries[0]['sha256']))


if __name__ == '__main__':
    unittest.main()

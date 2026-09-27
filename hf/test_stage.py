import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('stage', Path(__file__).with_name('stage.py'))
stage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stage)


class ExportTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve()
        (self.root / 'hf').mkdir()
        (self.root / 'hf/README.md').write_text('# Card\n')
        self.source = stage.SOURCE_PREFIX + 'v1/report.json'
        path = self.root / self.source
        path.parent.mkdir(parents=True)
        path.write_text('{}\n')
        self.manifest = {'schema': 'hraness-hf-export-v1', 'repo_type': 'dataset', 'repo_id': stage.REPO_ID,
                         'files': [{'source': self.source, 'destination': 'data/v1/report.json',
                                    'sha256': hashlib.sha256(b'{}\n').hexdigest()}]}
        self.save()

    def tearDown(self):
        self.tmp.cleanup()

    def save(self):
        (self.root / 'hf/manifest.json').write_text(json.dumps(self.manifest))

    def test_exports_only_allowlisted_bytes(self):
        (self.root / 'private.json').write_text('do not export')
        _, files, _ = stage.validate(self.root)
        self.assertEqual(files, [('data/v1/report.json', b'{}\n')])

    def test_rejects_changed_bytes(self):
        (self.root / self.source).write_text('{"changed":true}')
        with self.assertRaises(ValueError):
            stage.validate(self.root)

    def test_rejects_escape(self):
        self.manifest['files'][0]['source'] = stage.SOURCE_PREFIX + '../private.json'
        self.manifest['files'][0]['destination'] = 'data/../private.json'
        self.save()
        with self.assertRaises(ValueError):
            stage.validate(self.root)

    def test_rejects_symlink(self):
        (self.root / self.source).unlink()
        (self.root / 'private.json').write_text('{}\n')
        (self.root / self.source).symlink_to(self.root / 'private.json')
        with self.assertRaises(ValueError):
            stage.validate(self.root)

    def test_rejects_duplicate_and_wrong_destination(self):
        self.manifest['files'].append(dict(self.manifest['files'][0]))
        self.save()
        with self.assertRaises(ValueError):
            stage.validate(self.root)
        self.manifest['files'].pop()
        self.manifest['repo_id'] = 'someone/else'
        self.save()
        with self.assertRaises(ValueError):
            stage.validate(self.root)

    def test_rejects_raw_transcripts(self):
        self.manifest['files'][0]['source'] = 'results/session.json'
        self.save()
        with self.assertRaises(ValueError):
            stage.validate(self.root)


if __name__ == '__main__':
    unittest.main()

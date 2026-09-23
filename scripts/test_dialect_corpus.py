#!/usr/bin/env python3
"""Negative controls for the frozen-corpus admission checker."""
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("dialect_check", Path(__file__).with_name("check-dialects.py"))
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)
ORIGINAL = CHECK.CORPUS


class CorpusAdmission(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="gob-dialect-corpus-")
        self.corpus = Path(self.temp.name) / "corpus"
        shutil.copytree(ORIGINAL, self.corpus)
        CHECK.CORPUS = self.corpus
        self.manifest = self.corpus / "manifest.json"

    def tearDown(self):
        CHECK.CORPUS = ORIGINAL
        self.temp.cleanup()

    def test_valid_frozen_contract(self):
        CHECK.check_corpus()

    def test_fixture_drift_is_rejected(self):
        with (self.corpus / "codex-window.jsonl").open("ab") as output:
            output.write(b"\n")
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            CHECK.check_corpus()

    def test_duplicate_manifest_field_is_rejected(self):
        self.manifest.write_text('{"schema":1,"schema":1}')
        with self.assertRaisesRegex(ValueError, "duplicate corpus key"):
            CHECK.check_corpus()

    def test_unlisted_corpus_is_rejected(self):
        (self.corpus / "unlisted.jsonl").write_text('{}\n')
        with self.assertRaises(ValueError):
            CHECK.check_corpus()

    def test_missing_projection_anchor_is_rejected(self):
        doc = json.loads(self.manifest.read_text())
        doc["fixtures"][0]["changes"][0]["line"] = 0
        self.manifest.write_text(json.dumps(doc))
        with self.assertRaises(ValueError):
            CHECK.check_corpus()


if __name__ == "__main__":
    unittest.main()

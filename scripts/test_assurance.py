"""Adversarial admission checks for the C1 inventory, with no provider execution."""

import copy
import json
from pathlib import Path
import tempfile
import unittest

import check_assurance as assurance


ROOT = Path(__file__).resolve().parents[1]


class AssuranceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.documents = assurance.load_documents(ROOT)

    def changed(self):
        return copy.deepcopy(self.documents)

    def reject(self, documents, expected):
        errors = assurance.validate(ROOT, documents)
        self.assertTrue(any(expected in error for error in errors), errors)

    def test_current_inventory(self):
        self.assertEqual([], assurance.validate(ROOT, self.documents))

    def test_unsupported_claim_fixture(self):
        documents = self.changed()
        fixture = json.loads((ROOT / "docs/assurance/fixtures/unsupported-claim.json").read_text())
        claim = next(c for c in documents["claims"]["claims"] if c["id"] == fixture["claim_id"])
        claim.update(fixture["replacement"])
        errors = assurance.validate(ROOT, documents)
        for expected in fixture["expected_errors"]:
            self.assertTrue(any(expected in e for e in errors), errors)

    def test_bounded_claim_needs_pinned_evidence(self):
        documents = self.changed()
        claim = next(c for c in documents["claims"]["claims"] if c["id"] == "CLAIM-VAULT")
        claim["evidence"] = ["E-SOURCE"]
        self.reject(documents, "bounded check lacks a pinned receipt")

    def test_coverage_cannot_disappear(self):
        documents = self.changed()
        documents["ledger"]["coverage"].pop()
        self.reject(documents, "differs from audit coverage map")

    def test_obligation_requires_owner_and_exclusions(self):
        documents = self.changed()
        row = documents["ledger"]["invariants"][0]
        row["owner"] = ""
        row["exclusions"] = []
        self.reject(documents, "missing owner")
        self.reject(documents, "missing exclusions")

    def test_unknown_write_state_rejected(self):
        documents = self.changed()
        documents["effects"]["profiles"][0]["writes"] = [{"state": "unclassified-live-store", "mode": "replace", "condition": "always"}]
        self.reject(documents, "unknown write state")

    def test_public_surfaces_cannot_disappear(self):
        for collection, expected in [("cli", "CLI inventory differs"), ("mcp", "MCP inventory differs"), ("hooks", "hook inventory differs"), ("scripts", "script effect inventory differs"), ("library_callables", "Rust public callable inventory drift")]:
            with self.subTest(collection=collection):
                documents = self.changed()
                documents["effects"][collection].pop()
                self.reject(documents, expected)

    def test_unknown_effect_and_content_permission_rejected(self):
        documents = self.changed()
        documents["effects"]["mcp"][0]["effects"] = ["arbitrary-plugin"]
        documents["effects"]["mcp"][0]["content_opt_in"] = True
        self.reject(documents, "unknown reference arbitrary-plugin")
        self.reject(documents, "content gate mismatch")

    def test_baseline_alerts_cannot_be_dismissed_or_dropped(self):
        documents = self.changed()
        documents["codeql-triage"]["alerts"][0]["scanner_action"] = "dismiss"
        self.reject(documents, "triage must not dismiss or suppress")
        documents["codeql-triage"]["alerts"].pop()
        self.reject(documents, "preserve all 21 exact alert IDs")

    def test_activation_is_separate_and_required(self):
        documents = self.changed()
        documents["ledger"]["activation_gates"] = [documents["ledger"]["activation_gates"][0]]
        self.reject(documents, "missing activation boundaries")

    def test_source_reference_and_schema_must_be_supported(self):
        documents = self.changed()
        documents["claims"]["claims"][0]["public_sources"] = [{"path": "../outside"}]
        documents["effects"]["schema"] = "gobstopper.effects.v999"
        self.reject(documents, "invalid source path")
        self.reject(documents, "unsupported schema")

    def test_duplicate_json_keys_rejected(self):
        with self.assertRaisesRegex(ValueError, "duplicate JSON key"):
            json.loads('{"status":"specified","status":"proven"}', object_pairs_hook=assurance.unique_object)

    def test_callable_scan_detects_new_mutator_and_excludes_test_module(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "crates/gobstopper-adapters/src/example.rs"
            source.parent.mkdir(parents=True)
            source.write_text(
                'pub fn added_mutator() {}\n'
                'pub unsafe fn unsafe_mutator() {}\n'
                'pub const fn constant() {}\n'
                'pub(crate) async unsafe fn qualified_mutator() {}\n'
                'pub unsafe extern "C" fn ffi_mutator() {}\n'
                '#[cfg(test)]\nmod tests {\npub fn fixture() {}\n}\n'
            )
            self.assertEqual(
                {("crates/gobstopper-adapters/src/example.rs", name) for name in
                 ("added_mutator", "unsafe_mutator", "constant", "qualified_mutator", "ffi_mutator")},
                assurance.source_callables(root),
            )


if __name__ == "__main__":
    unittest.main()

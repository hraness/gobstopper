import copy
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("registered_study", Path(__file__).with_name("registered-study.py"))
STUDY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STUDY)


class RegisteredStudyTests(unittest.TestCase):
    def test_frozen_corpus_and_unexecuted_native_arms(self):
        registration, digest = STUDY.validate_corpus()
        self.assertEqual(len(digest), 64)
        self.assertEqual({case["id"] for case in registration["cases"]},
                         {"negation", "superseded_approval", "pending_effect", "tool_state"})
        self.assertEqual(sum(arm["mode"].startswith("not_run") for arm in registration["arms"]), 2)
        self.assertIsNone(registration["metrics"]["continuation_task_success"])

    def test_asset_change_is_detected_before_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "corpus"
            shutil.copytree(STUDY.CORPUS, root)
            (root / "negation.jsonl").write_text("changed")
            with self.assertRaisesRegex(RuntimeError, "asset_changed"):
                STUDY.validate_corpus(root)

    def test_duplicate_registration_fields_are_rejected(self):
        with self.assertRaisesRegex(RuntimeError, "duplicate_json_field"):
            STUDY.strict_json('{"provider_calls":0,"provider_calls":1}')

    def test_report_rejects_missing_arms_wrong_source_and_impossible_counts(self):
        registration, _ = STUDY.validate_corpus()
        case = registration["cases"][0]
        report = {"source_sha256": registration["asset_sha256"][case["source"]],
                  "manifest_sha256": registration["asset_sha256"][case["manifest"]],
                  "provider_calls": 0, "billed_cost_usd": None, "continuation_success": None,
                  "semantic_equivalence_qualified": False, "rows": [
                      {"arm": "realized_after", "round": 1, "verify_errors": 0, "new_verify_errors": 0,
                       "retention": {"total": 1, "retained": 1, "lexical_retained": 1,
                                     "same_origin_retained": 1, "source_bound_retained": 1}}]}
        STUDY.validate_report(report, case, registration, control=True)
        for mutation, reason in ((lambda r: r.update(rows=[]), "incomplete_arms"),
                                 (lambda r: r.update(source_sha256="f" * 64), "source_binding"),
                                 (lambda r: r["rows"][0]["retention"].update(retained=2), "invalid_counter"),
                                 (lambda r: r.update(continuation_success=True), "unsupported_measurement_claim")):
            changed = copy.deepcopy(report)
            mutation(changed)
            with self.assertRaisesRegex(RuntimeError, reason):
                STUDY.validate_report(changed, case, registration, control=True)


if __name__ == "__main__":
    unittest.main()

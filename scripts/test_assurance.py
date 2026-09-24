"""Adversarial admission checks for the C1 inventory, with no provider execution."""

import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

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

    def reject_receipt(self, family, mutation, expected):
        documents = self.changed()
        evidence = next(e for e in documents["ledger"]["evidence"] if e["id"] == f"E-{family.upper()}-CURRENT")
        receipt_path = ROOT / evidence["receipt"]["path"]
        receipt = json.loads(receipt_path.read_text())
        mutation(receipt)
        altered = json.dumps(receipt).encode()
        evidence["receipt"]["sha256"] = hashlib.sha256(altered).hexdigest()
        original_read = Path.read_bytes
        def read(path):
            return altered if path == receipt_path else original_read(path)
        with patch.object(Path, "read_bytes", read):
            self.reject(documents, expected)

    def contract_errors(self, family, receipt):
        errors = []
        assurance.validate_receipt_contract(
            ROOT, f"E-{family.upper()}-CURRENT", receipt,
            lambda condition, message: None if condition else errors.append(message),
        )
        return errors

    def stress_contract_fixture(self):
        # Synthetic checker input, not execution evidence or a ledger receipt.
        # Keep bounds explicit so changes to the validator cannot silently
        # change the expected fixture along with the check under test.
        suites = json.loads((ROOT / "verify/stress/suites.json").read_text())["suites"]
        results = [{"case": suite["name"], "passed": True, "exit_code": 0,
                    "timeout": False, "log_limit": False, "log_sha256": "1" * 64,
                    "passed_tests": sorted(suite["tests"]), "elapsed_seconds": 1,
                    "largest_reaped_child_rss_bytes": 1024, "sequence_metrics": None}
                   for suite in suites]
        next(row for row in results if row["case"] == "sequence")["sequence_metrics"] = {
            "seed": 0x6a09e667f3bcc909, "steps": 64, "corruption_recoveries": 16,
            "peak_files": 402, "peak_bytes": 334635, "elapsed_ms": 1000,
        }
        return {
            "results": results,
            "source_sha256": {name: "2" * 64 for name in assurance.receipt_sources(ROOT, "stress", ())},
            "tool_sha256": {name: "3" * 64 for name in
                            ("cargo", "rustc", "cargo-dispatcher", "rustc-dispatcher", "python")},
            "platform": "darwin", "elapsed_seconds": 20,
            "identity_log_sha256": {name: "4" * 64 for name in
                                    ("rust-sysroot.log", "cargo-version.log", "rust-version.log")},
            "raw_bounds": {"total_seconds": 900, "max_log_bytes_per_command": 8 * 1024 * 1024,
                           "cargo_jobs": 2, "test_threads": 1,
                           "max_observed_single_child_rss_bytes": 2 * 1024 * 1024 * 1024,
                           "sequence_steps": 64, "sequence_corruption_recoveries": 16,
                           "sequence_post_step_file_limit": 1200,
                           "sequence_post_step_bytes_limit": 16 * 1024 * 1024,
                           "sequence_elapsed_ms_limit": 90_000},
        }

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

    def test_proof_setup_helpers_are_in_effect_inventory(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name in ("scripts/production.py", "scripts/test_fixture.py",
                         "verify/tools.py", "verify/model/check.py",
                         "verify/model/test_check.py", "verify/model/__init__.py"):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("")
            self.assertEqual({"scripts/production.py", "verify/tools.py",
                              "verify/model/check.py"}, assurance.source_scripts(root))

    def test_verification_receipt_rejects_stale_sources_and_failed_cases(self):
        for mutation, expected in (
            (lambda r: r["source_sha256"].update({"verify/vault/Vault.tla": "0" * 64}), "receipt source drift"),
            (lambda r: r["results"][0].update(passed=False), "failed receipt case"),
            (lambda r: r.update(inputs_unchanged=False), "did not pass unchanged"),
            (lambda r: r["results"].append(r["results"][0]), "duplicate receipt case"),
        ):
            with self.subTest(expected=expected):
                self.reject_receipt("vault", mutation, expected)

    def test_receipt_reference_cannot_bypass_validation_with_a_url(self):
        documents = self.changed()
        evidence = next(e for e in documents["ledger"]["evidence"] if e["id"] == "E-VAULT-CURRENT")
        evidence["receipt"] = {"url": "https://example.invalid/unsupported-receipt"}
        self.reject(documents, "receipt must be a local pinned file")

    def test_tlc_receipt_requires_complete_intended_controls_and_input_identities(self):
        for mutation, expected in (
            (lambda r: r.update(results=r["results"][:1]), "receipt case inventory differs"),
            (lambda r: r["source_sha256"].pop("verify/vault/Vault.tla"), "receipt source inventory differs"),
            (lambda r: r["tool_sha256"].pop("java"), "receipt tool inventory differs"),
            (lambda r: r["tool_sha256"].update({"tlc.jar": "0" * 64}), "TLC tool differs from reviewed pin"),
            (lambda r: r["results"][0].update(timeout=True), "incomplete resource-limited case"),
            (lambda r: r["results"][0].update(log_limit=True), "incomplete resource-limited case"),
            (lambda r: r["results"][0]["states"].update(queued=1), "incomplete state exploration"),
            (lambda r: r["results"][1].update(exit_code=1), "wrong result exit"),
            (lambda r: r["results"][1].update(expected_invariant_violation="WrongInvariant"), "wrong intended invariant"),
        ):
            with self.subTest(expected=expected):
                self.reject_receipt("vault", mutation, expected)

    def test_core_receipt_requires_the_actual_boundary_mutation_and_proof_bounds(self):
        for mutation, expected in (
            (lambda r: r.update(results=r["results"][:1]), "receipt case inventory differs"),
            (lambda r: r["results"][1].update(failed_labels=["could not compile"]), "wrong intended assertion"),
            (lambda r: r["mutation"].update(source_sha256="0" * 64), "mutant source identity mismatch"),
            (lambda r: r["unwind_bounds"].update(edit_sequences_four=1), "wrong unwind bounds"),
            (lambda r: r["versions"].update(kani="other"), "wrong proof versions"),
            (lambda r: r["results"][0]["counts"].update(assertions=1), "incomplete proof counts"),
            (lambda r: r["results"][1]["counts"].update(unwind_checks=False), "incomplete proof counts"),
            (lambda r: r.update(error="proof execution failed"), "contradictory receipt error"),
        ):
            with self.subTest(expected=expected):
                self.reject_receipt("core", mutation, expected)

    def test_lean_receipt_cannot_omit_kernel_replay_or_axiom_and_mutant_evidence(self):
        for mutation, expected in (
            (lambda r: r.update(results=[c for c in r["results"] if c["case"] != "kernel-replay"]), "receipt case inventory differs"),
            (lambda r: r["axioms"].pop("protected_identity"), "incomplete or unsupported axiom audit"),
            (lambda r: r["axioms"].update(protected_identity=["sorryAx"]), "incomplete or unsupported axiom audit"),
            (lambda r: r["vectors"].update(cases=1), "incomplete vector correspondence"),
            (lambda r: r["mutations"]["rust"].update(source_sha256="0" * 64), "mutant source identity mismatch"),
            (lambda r: r["mutations"]["rust"].pop("uncompiled_cli_metadata_target", None), "missing isolated workspace metadata declaration"),
        ):
            with self.subTest(expected=expected):
                self.reject_receipt("transcript", mutation, expected)

    def test_lean_receipt_binds_all_tool_roles_and_selected_sysroot(self):
        for role in ("lake", "lean", "leanchecker", "cargo", "rustc", "cargo-dispatcher", "rustc-dispatcher", "python"):
            with self.subTest(role=role):
                self.reject_receipt("transcript", lambda r: r["tool_sha256"].pop(role, None),
                                    "receipt tool inventory differs")
        self.reject_receipt("transcript",
                            lambda r: r.update(results=[c for c in r["results"] if c["case"] != "rust-sysroot"]),
                            "receipt case inventory differs")

    def test_stress_receipt_requires_complete_tests_tools_sources_and_measured_bounds(self):
        original = self.stress_contract_fixture()
        self.assertEqual([], self.contract_errors("stress", original))
        for mutation, expected in (
            (lambda r: r["results"].pop(), "receipt case inventory differs"),
            (lambda r: r["results"][0]["passed_tests"].clear(), "incomplete named tests"),
            (lambda r: r["results"][0]["passed_tests"].append(r["results"][0]["passed_tests"][0]), "incomplete named tests"),
            (lambda r: r["source_sha256"].pop("Cargo.lock"), "receipt source inventory differs"),
            (lambda r: r["tool_sha256"].pop("cargo-dispatcher"), "receipt tool inventory differs"),
            (lambda r: r["tool_sha256"].update(python3=r["tool_sha256"].pop("python")), "receipt tool inventory differs"),
            (lambda r: r["identity_log_sha256"].pop("rust-sysroot.log"), "incomplete tool identity logs"),
            (lambda r: r["identity_log_sha256"].update({"rust-version.log": "not-a-digest"}), "incomplete tool identity logs"),
            (lambda r: r.update(platform="unsupported"), "unsupported resource accounting platform"),
            (lambda r: r["raw_bounds"].update(total_seconds=901), "stress resource bounds differ"),
            (lambda r: r["raw_bounds"].update(test_threads=True), "stress resource bounds differ"),
            (lambda r: r.update(elapsed_seconds=901), "invalid aggregate elapsed bound"),
            (lambda r: r.update(elapsed_seconds=1), "aggregate elapsed contradicts suite durations"),
            (lambda r: r["results"][0].update(elapsed_seconds=float("nan")), "invalid elapsed bound"),
            (lambda r: r["results"][0].update(elapsed_seconds=10**1000), "invalid elapsed bound"),
            (lambda r: r["results"][0].update(largest_reaped_child_rss_bytes=2 * 1024**3 + 1), "invalid observed child memory bound"),
            (lambda r: r["results"][0]["sequence_metrics"].update(corruption_recoveries=15), "incomplete sequence bounds"),
            (lambda r: r["results"][0].update(timeout=True), "incomplete resource-limited case"),
            (lambda r: r.update(error="the run failed"), "contradictory receipt error"),
        ):
            with self.subTest(expected=expected):
                receipt = copy.deepcopy(original)
                mutation(receipt)
                errors = self.contract_errors("stress", receipt)
                self.assertTrue(any(expected in error for error in errors), errors)

    def test_stress_contract_cannot_lower_the_runner_test_count_via_suite_manifest(self):
        original = self.stress_contract_fixture()
        manifest_path = ROOT / "verify/stress/suites.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["suites"][0]["tests"] = []
        original["results"][0]["passed_tests"] = []
        original_read = Path.read_text

        def read(path, *args, **kwargs):
            return json.dumps(manifest) if path == manifest_path else original_read(path, *args, **kwargs)

        with patch.object(Path, "read_text", read):
            errors = self.contract_errors("stress", original)
        self.assertTrue(any("stress suite command or bounds differ" in error for error in errors), errors)

    def test_runner_contract_reader_never_executes_python(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "check.py"
            sentinel = Path(tmp) / "must-not-exist"
            path.write_text(f"from pathlib import Path\nPath({str(sentinel)!r}).touch()\nCASES = {{'safe': None}}\n")
            self.assertEqual({"CASES": {"safe": None}}, assurance.literal_contract(path, ("CASES",)))
            self.assertFalse(sentinel.exists())
            path.write_text(f"CASES = __import__('pathlib').Path({str(sentinel)!r}).touch()\n")
            with self.assertRaises(ValueError):
                assurance.literal_contract(path, ("CASES",))
            self.assertFalse(sentinel.exists())

    def test_runner_contract_arithmetic_is_bounded_and_cannot_execute(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "check.py"
            path.write_text("LIMIT = 8 * 1024 * 1024\n")
            self.assertEqual({"LIMIT": 8388608}, assurance.literal_contract(path, ("LIMIT",)))
            for expression in ("2 ** 1000000", "'x' * 1000000", "18446744073709551616 * 2", "8 * int('2')"):
                with self.subTest(expression=expression):
                    path.write_text(f"LIMIT = {expression}\n")
                    with self.assertRaises(ValueError):
                        assurance.literal_contract(path, ("LIMIT",))


if __name__ == "__main__":
    unittest.main()

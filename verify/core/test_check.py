#!/usr/bin/env python3
"""Negative admission tests; no Kani invocation or source mutation."""

import copy
from pathlib import Path
import unittest

import check


def document():
    results = []
    for name, required in check.HARNESSES.items():
        checks = [{"category": "assertion", "description": label, "status": "Success"}
                  for label in required]
        checks.append({"category": "cover", "description": "reachable boundary", "status": "Satisfied"})
        if "indexes_" in name and "empty" not in name:
            checks.append({"category": "unwind", "description": "unwinding assertion loop 0",
                           "status": "Success"})
        results.append({"harness_id": name, "status": "Success", "checks": checks})
    return {
        "metadata": {"version": "1.0"},
        "tools": {"kani": check.KANI_VERSION, "cbmc": check.CBMC_VERSION, "rustc": check.RUSTC_VERSION},
        "harness_metadata": [{"pretty_name": name, "attributes": {"kind": "Proof", "should_panic": False}}
                             for name in check.HARNESSES],
        "verification_results": {
            "summary": {"total_harnesses": len(results), "executed": len(results),
                        "status": "completed", "successful": len(results), "failed": 0},
            "results": results,
        },
    }


class AdmissionTests(unittest.TestCase):
    def passed(self, data, **kwargs):
        return check.admit(data, kwargs.pop("exit_code", 0), kwargs.pop("timeout", False),
                           kwargs.pop("log_limit", False), **kwargs)["passed"]

    def test_complete_positive_document(self):
        self.assertTrue(self.passed(document()))

    def test_missing_or_unexecuted_harness_refuses(self):
        data = document()
        data["verification_results"]["results"].pop()
        self.assertFalse(self.passed(data))
        data = document()
        data["verification_results"]["summary"]["executed"] -= 1
        self.assertFalse(self.passed(data))

    def test_success_banner_cannot_hide_unreachable_or_unsatisfied_cover(self):
        for status in ("Unreachable", "Unsatisfiable", "Undetermined"):
            data = document()
            data["verification_results"]["results"][0]["checks"][-1]["status"] = status
            self.assertFalse(self.passed(data))

    def test_required_assertion_cannot_be_unreachable(self):
        data = document()
        data["verification_results"]["results"][0]["checks"][0]["status"] = "Unreachable"
        self.assertFalse(self.passed(data))

    def test_unwinding_and_unsupported_paths_must_be_closed(self):
        for category in ("unwind", "unsupported_construct"):
            for status in ("Failure", "Undetermined", "Unreachable"):
                data = document()
                data["verification_results"]["results"][0]["checks"].append(
                    {"category": category, "status": status, "description": "not proved"})
                self.assertFalse(self.passed(data))

    def test_timeout_wrong_tool_and_log_limit_refuse(self):
        self.assertFalse(self.passed(document(), timeout=True))
        self.assertFalse(self.passed(document(), log_limit=True))
        self.assertFalse(self.passed(document(), exit_code=1))
        data = document()
        data["tools"]["kani"] = "different"
        self.assertFalse(self.passed(data))

    def test_mutant_requires_only_the_intended_assertion_failure(self):
        data = document()
        data["harness_metadata"] = [row for row in data["harness_metadata"]
                                    if row["pretty_name"] == check.BOUNDARY_HARNESS]
        verification = data["verification_results"]
        verification["results"] = [row for row in verification["results"]
                                   if row["harness_id"] == check.BOUNDARY_HARNESS]
        verification["summary"] = {"total_harnesses": 1, "executed": 1, "status": "completed",
                                    "successful": 0, "failed": 1}
        result = verification["results"][0]
        result["status"] = "Failure"
        result["checks"][0]["status"] = "Failure"
        self.assertTrue(self.passed(data, mutant=True, exit_code=1))
        changed = copy.deepcopy(data)
        changed["verification_results"]["results"][0]["checks"][0]["description"] = "wrong failure"
        self.assertFalse(self.passed(changed, mutant=True, exit_code=1))
        result["checks"].append({"category": "unwind", "description": "short bound", "status": "Failure"})
        self.assertFalse(self.passed(data, mutant=True, exit_code=1))

    def test_production_source_hygiene_and_unwind_inventory(self):
        root = Path(__file__).resolve().parents[2]
        sources = {str(path.relative_to(root)): path.read_bytes()
                   for path in (root / "crates/gobstopper-core/src").rglob("*.rs")}
        check.check_sources(sources)
        name = "crates/gobstopper-core/src/admission.rs"
        for replacement in (b"kani::assume(false);", b"#[kani::stub(actual, toy)]"):
            changed = dict(sources)
            changed[name] += replacement
            with self.assertRaises(ValueError):
                check.check_sources(changed)
        changed = dict(sources)
        changed[name] = changed[name].replace(b"#[kani::unwind(6)]", b"#[kani::unwind(1)]")
        with self.assertRaises(ValueError):
            check.check_sources(changed)


if __name__ == "__main__":
    unittest.main()

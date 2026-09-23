#!/usr/bin/env python3
"""Reject weakened vault configurations and unrelated negative-control failures."""

from pathlib import Path
import unittest

import check


SAFE = """Model checking completed. No error has been found.
2,000 states generated, 1,000 distinct states found, 0 states left on queue.
"""
COUNTEREXAMPLE = """Error: Invariant CompletedDurable is violated.
Error: The behavior up to this point is:
State 1: <Init>
State 2: <Publish>
1,000 states generated, 500 distinct states found, 25 states left on queue.
"""


class VaultAdmissionTests(unittest.TestCase):
    def test_all_configs_match_reviewed_case_contract(self):
        source = Path(__file__).resolve().parent
        check.validate_configs({f"{name}.cfg": (source / f"{name}.cfg").read_bytes()
                                for name in check.CASES})

    def test_missing_property_wrong_mutant_or_mutated_witness_fails(self):
        configs = {f"{name}.cfg": check.expected_config(name).encode() for name in check.CASES}
        for name, old, new in (
            ("safe", " ExclusiveCustody", ""),
            ("no-pins", "RespectPins = FALSE", "RespectPins = TRUE"),
            ("witness-completion", "UseCustody = TRUE", "UseCustody = FALSE"),
            ("publication-safe", " CompletedDurable", ""),
            ("publication-no-sync", "RequireSync = FALSE", "RequireSync = TRUE"),
            ("publication-recovery", "RequireIntent = TRUE", "RequireIntent = FALSE"),
        ):
            changed = dict(configs)
            changed[f"{name}.cfg"] = changed[f"{name}.cfg"].replace(old.encode(), new.encode())
            with self.assertRaises(ValueError, msg=name):
                check.validate_configs(changed)

    def test_safe_requires_complete_nontrivial_unique_stats(self):
        def admitted(raw, code=0, timeout=False, limit=False):
            return check.runner.admit_result(raw, code, timeout, limit, None)["passed"]
        self.assertTrue(admitted(SAFE))
        for raw in (SAFE + SAFE, SAFE.replace("0 states left", "1 states left"),
                    SAFE.replace("1,000 distinct", "0 distinct"), SAFE + "Error: failure\n",
                    "Semantic error: missing model", "java.net.SocketException: denied"):
            self.assertFalse(admitted(raw))
        self.assertFalse(admitted(SAFE, timeout=True))
        self.assertFalse(admitted(SAFE, limit=True))
        self.assertFalse(admitted(SAFE, code=12))

    def test_counterexample_is_exact_and_cannot_hide_another_error(self):
        def admitted(raw=COUNTEREXAMPLE, code=12):
            return check.runner.admit_result(raw, code, False, False,
                                             "CompletedDurable")["passed"]
        self.assertTrue(admitted())
        self.assertFalse(admitted(code=255))
        self.assertFalse(admitted(COUNTEREXAMPLE.replace("CompletedDurable", "TypeOK")))
        self.assertFalse(admitted(COUNTEREXAMPLE.replace("State 2:", "Incomplete:")))
        self.assertFalse(admitted(COUNTEREXAMPLE + "Error: unexpected exception\n"))
        self.assertFalse(admitted(COUNTEREXAMPLE + SAFE))


if __name__ == "__main__":
    unittest.main()

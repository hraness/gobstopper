#!/usr/bin/env python3
"""Focused negative tests for receipt admission and owned TLC process cleanup."""

import os
from pathlib import Path
import sys
import tempfile
import time
import unittest

import check


SAFE = """Model checking completed. No error has been found.
2,000 states generated, 1,000 distinct states found, 0 states left on queue.
"""
COUNTEREXAMPLE = """Error: Invariant NoUnresolvedReplay is violated.
Error: The behavior up to this point is:
State 1: <Initial predicate>
State 2: <Send>
1,000 states generated, 500 distinct states found, 25 states left on queue.
"""


class AdmissionTests(unittest.TestCase):
    def admitted(self, raw=SAFE, code=0, timeout=False, limit=False, invariant=None):
        return check.admit_result(raw, code, timeout, limit, invariant)["passed"]

    def test_safe_requires_exhaustion_and_nontrivial_graph(self):
        self.assertTrue(self.admitted())
        self.assertFalse(self.admitted(SAFE.replace("0 states left", "1 states left")))
        self.assertFalse(self.admitted(SAFE.replace("1,000 distinct", "1 distinct")))
        self.assertFalse(self.admitted("Model checking completed. No error has been found."))
        self.assertFalse(self.admitted(SAFE + "Error: semantic exception\n"))
        self.assertFalse(self.admitted(code=12))

    def test_intended_counterexample_requires_exact_invariant_and_exit(self):
        self.assertTrue(self.admitted(COUNTEREXAMPLE, 12, invariant="NoUnresolvedReplay"))
        self.assertFalse(self.admitted(COUNTEREXAMPLE, 0, invariant="NoUnresolvedReplay"))
        self.assertFalse(self.admitted(COUNTEREXAMPLE, 12, invariant="AppliedNeedsTerminal"))
        self.assertFalse(self.admitted(COUNTEREXAMPLE.replace("State 2:", "Trace missing:"),
                                       12, invariant="NoUnresolvedReplay"))
        self.assertFalse(self.admitted(COUNTEREXAMPLE + "Error: unexpected exception\n",
                                       12, invariant="NoUnresolvedReplay"))
        self.assertFalse(self.admitted(COUNTEREXAMPLE + SAFE,
                                       12, invariant="NoUnresolvedReplay"))

    def test_timeout_or_truncated_log_never_counts_as_evidence(self):
        for raw, code, invariant in ((SAFE, 0, None),
                                     (COUNTEREXAMPLE, 12, "NoUnresolvedReplay")):
            self.assertFalse(self.admitted(raw, code, timeout=True, invariant=invariant))
            self.assertFalse(self.admitted(raw, code, limit=True, invariant=invariant))

    def test_parse_permission_and_generic_errors_are_not_counterexamples(self):
        for error in ("Semantic errors", "java.net.SocketException: Operation not permitted",
                      "Error: TLC threw an unexpected exception."):
            self.assertFalse(self.admitted(error, 255, invariant="NoUnresolvedReplay"))

    def test_configs_enforce_bounds_safe_flags_and_exact_properties(self):
        configs = {f"{name}.cfg": check.expected_config(name).encode()
                   for name in check.CASES}
        check.validate_configs(configs)
        for name, old, new in (
            ("safe", "MaxOperations = 2", "MaxOperations = 1"),
            ("safe", "INVARIANT NoUnresolvedReplay\n", ""),
            ("witness-applied", "AllowAckSuccess = FALSE", "AllowAckSuccess = TRUE"),
            ("replay-after-expiry", "AllowExpiredReplay = TRUE", "AllowExpiredReplay = FALSE"),
        ):
            changed = dict(configs)
            changed[f"{name}.cfg"] = changed[f"{name}.cfg"].replace(old.encode(), new.encode())
            with self.assertRaises(ValueError):
                check.validate_configs(changed)

    def test_checked_in_configs_match_runner_contract(self):
        source = Path(__file__).resolve().parent
        check.validate_configs({f"{name}.cfg": (source / f"{name}.cfg").read_bytes()
                                for name in check.CASES})


@unittest.skipUnless(os.name == "posix" and hasattr(os, "WNOWAIT"), "Unix runner")
class OwnedProcessTests(unittest.TestCase):
    def run_python(self, source, timeout=2, limit=32768):
        with tempfile.TemporaryDirectory(prefix="gobstopper-watch-check-") as directory:
            return check.run_owned([sys.executable, "-c", source], Path(directory),
                                   dict(os.environ), timeout, limit)

    def test_preserves_real_exit_code_and_output(self):
        code, log, timeout, limit = self.run_python("print('complete'); raise SystemExit(12)")
        self.assertEqual((code, log, timeout, limit), (12, "complete\n", False, False))

    def test_timeout_is_bounded_and_reported(self):
        started = time.monotonic()
        code, _, timeout, limit = self.run_python("import time; time.sleep(10)", timeout=0.1)
        self.assertTrue(timeout)
        self.assertFalse(limit)
        self.assertNotEqual(code, 0)
        self.assertLess(time.monotonic() - started, 2)

    def test_output_limit_cannot_be_passed_as_complete(self):
        _, log, timeout, limit = self.run_python("print('x' * 65536)", limit=1024)
        self.assertFalse(timeout)
        self.assertTrue(limit)
        self.assertEqual(len(log), 1024)

    def test_inherited_pipe_does_not_wait_for_background_child(self):
        started = time.monotonic()
        source = ("import subprocess,sys; "
                  "subprocess.Popen([sys.executable,'-c','import time; time.sleep(10)']); "
                  "print('leader done')")
        code, log, timeout, limit = self.run_python(source)
        self.assertEqual((code, log, timeout, limit), (0, "leader done\n", False, False))
        self.assertLess(time.monotonic() - started, 2)


if __name__ == "__main__":
    unittest.main()

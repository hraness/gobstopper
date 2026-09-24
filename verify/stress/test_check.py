#!/usr/bin/env python3
"""Negative stress-admission fixtures; these do not launch test processes."""
import json
from pathlib import Path
import unittest

import check


def document():
    return json.loads((Path(__file__).parent / "suites.json").read_text())


def log(suite):
    if suite["name"] == "monitor":
        out = "\n".join(f"{name.split('.')[-1]} (__main__.{name}) ... ok" for name in suite["tests"])
        return out + f"\n\nRan {len(suite['tests'])} tests in 0.100s\n\nOK\n"
    out = "\n".join(f"test {name} ... ok" for name in suite["tests"])
    if suite["name"] == "sequence":
        out = out.replace(" ... ok", " ... sequence receipt: "
            f"seed={check.SEED}, steps=64, corruption_recoveries=16, peak_files=402, peak_bytes=334635, elapsed_ms=28000\nok")
    return out + f"\n\ntest result: ok. {len(suite['tests'])} passed; 0 failed; 0 ignored; 0 measured; 17 filtered out; finished in 0.1s\n"


class AdmissionTests(unittest.TestCase):
    def test_exact_inventory_and_budget(self):
        suites = check.admitted_inventory(document())
        self.assertEqual(sum(len(s["tests"]) for s in suites), 159)
        self.assertEqual(check.TOTAL_SECONDS, 900)
        self.assertEqual(next(s for s in suites if s["name"] == "watch")["seconds"], 240)
        for mutate in (lambda d:d["suites"].pop(),
                       lambda d:d["suites"][0]["argv"].append("--ignored"),
                       lambda d:d["suites"][0].update(seconds=999999),
                       lambda d:d["suites"][0]["tests"].clear()):
            changed = document()
            mutate(changed)
            with self.assertRaises(ValueError):
                check.admitted_inventory(changed)
        changed = document()
        next(s for s in changed["suites"] if s["name"] == "watch")["seconds"] = 241
        with self.assertRaises(ValueError):
            check.admitted_inventory(changed)

    def test_every_named_test_must_execute_once_and_pass(self):
        for suite in check.admitted_inventory(document()):
            good = log(suite)
            self.assertTrue(check.admit_tests(suite,0,good,False,False)["passed"])
            for changed in (good.replace(suite["tests"][0],"unreviewed"),
                            good.replace(" ... ok"," ... ignored",1),
                            good + good.splitlines()[0] + "\n",
                            good.replace("0 failed","1 failed"),good + "error[E0000]: compile error"):
                if changed != good:
                    self.assertFalse(check.admit_tests(suite,0,changed,False,False)["passed"])
            self.assertFalse(check.admit_tests(suite,0,good,True,False)["passed"])
            self.assertFalse(check.admit_tests(suite,0,good,False,True)["passed"])
            self.assertFalse(check.admit_tests(suite,101,good,False,False)["passed"])

    def test_python_skips_and_malformed_summaries_never_count_as_success(self):
        suite = check.admitted_inventory(document())[-1]
        good = log(suite)
        for changed in (good.replace(" ... ok", " ... skipped 'unsupported'", 1),
                        good.replace("\nOK\n", "\nOK (skipped=1)\n"),
                        good.replace(f"Ran {len(suite['tests'])} tests", "Ran 0 tests"),
                        good + "\nERROR: hidden failure\n", good + "\nRan 0 tests in 0.000s\n",
                        good.replace("(__main__.", "(other.", 1)):
            self.assertFalse(check.admit_tests(suite,0,changed,False,False)["passed"])

    def test_sequence_requires_exact_seed_and_nonvacuous_bounded_metrics(self):
        suite = check.admitted_inventory(document())[0]
        good = log(suite)
        for old,new in ((f"seed={check.SEED}","seed=1"),("steps=64","steps=1"),
                        ("corruption_recoveries=16","corruption_recoveries=0"),
                        ("peak_files=402","peak_files=1201"),("peak_bytes=334635","peak_bytes=16777217"),
                        ("elapsed_ms=28000","elapsed_ms=90000"),("sequence receipt:","missing receipt:")):
            self.assertFalse(check.admit_tests(suite,0,good.replace(old,new),False,False)["passed"])

    def test_source_bound_sequence_constants_match_reviewed_budgets(self):
        source = (check.ROOT / "crates/gobstopper-adapters/tests/sequence.rs").read_text()
        for constant in ("const STEPS: usize = 64;", "const MAX_BYTES: u64 = 16 * 1024 * 1024;",
                         "const MAX_FILES: u64 = 1200;", "Duration::from_secs(90)"):
            self.assertIn(constant,source)


if __name__ == "__main__":
    unittest.main()

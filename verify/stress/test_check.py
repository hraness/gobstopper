#!/usr/bin/env python3
"""Negative stress-admission fixtures; these do not launch test processes."""
import json
from pathlib import Path
import threading
import unittest
from unittest.mock import patch

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
        self.assertEqual(sum(len(s["tests"]) for s in suites), 157)
        self.assertEqual(check.TOTAL_SECONDS, 900)
        self.assertEqual(next(s for s in suites if s["name"] == "watch")["seconds"], 360)
        self.assertEqual(check.SUITE_WORKERS, 3)
        # Only the watch suite runs two libtest threads; every other suite is serial.
        threads = {s["name"]: s["argv"][-1] for s in suites if s["argv"][0] == "cargo"}
        self.assertEqual(threads.pop("watch"), "--test-threads=2")
        self.assertEqual(set(threads.values()), {"--test-threads=1"})
        for mutate in (lambda d:d["suites"].pop(),
                       lambda d:d["suites"][0]["argv"].append("--ignored"),
                       lambda d:d["suites"][0]["argv"].__setitem__(-1, "--test-threads=4"),
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


class SchedulingTests(unittest.TestCase):
    def test_sequence_finishes_before_any_other_suite_and_inventory_runs_once(self):
        suites = check.admitted_inventory(document())
        other_started = threading.Event()
        sequence_finished = threading.Event()
        calls = []
        lock = threading.Lock()

        def run(suite):
            name = suite["name"]
            with lock:
                calls.append(name)
            if name == "sequence":
                # Give an incorrectly concurrent scheduler a chance to enter
                # another callback while the sequence is explicitly held.
                self.assertFalse(other_started.wait(0.05))
                sequence_finished.set()
            else:
                other_started.set()
                self.assertTrue(sequence_finished.is_set())
            return {"case": name, "passed": True}

        results, errors = check.run_suites(suites, run, threading.Event())
        self.assertEqual(errors, [])
        self.assertEqual(calls[0], "sequence")
        self.assertCountEqual(calls, [suite["name"] for suite in suites])
        self.assertEqual([row["case"] for row in results], [suite["name"] for suite in suites])

    def test_pool_preserves_cli_serialization_and_three_worker_limit(self):
        suites = check.admitted_inventory(document())
        # Hold each worker's first task until all three have actually entered;
        # this proves useful parallelism as well as the upper bound.
        first_wave = threading.Barrier(3, timeout=2)
        lock = threading.Lock()
        calls = []
        cli_calls = []
        active = cli_active = peak = cli_peak = 0

        def run(suite):
            nonlocal active, cli_active, peak, cli_peak
            name = suite["name"]
            if name == "sequence":
                return {"case": name, "passed": True}
            is_cli = check.SUITES[name][0] == check.CLI_LANE_PACKAGE
            with lock:
                calls.append(name)
                first = len(calls) <= 3
                active += 1
                cli_active += is_cli
                peak = max(peak, active)
                cli_peak = max(cli_peak, cli_active)
                if is_cli:
                    cli_calls.append(name)
            try:
                if first:
                    first_wave.wait()
                return {"case": name, "passed": True}
            finally:
                with lock:
                    active -= 1
                    cli_active -= is_cli

        results, errors = check.run_suites(suites, run, threading.Event())
        self.assertEqual(errors, [])
        self.assertEqual(len(results), len(suites))
        self.assertEqual(peak, 3)
        self.assertEqual(cli_peak, 1)
        self.assertEqual(cli_calls, [suite["name"] for suite in suites
                                    if check.SUITES[suite["name"]][0] == check.CLI_LANE_PACKAGE])
        self.assertCountEqual(calls, [suite["name"] for suite in suites if suite["name"] != "sequence"])

    def test_failed_or_missing_sequence_never_starts_pool(self):
        suites = check.admitted_inventory(document())
        for result in ({"case": "sequence", "passed": False}, None):
            with self.subTest(result=result):
                failed = threading.Event()
                calls = []

                def run(suite):
                    calls.append(suite["name"])
                    return result

                with patch.object(check, "ThreadPoolExecutor", side_effect=AssertionError("pool started")):
                    results, errors = check.run_suites(suites, run, failed)
                self.assertTrue(failed.is_set())
                self.assertEqual(calls, ["sequence"])
                self.assertEqual(results, [] if result is None else [result])
                self.assertEqual(errors, [])

    def test_sequence_error_propagates_without_starting_pool(self):
        suites = check.admitted_inventory(document())
        calls = []

        def run(suite):
            calls.append(suite["name"])
            raise TimeoutError("aggregate deadline exceeded")

        with patch.object(check, "ThreadPoolExecutor", side_effect=AssertionError("pool started")):
            with self.assertRaisesRegex(TimeoutError, "aggregate deadline exceeded"):
                check.run_suites(suites, run, threading.Event())
        self.assertEqual(calls, ["sequence"])


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Fail-closed admission tests without invoking Lean or Cargo."""
import copy
from pathlib import Path
import unittest

import check


class AdmissionTests(unittest.TestCase):
    def test_compiler_identity_refuses_relative_diagnostic_or_incomplete_output(self):
        self.assertEqual(check.rust_tools(0, "/toolchain\n", False, False),
                         (Path("/toolchain/bin/cargo"), Path("/toolchain/bin/rustc")))
        for code, log, timeout, capped in ((1,"/toolchain\n",False,False),
                                         (0,"/toolchain\n",True,False),
                                         (0,"/toolchain\n",False,True),
                                         (0,"relative\n",False,False),
                                         (0,"/toolchain\nwarning: diagnostic\n",False,False),
                                         (0,"",False,False)):
            with self.assertRaises(ValueError):
                check.rust_tools(code,log,timeout,capped)

    def test_source_inventory_and_escape_tripwires(self):
        files = {p.name: p.read_bytes() for p in Path(__file__).parent.iterdir() if p.is_file()}
        check.source_hygiene(files)
        for escape in (b"\naxiom fabricated : False\n", b"\nexample : False := by sorry\n",
                       b"\nset_option debug.skipKernelTC true\n", b"\nunsafe def bypass := 1\n",
                       b"\nexample : True := by native_decide\n"):
            mutated = files | {"Transcript.lean": files["Transcript.lean"] + escape}
            with self.assertRaises(ValueError):
                check.source_hygiene(mutated)
        with self.assertRaises(ValueError):
            check.source_hygiene(files | {"Unreviewed.lean": b""})
        with self.assertRaises(ValueError):
            check.source_hygiene(files | {"Audit.lean": b"import Transcript\n"})

    def test_axiom_inventory_is_exact_and_closed(self):
        good = "\n".join(f"'Transcript.{name}' depends on axioms: [propext, Quot.sound]"
                         for name in sorted(check.THEOREMS))
        self.assertEqual(set(check.admit_axioms(good)), check.THEOREMS)
        for changed in (good.replace("propext", "sorryAx"), good.replace("Quot.sound", "invented"),
                        good + "\n" + good.splitlines()[0], "\n".join(good.splitlines()[1:]),
                        good + "\nwarning: declaration uses an unchecked proof"):
            with self.assertRaises(ValueError):
                check.admit_axioms(changed)

    def test_vectors_have_positive_negative_and_digest_witnesses(self):
        raw = (Path(__file__).parent / "vectors.json").read_bytes()
        counts = check.admit_vectors(raw)
        self.assertEqual(counts["steps_per_provider"], 23)
        import json
        document = json.loads(raw)
        changed = copy.deepcopy(document)
        changed["cases"].pop()
        with self.assertRaises(ValueError):
            check.admit_vectors(json.dumps(changed).encode())
        changed = copy.deepcopy(document)
        changed["cases"][0]["steps"][0]["admitted"] = False
        with self.assertRaises(ValueError):
            check.admit_vectors(json.dumps(changed).encode())

    def test_success_requires_exact_executed_test(self):
        good = ("test lean_vectors_match_production ... ok\n"
                "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n")
        self.assertTrue(check.admit_cargo(0, good, False, False))
        for code, log, timeout, capped in ((1,good,False,False),(0,good,True,False),(0,good,False,True),
                                         (0,good.replace("1 passed", "0 passed"),False,False),
                                         (0,good.replace(" ... ok", " ... ignored"),False,False)):
            self.assertFalse(check.admit_cargo(code,log,timeout,capped))

    def test_mutants_require_the_exact_counterexample_not_build_or_random_failure(self):
        for mutant, left, right in (("oracle",0,1),("rust",3,0)):
            good = ("test lean_vectors_match_production ... FAILED\n"
                    "thread 'lean_vectors_match_production' panicked at tests/lean_correspondence.rs:1:1:\n"
                    "assertion `left == right` failed: LEAN_CORRESPONDENCE_PAYLOAD Codex/first\n"
                    f"  left: {left}\n right: {right}\n"
                    "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n")
            self.assertTrue(check.admit_cargo(101,good,False,False,mutant))
            for changed in (good.replace("PAYLOAD", "ADMISSION"),good.replace(f"left: {left}","left: 999"),
                            good + "error[E0123]: broken compilation", good + "thread panicked at unrelated:1:1"):
                self.assertFalse(check.admit_cargo(101,changed,False,False,mutant))
            self.assertFalse(check.admit_cargo(1,good,False,False,mutant))


if __name__ == "__main__":
    unittest.main()

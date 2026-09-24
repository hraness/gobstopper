#!/usr/bin/env python3
"""Check assurance inventory structure and surface coverage, without claiming proof."""

import argparse
import ast
import hashlib
import json
import math
from pathlib import Path
import re
import sys

FILES = {
    "ledger": "gobstopper.assurance.v1",
    "effects": "gobstopper.effects.v1",
    "claims": "gobstopper.claims.v1",
    "codeql-triage": "gobstopper.codeql-triage.v1",
}
STATUSES = {"specified", "source_reviewed", "specification", "bounded_check", "historical_observation"}
ALERT_IDS = {8, 12, 13, 14, 15, 26, 27, 30, 31, 33, 34, 35, 36, 39, 40, 41, 42, 62, 63, 68, 71}
BASELINE = "ffc71480564f0d0077f27e59a04df3174d5335ef"
WRITE_MODES = {"emit", "in_memory", "trusted_code", "spawn", "create", "create_delete", "replace", "create_replace", "permissions", "append", "delete", "sql_transaction", "native", "rename"}


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load_documents(root):
    return {
        name: json.loads((root / "docs/assurance" / f"{name}.json").read_text(), object_pairs_hook=unique_object)
        for name in FILES
    }


def enum_variants(text, name):
    match = re.search(r"\benum " + re.escape(name) + r"\s*\{([\s\S]*?)\n\}", text)
    if not match:
        raise ValueError(f"missing source enum {name}")
    return set(re.findall(r"^    ([A-Z][A-Za-z0-9_]*)\s*(?:\{|,)", match.group(1), re.M))


def production_text(text):
    # Conservative declaration inventory, not a Rust parser/effect analyzer.
    return re.split(r"\n#\[cfg\(test\)\]\s*\nmod tests\b", text, maxsplit=1)[0]


def source_callables(root):
    result = set()
    for package in ("gobstopper-adapters", "gobstopper-cli", "gobstopper-core"):
        for path in (root / "crates" / package / "src").rglob("*.rs"):
            names = re.findall(
                r'\bpub(?:\([^)]*\))?\s+(?:(?:async|const|unsafe)\s+|extern(?:\s+"[^"]*")?\s+)*fn\s+([a-zA-Z_][a-zA-Z_0-9]*)',
                production_text(path.read_text()),
            )
            result.update((str(path.relative_to(root)), name) for name in names)
    return result


def source_scripts(root):
    # Include proof setup/download helpers as well as check.py entry points.
    return {str(path.relative_to(root))
            for directory in ("scripts", "verify")
            for path in (root / directory).rglob("*.py")
            if not path.name.startswith("test_") and path.name != "__init__.py"}


def literal_contract(path, names):
    """Read literals and bounded integer products without executing a runner."""
    def value(node):
        if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Mult):
            left, right = value(node.left), value(node.right)
            if type(left) is not int or type(right) is not int or not 0 <= left <= 2**64 or not 0 <= right <= 2**64:
                raise ValueError("unreviewed runner constant arithmetic")
            result = left * right
            if result > 2**64:
                raise ValueError("unbounded runner constant arithmetic")
            return result
        return ast.literal_eval(node)

    wanted = set(names)
    result = {}
    for node in ast.parse(path.read_text()).body:
        if isinstance(node, ast.Assign) and len(node.targets) == 1:
            target = node.targets[0]
            if isinstance(target, ast.Name) and target.id in wanted:
                if target.id in result:
                    raise ValueError(f"duplicate runner contract: {target.id}")
                result[target.id] = value(node.value)
    if set(result) != wanted:
        raise ValueError("missing literal runner contract")
    return result


def receipt_sources(root, family, cases):
    """Mirror the reviewed runners' input inventories, not receipt-chosen subsets.

    Runner source is itself included. A change to its inventory algorithm needs
    corresponding review here; this deliberately does not execute proof code.
    """
    here = root / "verify" / family
    runner = root / "verify/watch/check.py"
    if family in {"vault", "watch"}:
        models = ["Vault.tla", "Publication.tla"] if family == "vault" else ["Watch.tla"]
        paths = [here / name for name in ["check.py", "test_check.py", "README.md", *models]]
        paths += [here / f"{name}.cfg" for name in cases]
        paths.append(runner)
    else:
        paths = [root / "Cargo.toml", root / "Cargo.lock", runner]
        if family == "core":
            paths += list(here.glob("*.py")) + [here / "README.md"]
            paths += [root / "crates/gobstopper-core/Cargo.toml"]
            paths += list((root / "crates/gobstopper-core/src").rglob("*.rs"))
            paths += [root / name for name in (
                "crates/gobstopper-cli/src/config.rs", "crates/gobstopper-adapters/src/codex.rs",
                "crates/gobstopper-adapters/src/payload.rs")]
        else:
            paths += [path for path in here.iterdir() if path.is_file() and not path.name.startswith(".")]
            if family == "transcript":
                paths += [root / "crates/gobstopper-cli/Cargo.toml", root / "verify/tools.lock.json",
                          root / "crates/gobstopper-adapters/tests/lean_correspondence.rs"]
                for crate in ("gobstopper-core", "gobstopper-adapters"):
                    base = root / "crates" / crate
                    paths += [base / "Cargo.toml", *list((base / "src").rglob("*.rs"))]
            elif family == "stress":
                paths += [root / "scripts/monitor.py", root / "scripts/test_monitor.py"]
                for crate in (root / "crates").iterdir():
                    if crate.is_dir():
                        paths.append(crate / "Cargo.toml")
                        for directory in ("src", "tests"):
                            paths += [path for path in (crate / directory).rglob("*") if path.is_file()]
            else:
                raise ValueError("unsupported receipt family")
    return {str(path.relative_to(root)) for path in paths}


def validate_receipt_contract(root, name, receipt, require):
    """Check normalized evidence completeness and contradictions, not log attestation."""
    families = {f"E-{family.upper()}-CURRENT": family
                for family in ("vault", "watch", "core", "transcript", "stress")}
    if name not in families:
        require(False, f"receipt {name}: unreviewed receipt family")
        return
    family = families[name]
    label = f"receipt {name}"
    require("error" not in receipt, f"{label}: contradictory receipt error")
    checker = root / "verify" / family / "check.py"
    cases = receipt.get("results", [])
    if not isinstance(cases, list) or not all(isinstance(case, dict) for case in cases):
        return
    rows = {case.get("case"): case for case in cases}
    sources = receipt.get("source_sha256", {})
    tools = receipt.get("tool_sha256", {})
    if not isinstance(sources, dict) or not isinstance(tools, dict):
        return
    for row in cases:
        require(row.get("timeout") is False and row.get("log_limit") is False,
                f"{label}: incomplete resource-limited case")
        require(isinstance(row.get("log_sha256"), str)
                and re.fullmatch(r"[0-9a-f]{64}", row["log_sha256"]) is not None,
                f"{label}: missing case log identity")

    def integer(value, minimum=0):
        return type(value) is int and value >= minimum

    def digest(value):
        return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None

    def elapsed(value, maximum):
        return (type(value) in {int, float} and 0 <= value <= maximum
                and (type(value) is int or math.isfinite(value)))

    def mutation(value, path, old, new):
        raw = (root / path).read_bytes()
        require(isinstance(value, dict) and value.get("old") == old and value.get("new") == new,
                f"{label}: wrong intended mutation")
        require(raw.count(old.encode()) == 1, f"{label}: mutation target is not unique")
        if isinstance(value, dict):
            require(value.get("source_sha256") == hashlib.sha256(raw.replace(old.encode(), new.encode())).hexdigest(),
                    f"{label}: mutant source identity mismatch")

    if family in {"vault", "watch"}:
        contract = literal_contract(checker, ("CASES", "TLC_SHA256"))
        expected = contract["CASES"]
        require(set(rows) == set(expected), f"{label}: receipt case inventory differs from runner")
        require(set(tools) == {"java", "tlc.jar"}, f"{label}: receipt tool inventory differs from runner")
        pins = json.loads((root / "verify/tools.lock.json").read_text(), object_pairs_hook=unique_object)
        require(tools.get("tlc.jar") == contract["TLC_SHA256"] == pins["tools"]["tlc"]["any"]["sha256"],
                f"{label}: TLC tool differs from reviewed pin")
        for case, value in expected.items():
            row = rows.get(case, {})
            invariant = value[2] if family == "watch" else value
            require(row.get("expected_invariant_violation") == invariant,
                    f"{label}: wrong intended invariant for {case}")
            require(type(row.get("exit_code")) is int and row["exit_code"] == (12 if invariant else 0),
                    f"{label}: wrong result exit for {case}")
            states = row.get("states", {})
            valid = (isinstance(states, dict) and all(integer(states.get(key)) for key in ("generated", "distinct", "queued")))
            require(valid and states["generated"] >= states["distinct"] >= states["queued"]
                    and states["distinct"] > (1 if invariant else 100)
                    and (invariant is not None or states["queued"] == 0),
                    f"{label}: incomplete state exploration for {case}")
            if family == "watch":
                require((row.get("kind"), row.get("mutation")) == value[:2],
                        f"{label}: wrong case role for {case}")
    elif family == "core":
        expected = {"production", "boundary-mutant"}
        contract = literal_contract(checker, ("KANI_VERSION", "CBMC_VERSION", "RUSTC_VERSION", "UNWINDS",
                                              "BOUNDARY_ASSERTION", "MUTANT_OLD", "MUTANT_NEW"))
        require(set(rows) == expected, f"{label}: receipt case inventory differs from runner")
        require(set(tools) == {"cargo-kani", "kani-driver", "kani-compiler", "cbmc", "goto-instrument",
                               "goto-cc", "rustc-version", "rust-toolchain-version"},
                f"{label}: receipt tool inventory differs from runner")
        require(receipt.get("versions") == {"kani": contract["KANI_VERSION"], "cbmc": contract["CBMC_VERSION"],
                                            "rustc": contract["RUSTC_VERSION"]}, f"{label}: wrong proof versions")
        require(receipt.get("unwind_bounds") == contract["UNWINDS"], f"{label}: wrong unwind bounds")
        for case in expected:
            row = rows.get(case, {})
            mutant = case == "boundary-mutant"
            code = row.get("exit_code")
            require(integer(code) and (code != 0 if mutant else code == 0), f"{label}: wrong result exit for {case}")
            require(row.get("failed_labels") == ([contract["BOUNDARY_ASSERTION"]] if mutant else []),
                    f"{label}: wrong intended assertion for {case}")
            counts = row.get("counts", {})
            # These are the reviewed output counts for the pinned Kani/compiler
            # and current source. A changed proof inventory needs a new receipt
            # and review here, not merely a nonzero assertion total.
            expected_counts = dict(zip(
                ("assertions", "covers", "unwind_checks", "unreachable_safety_checks", "unsupported_paths_excluded"),
                (2, 3, 0, 0, 0) if mutant else (159, 46, 3, 30, 11),
            ))
            require(isinstance(counts, dict) and counts == expected_counts
                    and all(integer(value) for value in counts.values()),
                    f"{label}: incomplete proof counts for {case}")
            require(isinstance(row.get("report_sha256"), str) and re.fullmatch(r"[0-9a-f]{64}", row["report_sha256"]),
                    f"{label}: missing proof report identity")
        mutation(receipt.get("mutation"), "crates/gobstopper-core/src/admission.rs", contract["MUTANT_OLD"], contract["MUTANT_NEW"])
    elif family == "transcript":
        expected = {"rust-sysroot", "lean-version", "lake-version", "rust-version", "cargo-version", "lean-build", "axioms",
                    "kernel-replay", "vectors", "rust-correspondence", "oracle-mutant-build", "oracle-mutant-vectors",
                    "oracle-mutant-correspondence", "rust-mutant-correspondence"}
        require(set(rows) == expected, f"{label}: receipt case inventory differs from runner")
        require(set(tools) == {"lake", "lean", "leanchecker", "cargo", "rustc", "cargo-dispatcher",
                               "rustc-dispatcher", "python"},
                f"{label}: receipt tool inventory differs from runner")
        contract = literal_contract(checker, ("THEOREMS", "ALLOWED_AXIOMS", "ORACLE_OLD", "ORACLE_NEW", "RUST_OLD", "RUST_NEW"))
        for case in expected:
            code = rows.get(case, {}).get("exit_code")
            require(type(code) is int and code == (101 if case in {"oracle-mutant-correspondence", "rust-mutant-correspondence"} else 0),
                    f"{label}: wrong result exit for {case}")
        axioms = receipt.get("axioms", {})
        require(isinstance(axioms, dict) and set(axioms) == contract["THEOREMS"]
                and all(isinstance(used, list) and set(used) <= contract["ALLOWED_AXIOMS"] for used in axioms.values()),
                f"{label}: incomplete or unsupported axiom audit")
        require(receipt.get("vectors") == {"cases": 20, "steps_per_provider": 23, "accepted_per_provider": 12,
                                            "refused_per_provider": 11, "digests_per_provider": 3},
                f"{label}: incomplete vector correspondence")
        mutations = receipt.get("mutations", {})
        require(isinstance(mutations, dict) and set(mutations) == {"oracle", "rust"},
                f"{label}: incomplete intended mutation inventory")
        if not isinstance(mutations, dict):
            mutations = {}
        mutation(mutations.get("oracle"), "verify/transcript/Vectors.lean", contract["ORACLE_OLD"], contract["ORACLE_NEW"])
        mutation(mutations.get("rust"), "crates/gobstopper-adapters/src/codex.rs", contract["RUST_OLD"], contract["RUST_NEW"])
        require(isinstance(mutations.get("rust"), dict)
                and mutations["rust"].get("uncompiled_cli_metadata_target") is True,
                f"{label}: missing isolated workspace metadata declaration")
    else:
        suites = json.loads((root / "verify/stress/suites.json").read_text(), object_pairs_hook=unique_object)["suites"]
        contract = literal_contract(checker, ("TOTAL_SECONDS", "MAX_LOG_BYTES", "MAX_CHILD_RSS_BYTES", "SEED", "SUITES"))
        expected = {suite["name"] for suite in suites}
        require(len(suites) == len(expected) and expected == set(contract["SUITES"]),
                f"{label}: stress suite inventory differs from runner")
        require(set(rows) == expected, f"{label}: receipt case inventory differs from runner")
        require(set(tools) == {"cargo", "rustc", "cargo-dispatcher", "rustc-dispatcher", "python"},
                f"{label}: receipt tool inventory differs from runner")
        require(receipt.get("platform") in {"linux", "darwin"}, f"{label}: unsupported resource accounting platform")
        identities = receipt.get("identity_log_sha256", {})
        require(isinstance(identities, dict)
                and set(identities) == {"rust-sysroot.log", "cargo-version.log", "rust-version.log"}
                and all(digest(value) for value in identities.values()),
                f"{label}: incomplete tool identity logs")
        bounds = receipt.get("raw_bounds", {})
        expected_bounds = {
            "total_seconds": contract["TOTAL_SECONDS"], "max_log_bytes_per_command": contract["MAX_LOG_BYTES"],
            "cargo_jobs": 2, "test_threads": 1, "max_observed_single_child_rss_bytes": contract["MAX_CHILD_RSS_BYTES"],
            "sequence_steps": 64, "sequence_corruption_recoveries": 16, "sequence_post_step_file_limit": 1200,
            "sequence_post_step_bytes_limit": 16 * 1024 * 1024, "sequence_elapsed_ms_limit": 90_000,
        }
        require(isinstance(bounds, dict) and bounds == expected_bounds
                and all(integer(value, 1) for value in bounds.values()),
                f"{label}: stress resource bounds differ from runner")
        require(elapsed(receipt.get("elapsed_seconds"), contract["TOTAL_SECONDS"]),
                f"{label}: invalid aggregate elapsed bound")
        suite_elapsed = 0
        for suite in suites:
            row = rows.get(suite["name"], {})
            package, targets, count, seconds = contract["SUITES"].get(suite["name"], (None, [], 0, 0))
            argv = (["cargo", "test", "-p", package, "--locked", *targets, "--", "--nocapture", "--test-threads=1"]
                    if package else ["python3", *targets])
            require(suite.get("argv") == argv and suite.get("seconds") == seconds
                    and len(suite["tests"]) == len(set(suite["tests"])) == count,
                    f"{label}: stress suite command or bounds differ from runner")
            require(type(row.get("exit_code")) is int and row["exit_code"] == 0, f"{label}: wrong result exit")
            require(row.get("passed_tests") == sorted(suite["tests"]), f"{label}: incomplete named tests")
            duration = row.get("elapsed_seconds")
            require(elapsed(duration, suite["seconds"]),
                    f"{label}: invalid elapsed bound")
            if elapsed(duration, suite["seconds"]):
                suite_elapsed += duration
            require(integer(row.get("largest_reaped_child_rss_bytes"))
                    and row["largest_reaped_child_rss_bytes"] <= contract["MAX_CHILD_RSS_BYTES"],
                    f"{label}: invalid observed child memory bound")
        require(elapsed(receipt.get("elapsed_seconds"), contract["TOTAL_SECONDS"])
                and receipt["elapsed_seconds"] >= suite_elapsed,
                f"{label}: aggregate elapsed contradicts suite durations")
        metrics = rows.get("sequence", {}).get("sequence_metrics", {})
        require(isinstance(metrics, dict) and metrics.get("seed") == contract["SEED"]
                and metrics.get("steps") == 64 and metrics.get("corruption_recoveries") == 16
                and integer(metrics.get("peak_files"), 1) and metrics["peak_files"] <= 1200
                and integer(metrics.get("peak_bytes"), 1) and metrics["peak_bytes"] <= 16 * 1024 * 1024
                and integer(metrics.get("elapsed_ms")) and metrics["elapsed_ms"] < 90_000,
                f"{label}: incomplete sequence bounds")
    require(set(sources) == receipt_sources(root, family, expected),
            f"{label}: receipt source inventory differs from runner")


def validate(root, documents):
    """Return all structural errors; no test, network or provider execution."""
    errors = []

    def require(condition, message):
        if not condition:
            errors.append(message)

    def text(value):
        return isinstance(value, str) and bool(value.strip())

    def nonempty(row, fields, label):
        for field in fields:
            value = row.get(field)
            require((text(value) or isinstance(value, list) and bool(value)), f"{label}: missing {field}")

    def index(rows, key, label):
        result = {}
        require(isinstance(rows, list), f"{label}: expected list")
        for row in rows if isinstance(rows, list) else []:
            require(isinstance(row, dict), f"{label}: expected object")
            if not isinstance(row, dict):
                continue
            value = row.get(key)
            require(text(value) or isinstance(value, int), f"{label}: missing {key}")
            require(value not in result, f"{label}: duplicate {value}")
            result[value] = row
        return result

    def refs(values, allowed, label):
        require(isinstance(values, list) and bool(values), f"{label}: empty references")
        for value in values if isinstance(values, list) else []:
            require(value in allowed, f"{label}: unknown reference {value}")

    def source_ref(ref, label):
        if "url" in ref:
            require(isinstance(ref["url"], str) and ref["url"].startswith("https://"), f"{label}: invalid evidence URL")
            return
        path = ref.get("path", "")
        require(text(path) and not Path(path).is_absolute() and ".." not in Path(path).parts, f"{label}: invalid source path")
        if not text(path) or Path(path).is_absolute() or ".." in Path(path).parts:
            return
        candidate = root / path
        require(candidate.is_file(), f"{label}: missing source {path}")
        if candidate.is_file() and "anchor" in ref:
            require(text(ref["anchor"]) and ref["anchor"] in candidate.read_text(), f"{label}: absent anchor in {path}: {ref['anchor']}")

    def next_gate(row, label):
        require(re.fullmatch(r"C(?:[1-9]|1[0-5])", str(row.get("next_gate", ""))) is not None, f"{label}: invalid next gate")

    for name, schema in FILES.items():
        document = documents.get(name, {})
        require(document.get("schema") == schema, f"{name}: unsupported schema")
        require(document.get("baseline_revision") == BASELINE, f"{name}: baseline identity changed without validator review")
    ledger = documents.get("ledger", {})
    states = index(ledger.get("states", []), "id", "states")
    assumptions = index(ledger.get("assumptions", []), "id", "assumptions")
    evidence = index(ledger.get("evidence", []), "id", "evidence")
    invariants = index(ledger.get("invariants", []), "id", "invariants")
    for name, row in states.items():
        nonempty(row, ["meaning", "owner", "boundary"], f"state {name}")
    for name, row in assumptions.items():
        nonempty(row, ["statement"], f"assumption {name}")
        next_gate(row, f"assumption {name}")
    for name, row in evidence.items():
        nonempty(row, ["kind", "revision", "scope", "bounds", "references", "exclusions"], f"evidence {name}")
        for ref in row.get("references", []):
            source_ref(ref, f"evidence {name}")
        if row.get("kind") == "bounded_model_receipt":
            require(re.fullmatch(r"[0-9a-f]{40}", row.get("revision", "")) is not None, f"evidence {name}: model receipt needs exact revision")
            require(re.fullmatch(r"[0-9a-f]{64}", row.get("tool", {}).get("sha256", "")) is not None, f"evidence {name}: missing tool pin")
            require(bool(row.get("inputs")), f"evidence {name}: missing model inputs")
            for item in row.get("inputs", []):
                source_ref(item, f"evidence {name}")
                require(re.fullmatch(r"[0-9a-f]{64}", item.get("sha256", "")) is not None, f"evidence {name}: missing historical input digest")
        if row.get("kind") == "verification_receipt":
            ref = row.get("receipt", {})
            source_ref(ref, f"evidence {name}")
            relative = ref.get("path", "")
            require(text(relative) and "url" not in ref, f"evidence {name}: receipt must be a local pinned file")
            if not text(relative) or Path(relative).is_absolute() or ".." in Path(relative).parts:
                continue
            candidate = root / relative
            if not candidate.is_file():
                continue
            raw = candidate.read_bytes()
            require(hashlib.sha256(raw).hexdigest() == ref.get("sha256"), f"evidence {name}: receipt digest mismatch")
            receipt = json.loads(raw, object_pairs_hook=unique_object)
            require(receipt.get("schema") == "gobstopper.assurance-receipt.v1", f"evidence {name}: invalid receipt schema")
            require(receipt.get("passed") is True and receipt.get("inputs_unchanged") is True, f"evidence {name}: receipt did not pass unchanged")
            require(re.fullmatch(r"[0-9a-f]{64}", receipt.get("raw_receipt_sha256", "")) is not None, f"evidence {name}: missing raw receipt identity")
            nonempty(receipt, ["scope", "bounds", "exclusions", "results"], f"receipt {name}")
            for field in ("source_sha256", "tool_sha256"):
                require(isinstance(receipt.get(field), dict) and bool(receipt.get(field)),
                        f"receipt {name}: missing {field}")
            cases = receipt.get("results", [])
            require(isinstance(cases, list) and bool(cases)
                    and all(isinstance(case, dict) and case.get("passed") is True
                            and text(case.get("case")) for case in cases),
                    f"evidence {name}: incomplete or failed receipt case")
            if isinstance(cases, list) and all(isinstance(case, dict) for case in cases):
                require(len({case.get("case") for case in cases}) == len(cases),
                        f"evidence {name}: duplicate receipt case")
            for source, expected in receipt.get("source_sha256", {}).items():
                source_ref({"path": source}, f"receipt {name}")
                if Path(source).is_absolute() or ".." in Path(source).parts or not (root / source).is_file():
                    continue
                require(hashlib.sha256((root / source).read_bytes()).hexdigest() == expected,
                        f"evidence {name}: receipt source drift: {source}")
            for digest in receipt.get("tool_sha256", {}).values():
                require(re.fullmatch(r"[0-9a-f]{64}", digest) is not None, f"evidence {name}: invalid tool digest")
            validate_receipt_contract(root, name, receipt, require)

    def obligation(row, label):
        nonempty(row, ["owner", "bounds", "exclusions", "evidence", "assumptions"], label)
        require(row.get("status") in STATUSES, f"{label}: unsupported assurance status {row.get('status')!r}")
        refs(row.get("evidence"), evidence, f"{label} evidence")
        refs(row.get("assumptions"), assumptions, f"{label} assumptions")
        next_gate(row, label)

    for name, row in invariants.items():
        obligation(row, f"invariant {name}")
        nonempty(row, ["predicate", "negative_control", "review_trigger"], f"invariant {name}")
        require(row.get("status") == "specified", f"invariant {name}: universal obligation cannot inherit a bounded result")
    plan = (root / "docs/correctness-plan.md").read_text()
    expected_invariants = set(re.findall(r"^\| ([A-Z]+-\d+) \|", plan, re.M))
    require(set(invariants) == expected_invariants and bool(expected_invariants), "invariant inventory differs from correctness plan")
    coverage = index(ledger.get("coverage", []), "name", "coverage")
    audit = (root / "docs/correctness-audit.md").read_text().split("## Coverage map", 1)[1].split("\n## ", 1)[0]
    expected_coverage = {line.split("|")[1].strip() for line in audit.splitlines() if line.startswith("| ") and not line.startswith("| Capability")}
    require(set(coverage) == expected_coverage, "coverage inventory differs from audit coverage map")
    for name, row in coverage.items():
        obligation(row, f"coverage {name}")
        refs(row.get("invariants"), invariants, f"coverage {name} invariants")
        nonempty(row, ["source"], f"coverage {name}")
        for ref in row.get("source", []):
            source_ref(ref, f"coverage {name}")
    for row in ledger.get("compatibility", []):
        nonempty(row, ["id", "version", "source", "read_write_behavior", "upgrade_downgrade"], "compatibility")
        next_gate(row, "compatibility")
        for ref in row.get("source", []):
            source_ref(ref, "compatibility")
    require(len(ledger.get("compatibility", [])) >= 10, "missing schema compatibility boundaries")
    for row in ledger.get("activation_gates", []):
        nonempty(row, ["id", "owner", "requires", "permits", "does_not_permit"], "activation gate")
        next_gate(row, "activation gate")
    require({r.get("id") for r in ledger.get("activation_gates", [])} == {"artifact", "native", "direct-write", "provider-study"}, "missing activation boundaries")

    effects = documents.get("effects", {})
    profiles = index(effects.get("profiles", []), "id", "profiles")
    for name, row in profiles.items():
        obligation(row, f"effect {name}")
        nonempty(row, ["authority", "source"], f"effect {name}")
        refs(row.get("invariants"), invariants, f"effect {name} invariants")
        for state in row.get("reads", []):
            require(state in states, f"effect {name}: unknown read state {state}")
        require(isinstance(row.get("writes"), list), f"effect {name}: writes must be explicit")
        require(isinstance(row.get("outbound"), list), f"effect {name}: outbound must be explicit")
        for write in row.get("writes", []):
            require(write.get("state") in states, f"effect {name}: unknown write state")
            require(write.get("mode") in WRITE_MODES, f"effect {name}: unknown write mode")
            nonempty(write, ["condition"], f"effect {name} write")
        for ref in row.get("source", []):
            source_ref(ref, f"effect {name}")

    cli = index(effects.get("cli", []), "name", "CLI")
    mcp = index(effects.get("mcp", []), "name", "MCP")
    hooks = index(effects.get("hooks", []), "name", "hooks")
    require(set(cli) == enum_variants((root / "crates/gobstopper-cli/src/main.rs").read_text(), "Cmd"), "CLI inventory differs from command enum")
    require(set(hooks) == enum_variants((root / "crates/gobstopper-cli/src/hooks.rs").read_text(), "HookTarget"), "hook inventory differs from HookTarget")
    mcp_text = (root / "crates/gobstopper-cli/src/mcp.rs").read_text().split("fn tools(", 1)[1].split("\nfn ", 1)[0]
    require(set(mcp) == set(re.findall(r'"name": "([a-z_]+)"', mcp_text)), "MCP inventory differs from advertised tools")
    for label, surfaces in (("CLI", cli), ("MCP", mcp), ("hook", hooks)):
        for name, row in surfaces.items():
            refs(row.get("effects"), profiles, f"{label} {name}")
            source_ref(row.get("source", {}), f"{label} {name}")
            if label == "CLI":
                nonempty(row, ["contract"], f"CLI {name}")
            if label == "MCP":
                require(row.get("content_opt_in") is (name in {"read_snapshot", "search_snapshot"}), f"MCP {name}: content gate mismatch")
    scripts = index(effects.get("scripts", []), "path", "scripts")
    expected_scripts = source_scripts(root)
    require(set(scripts) == expected_scripts, "script effect inventory differs from production Python entry points")
    for path, row in scripts.items():
        source_ref({"path": path}, "script")
        refs(row.get("effects"), profiles, f"script {path}")
    callables = effects.get("library_callables", [])
    actual_callables = {(r.get("path"), r.get("symbol")) for r in callables}
    require(len(actual_callables) == len(callables), "duplicate callable effect row")
    require(actual_callables == source_callables(root), "Rust public callable inventory drift; classify added/removed declarations")
    for row in callables:
        refs(row.get("effects"), profiles, f"callable {row.get('path')}::{row.get('symbol')}")

    claims = index(documents.get("claims", {}).get("claims", []), "id", "claims")
    require(bool(claims), "missing claim registry")
    for name, row in claims.items():
        obligation(row, f"claim {name}")
        nonempty(row, ["assertion", "public_sources", "review_trigger"], f"claim {name}")
        require(not re.search(r"\b(?:provably correct|fully proven|universally safe|zero bugs)\b", str(row.get("assertion", "")), re.I), f"claim {name}: unsupported universal assertion")
        if row.get("status") == "bounded_check":
            require(any(evidence.get(e, {}).get("kind") in {"bounded_model_receipt", "verification_receipt"} for e in row.get("evidence", [])), f"claim {name}: bounded check lacks a pinned receipt")
        if row.get("status") == "historical_observation":
            require(any(evidence.get(e, {}).get("kind") in {"historical_receipt", "scanner_inventory"} for e in row.get("evidence", [])), f"claim {name}: historical observation lacks evidence")
        for ref in row.get("public_sources", []):
            source_ref(ref, f"claim {name}")

    triage = documents.get("codeql-triage", {})
    alerts = index(triage.get("alerts", []), "number", "alerts")
    require(set(alerts) == ALERT_IDS, "baseline alert inventory must preserve all 21 exact alert IDs")
    require(triage.get("snapshot", {}).get("count") == 21, "baseline alert count mismatch")
    require(re.fullmatch(r"[0-9a-f]{64}", triage.get("snapshot", {}).get("sha256", "")) is not None, "missing baseline scanner snapshot digest")
    classes = {"requested_identity", "synthetic_test_only", "credential_fragment", "background_identity"}
    counts = {kind: 0 for kind in classes}
    for number, row in alerts.items():
        require(row.get("baseline_state") == "open" and row.get("baseline_revision") == BASELINE, f"alert {number}: cannot rewrite baseline scanner state")
        require(row.get("scanner_action") == "none", f"alert {number}: triage must not dismiss or suppress")
        require(row.get("rule") == "rust/cleartext-logging", f"alert {number}: baseline rule changed")
        require(row.get("classification") in classes, f"alert {number}: missing individual disposition")
        if row.get("classification") in classes:
            counts[row["classification"]] += 1
        nonempty(row, ["authorized_output_contract", "decision", "resolution", "url"], f"alert {number}")
        require(row.get("resolution") in {"reviewed_retained_output", "source_repaired_pending_exact_head_scan"}, f"alert {number}: unsupported closure claim")
        nonempty(row.get("source", {}), ["path", "origin"], f"alert {number} source")
        nonempty(row.get("sink", {}), ["channel", "operation", "baseline_code"], f"alert {number} sink")
        nonempty(row.get("remediation", {}), ["owner", "source_revision", "evidence", "validation_gate", "remaining_risk"], f"alert {number} remediation")
        refs(row.get("remediation", {}).get("evidence"), evidence, f"alert {number} evidence")
        next_gate(row, f"alert {number}")
    require(counts == triage.get("counts"), "alert disposition counts mismatch")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    try:
        docs = load_documents(args.root)
        errors = validate(args.root, docs)
    except (OSError, ValueError, KeyError, IndexError, TypeError, AttributeError, SyntaxError) as error:
        errors = [f"invalid inventory/source structure: {error}"]
    if errors:
        for error in errors:
            print(f"assurance: {error}", file=sys.stderr)
        return 1
    print("assurance inventory: passed (structure and surface coverage only; not a correctness proof)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

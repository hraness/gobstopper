#!/usr/bin/env python3
"""Admit production Kani proofs only with complete checks, covers and a killed mutant."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "verify/watch/check.py"
spec = importlib.util.spec_from_file_location("watch_runner", RUNNER)
assert spec and spec.loader
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

KANI_VERSION = "0.68.0"
CBMC_VERSION = "6.11.0"
RUSTC_VERSION = "rustc 1.100.0-nightly (8925ea358 2026-08-20)"
BOUNDARY_HARNESS = "admission::proofs::production_plan_bounds"
BOUNDARY_ASSERTION = "exact production plan bounds"
MUTANT_OLD = "edits <= MAX_EDITS && items <= MAX_ITEMS"
MUTANT_NEW = "edits < MAX_EDITS && items <= MAX_ITEMS"

# Every harness must have these specification assertions proved reachable and
# successful; every cover, including any added later, must be SATISFIED.
HARNESSES = {
    "events::proofs::evidence_agreement_is_order_independent_and_conflict_absorbing": [
        "evidence join is commutative", "evidence join is associative",
        "identical evidence replay is idempotent", "conflicting evidence cannot recover through replay",
        "only identical evidence remains agreed"],
    BOUNDARY_HARNESS: [BOUNDARY_ASSERTION],
    "admission::proofs::digest_byte_admission": ["digest byte bound agrees with wide oracle"],
    "admission::proofs::eligibility_and_protection": ["only live positive unprotected payload is eligible"],
    "admission::proofs::protected_suffix_boundary": ["recent suffix never enters candidate prefix"],
    "admission::proofs::edit_step_is_atomic_and_exclusive": [
        "edit admission matches full domain oracle", "successful step preserves admission invariant",
        "provider controls are unmixed", "rejection preserves admission state"],
    "admission::proofs::edit_sequences_four": [
        "accepted sequence has no mixed control", "accepted sequence has at most one digest"],
    **{f"admission::proofs::indexes_{length}": [
        "strict order rejects duplicate and reversed IDs",
        "ordered lookup matches independent linear search"]
       for length in ("empty", "one", "two", "four")},
    "estimate::proofs::token_rounding_full_u64": ["token estimate agrees with wide oracle"],
    "estimate::proofs::aggregate_saturates_full_u64": ["aggregate agrees with wide saturation oracle"],
    "estimate::proofs::savings_never_exceed_live_payload_or_context": [
        "savings agree with independent wide oracle", "savings cannot exceed live item estimate"],
    "model::proofs::cumulative_usage_preserves_absence_and_bounds_cache": [
        "cached cumulative usage is bounded by known input",
        "replaying a cumulative report does not double count",
        "context report presence preserves or establishes provenance",
        "only cumulative input establishes full lifetime scope",
        "only reported context is a measured value including zero",
        "explicit reset is not measured empty context",
        "invalidated context is unavailable rather than measured empty",
        "partial component evidence is never complete context"],
    "policy::admission_proofs::policy_bounds_full_domain": [
        "production policy admission agrees with independent bounds"],
}
UNWINDS = {"edit_sequences_four": 5, "indexes_empty": 2, "indexes_one": 3,
           "indexes_two": 4, "indexes_four": 6}


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def label(check: dict) -> str:
    return check["description"].strip('"')


def admit(document: dict, exit_code: int, timeout: bool, log_limit: bool,
          mutant: bool = False) -> dict:
    """Closed admission of Kani's pinned JSON schema, not banner matching."""
    expected = {BOUNDARY_HARNESS} if mutant else set(HARNESSES)
    verification = document["verification_results"]
    summary = verification["summary"]
    results = verification["results"]
    metadata = document["harness_metadata"]
    tools = document["tools"]
    okay = (
        not timeout and not log_limit
        and document["metadata"]["version"] == "1.0"
        and tools["kani"] == KANI_VERSION and tools["cbmc"].split()[0] == CBMC_VERSION
        and tools["rustc"] == RUSTC_VERSION
        and summary["status"] == "completed"
        and summary["total_harnesses"] == len(expected) and summary["executed"] == len(expected)
        and len(results) == len(expected) and {r["harness_id"] for r in results} == expected
        and len(metadata) == len(expected) and {m["pretty_name"] for m in metadata} == expected
        and all(m["attributes"]["kind"] == "Proof" and not m["attributes"]["should_panic"]
                for m in metadata)
    )
    counts = {"assertions": 0, "covers": 0, "unwind_checks": 0,
              "unreachable_safety_checks": 0, "unsupported_paths_excluded": 0}
    failed_labels = []
    for result in results:
        checks = result["checks"]
        known = {r["harness_id"] for r in results} <= expected
        if not known:
            return {"passed": False, "counts": counts, "failed_labels": []}
        required = HARNESSES[result["harness_id"]]
        assertions = [c for c in checks if c["category"] == "assertion"]
        covers = [c for c in checks if c["category"] == "cover"]
        counts["assertions"] += len(assertions)
        counts["covers"] += len(covers)
        counts["unwind_checks"] += sum(c["category"] == "unwind" for c in checks)
        counts["unreachable_safety_checks"] += sum(c["status"] == "Unreachable" for c in checks)
        counts["unsupported_paths_excluded"] += sum(c["category"] == "unsupported_construct"
                                                      and c["status"] == "Success" for c in checks)
        failures = [c for c in checks if c["status"] == "Failure"]
        failed_labels.extend(label(c) for c in failures)
        if mutant:
            okay = okay and (
                exit_code != 0 and summary["failed"] == 1 and summary["successful"] == 0
                and result["status"] == "Failure" and len(failures) == 1
                and failures[0]["category"] == "assertion"
                and label(failures[0]) == BOUNDARY_ASSERTION
            )
            allowed_covers = {"Satisfied", "Unsatisfiable"}
        else:
            okay = okay and (
                exit_code == 0 and summary["failed"] == 0
                and summary["successful"] == len(expected) and result["status"] == "Success"
                and bool(assertions) and bool(covers)
                and all(any(label(c) == name and c["status"] == "Success" for c in assertions)
                        for name in required)
            )
            allowed_covers = {"Satisfied"}
        for check in checks:
            if check["category"] == "cover":
                okay = okay and check["status"] in allowed_covers
            elif mutant and check in failures:
                continue
            elif check["category"] in {"unwind", "unsupported_construct"}:
                okay = okay and check["status"] == "Success"
            else:
                okay = okay and check["status"] in {"Success", "Unreachable"}
    if not mutant:
        okay = okay and counts["unwind_checks"] >= 3
    return {"passed": bool(okay), "counts": counts, "failed_labels": failed_labels}


def check_sources(sources: dict[str, bytes]) -> None:
    core = {name: data.decode() for name, data in sources.items()
            if name.startswith("crates/gobstopper-core/src/") and name.endswith(".rs")}
    for name, source in core.items():
        if re.search(r"kani::(?:assume|stub|stub_verified|should_panic)\b", source):
            raise ValueError(f"unreviewed proof restriction/substitution in {name}")
    admission = core["crates/gobstopper-core/src/admission.rs"]
    if admission.count(MUTANT_OLD) != 1:
        raise ValueError("the reviewed production boundary mutation no longer has one exact target")
    for name, bound in UNWINDS.items():
        if not re.search(r"#\[kani::proof\]\s*#\[kani::unwind\(" + str(bound)
                         + r"\)\]\s*fn " + name + r"\(", admission):
            raise ValueError(f"unreviewed unwind bound for {name}")


def source_paths() -> list[Path]:
    paths = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", RUNNER,
             ROOT / "crates/gobstopper-core/Cargo.toml"]
    paths += sorted((ROOT / "crates/gobstopper-core/src").rglob("*.rs"))
    paths += sorted(Path(__file__).resolve().parent.glob("*.py"))
    paths += [Path(__file__).resolve().parent / "README.md"]
    # Admitted caller correspondence is reviewed/tested, not symbolically
    # executed by these core harnesses. Bind those exact callers separately.
    paths += [ROOT / path for path in (
        "crates/gobstopper-cli/src/config.rs", "crates/gobstopper-adapters/src/codex.rs",
        "crates/gobstopper-adapters/src/payload.rs", "crates/gobstopper-adapters/src/claude.rs",
        "crates/gobstopper-adapters/src/eval.rs",
        "crates/gobstopper-cli/src/main.rs", "crates/gobstopper-cli/src/hooks.rs",
        "crates/gobstopper-cli/src/report.rs", "crates/gobstopper-cli/src/telemetry.rs")]
    return paths


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kani", default="cargo-kani", help="installed Kani 0.68.0 executable")
    parser.add_argument("--output", type=Path, required=True, help="new private evidence directory")
    args = parser.parse_args()
    kani = Path(shutil.which(args.kani) or args.kani).resolve(strict=True)
    # The installed shim's documented default. Removing inherited KANI_HOME
    # below keeps observed bundle hashes bound to the selected installation.
    kani_home = (Path.home() / ".kani/kani-0.68.0").resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    output.chmod(0o700)
    paths = source_paths()
    sources = {str(path.relative_to(ROOT)): path.read_bytes() for path in paths}
    check_sources(sources)
    source_hashes = {name: sha(data) for name, data in sources.items()}
    binaries = [kani, *(kani_home / "bin" / name for name in (
        "kani-driver", "kani-compiler", "cbmc", "goto-instrument", "goto-cc")),
        kani_home / "rustc-version", kani_home / "rust-toolchain-version"]
    tool_hashes = {str(path): runner.digest(path) for path in binaries}
    env = dict(os.environ)
    for key in list(env):
        if (key in {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"}
                or key.startswith(("KANI", "CBMC"))
                or key.startswith("CARGO_TARGET_") and key.endswith("_RUSTFLAGS")):
            env.pop(key)
    env.update(CARGO_NET_OFFLINE="true", CARGO_TERM_COLOR="never")
    version_code, version_log, timed_out, capped = runner.run_owned(
        [str(kani), "--version"], ROOT, env, 10, 32768)
    (output / "tool-version.log").write_text(version_log)
    if (version_code != 0 or timed_out or capped
            or f"Kani Rust Verifier {KANI_VERSION} " not in version_log
            or f"CBMC {CBMC_VERSION}" not in version_log
            or (kani_home / "rustc-version").read_text().strip() != RUSTC_VERSION):
        parser.error("installed proof toolchain differs from the reviewed pin")

    # Mutate the actual kernel in an isolated copy, keeping the exact proof and
    # dependencies. Production files are never edited for a negative control.
    mutant = output / "boundary-mutant"
    for name, data in sources.items():
        if name in {"Cargo.toml", "Cargo.lock"} or name.startswith("crates/gobstopper-core/"):
            path = mutant / name
            path.parent.mkdir(parents=True, exist_ok=True)
            if name == "Cargo.toml":
                data = re.sub(rb"members\s*=\s*\[.*?\]",
                              b'members = ["crates/gobstopper-core"]', data, count=1, flags=re.S)
            if name == "crates/gobstopper-core/src/admission.rs":
                data = data.replace(MUTANT_OLD.encode(), MUTANT_NEW.encode())
            path.write_bytes(data)
    mutation_path = mutant / "crates/gobstopper-core/src/admission.rs"
    mutation_hash = runner.digest(mutation_path)
    results = []
    for name, cwd in (("production", ROOT), ("boundary-mutant", mutant)):
        report = output / f"{name}.json"
        argv = [str(kani), "-p", "gobstopper-core", "--output-format", "terse",
                "-Z", "unstable-options", "--export-json", str(report),
                "--harness-timeout", "60s", "--target-dir", str(output / f"{name}-target")]
        if name == "boundary-mutant":
            argv += ["--harness", BOUNDARY_HARNESS, "--exact"]
        code, log, timeout, log_limit = runner.run_owned(argv, cwd, env, 900, 32 * 1024 * 1024)
        log_path = output / f"{name}.log"
        log_path.write_text(log)
        try:
            parsed = admit(json.loads(report.read_text()), code, timeout, log_limit,
                           mutant=name == "boundary-mutant")
        except (OSError, ValueError, KeyError, TypeError):
            parsed = {"passed": False, "counts": None, "failed_labels": []}
        results.append({"case": name, **parsed, "argv": argv, "exit_code": code,
                        "timeout": timeout, "log_limit": log_limit,
                        "log_sha256": runner.digest(log_path),
                        "report_sha256": runner.digest(report) if report.exists() else None})
        print(f"{name}: {'PASS' if parsed['passed'] else 'FAIL'} {parsed['counts']}", flush=True)
    unchanged = (all(runner.digest(ROOT / name) == value for name, value in source_hashes.items())
                 and all(runner.digest(Path(name)) == value for name, value in tool_hashes.items())
                 and runner.digest(mutation_path) == mutation_hash)
    receipt = {
        "schema": "gobstopper.core-kani-check.v1",
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "scope": "production scalar kernels over full numeric domains; index lengths 0/1/2/4 and four-edit sequence",
        "passed": unchanged and all(result["passed"] for result in results),
        "inputs_unchanged": unchanged, "source_sha256": source_hashes, "tool_sha256": tool_hashes,
        "versions": {"kani": KANI_VERSION, "cbmc": CBMC_VERSION, "rustc": RUSTC_VERSION},
        "unwind_bounds": UNWINDS, "mutation": {"old": MUTANT_OLD, "new": MUTANT_NEW,
                                                "source_sha256": mutation_hash},
        "results": results,
    }
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(f"receipt: {output / 'receipt.json'}", flush=True)
    return 0 if receipt["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

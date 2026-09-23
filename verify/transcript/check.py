#!/usr/bin/env python3
"""Build/replay Lean proofs and admit finite production correspondence plus two mutants."""
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
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
RUNNER = ROOT / "verify/watch/check.py"
spec = importlib.util.spec_from_file_location("watch_runner", RUNNER)
assert spec and spec.loader
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

LEAN_VERSION = "4.34.0"
LEAN_COMMIT = "293d5d0c0c3f3dded4688b3ccd6a33939ac5102b"
THEOREMS = {
    "maskOne_key", "maskOne_live", "maskOne_protected", "maskOne_event",
    "protected_identity", "inactive_identity", "selected_is_masked", "unselected_identity",
    "maskOne_empty", "mask_empty", "length_preserved", "order_and_identity_preserved",
    "links_preserved", "effective_commutes", "protected_projection_preserved",
    "maskOne_compose", "composition", "idempotence", "admitted_targets_known_safe", "rejection_identity", "admitted_execution",
    "tool_wellformedness_preserved", "digest_keeps_prior_order", "digest_keeps_prior_records",
    "digest_fresh_identity", "quiet_append", "digest_preserves_tool_links",
}
CASES = {
    "empty", "first", "second", "both", "reverse_selection", "protected", "user", "call",
    "newest_user", "unknown", "duplicate", "mixed_unknown", "mixed_protected", "digest_only",
    "mask_and_digest", "both_and_digest", "rejected_with_digest", "compose", "repeat", "repeat_both",
}
ALLOWED_AXIOMS = {"propext", "Quot.sound"}
RUST_OLD = "raw = apply_elide(&raw, &targets, stub_template, per_item_stubs).0;"
RUST_NEW = "raw = apply_elide(&raw, &std::collections::HashSet::new(), stub_template, per_item_stubs).0;"
ORACLE_OLD = '("content", toJson r.content)'
ORACLE_NEW = '("content", toJson (if r.content == 0 then (1 : Nat) else r.content))'


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def rust_tools(code: int, log: str, timeout: bool, capped: bool) -> tuple[Path, Path]:
    if code or timeout or capped or len(log.splitlines()) != 1 or not Path(log.strip()).is_absolute():
        raise ValueError("cannot identify selected Rust sysroot")
    toolchain = Path(log.strip())
    return toolchain / "bin/cargo", toolchain / "bin/rustc"


def source_hygiene(files: dict[str, bytes]) -> None:
    expected = {"Transcript.lean", "Vectors.lean", "Audit.lean"}
    if {name for name in files if name.endswith(".lean")} != expected:
        raise ValueError("unreviewed Lean module inventory")
    allowed_imports = {
        "Transcript.lean": ["Init.Data.List.Lemmas"],
        "Vectors.lean": ["Transcript", "Lean.Data.Json.FromToJson"],
        "Audit.lean": ["Transcript"],
    }
    for name in expected:
        source = files[name].decode()
        # Deliberately conservative lexical tripwire, backed by kernel replay
        # and review. It is not a proof that arbitrary Lean metaprograms are safe.
        if re.search(r"\b(?:sorry|admit|axiom|unsafe|partial|opaque|implemented_by|extern|native_decide|run_tac|elab|macro|initialize|set_option)\b", source):
            raise ValueError(f"unreviewed proof escape or metaprogram in {name}")
        if re.findall(r"^import\s+(\S+)\s*$", source, re.M) != allowed_imports[name]:
            raise ValueError(f"unreviewed imports in {name}")
    declarations = re.findall(r"^(?:@\[simp\] )?theorem (\w+)", files["Transcript.lean"].decode(), re.M)
    audit = re.findall(r"^#print axioms Transcript\.(\w+)$", files["Audit.lean"].decode(), re.M)
    if len(declarations) != len(THEOREMS) or set(declarations) != THEOREMS:
        raise ValueError("unreviewed theorem inventory")
    if len(audit) != len(THEOREMS) or set(audit) != THEOREMS:
        raise ValueError("axiom audit must cover each theorem exactly once")
    manifest = json.loads(files["lake-manifest.json"])
    if manifest["packages"] != []:
        raise ValueError("unreviewed Lean dependencies")
    if files["lean-toolchain"].decode().strip() != f"leanprover/lean4:v{LEAN_VERSION}":
        raise ValueError("unreviewed Lean toolchain")
    if files["Vectors.lean"].decode().count(ORACLE_OLD) != 1:
        raise ValueError("oracle mutation does not have exactly one target")


def admit_axioms(log: str) -> dict[str, list[str]]:
    observed = {}
    for line in log.splitlines():
        empty = re.fullmatch(r"'Transcript\.(\w+)' does not depend on any axioms", line)
        used = re.fullmatch(r"'Transcript\.(\w+)' depends on axioms: \[(.*)\]", line)
        if empty:
            name, axioms = empty[1], []
        elif used:
            name, axioms = used[1], used[2].split(", ")
        else:
            raise ValueError("unknown or diagnostic output in axiom audit")
        if name in observed or not set(axioms) <= ALLOWED_AXIOMS:
            raise ValueError("duplicate theorem or unreviewed axiom")
        observed[name] = axioms
    if set(observed) != THEOREMS:
        raise ValueError("incomplete axiom audit")
    return observed


def admit_vectors(raw: bytes) -> dict:
    document = json.loads(raw)
    cases = document["cases"]
    if document["schema"] != 1 or document["oracle"] != "lean-structural-v1":
        raise ValueError("unreviewed vector schema")
    if len(cases) != len(CASES) or {case["name"] for case in cases} != CASES:
        raise ValueError("incomplete vector family")
    steps = [step for case in cases for step in case["steps"]]
    counts = {"cases": len(cases), "steps_per_provider": len(steps),
              "accepted_per_provider": sum(step["admitted"] is True for step in steps),
              "refused_per_provider": sum(step["admitted"] is False for step in steps),
              "digests_per_provider": sum(step["admitted"] is True and step["digest"] is True for step in steps)}
    if counts != {"cases": 20, "steps_per_provider": 23, "accepted_per_provider": 12,
                  "refused_per_provider": 11, "digests_per_provider": 3}:
        raise ValueError("vector non-vacuity counts changed")
    if not all(step["well_linked"] is True for step in steps):
        raise ValueError("correspondence fixtures must satisfy tool-link preconditions")
    return counts


def admit_cargo(code: int, log: str, timeout: bool, capped: bool, mutant: str | None = None) -> bool:
    if timeout or capped or "could not compile" in log or re.search(r"error\[E\d+\]", log):
        return False
    status = "FAILED" if mutant else "ok"
    test_lines = re.findall(r"^test lean_vectors_match_production \.\.\. (\w+)$", log, re.M)
    summary = ("test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;"
               if mutant else "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;")
    if test_lines != [status] or log.count(summary) != 1 or code != (101 if mutant else 0):
        return False
    if mutant:
        expected_values = r"left: 0\s+right: 1" if mutant == "oracle" else r"left: 3\s+right: 0"
        return (mutant in {"oracle", "rust"} and log.count("LEAN_CORRESPONDENCE_PAYLOAD Codex/first") == 1
                and len(re.findall(r"panicked at", log)) == 1
                and re.search(expected_values, log) is not None)
    return True


def input_paths() -> list[Path]:
    paths = [path for path in HERE.iterdir() if path.is_file() and not path.name.startswith(".")]
    paths += [ROOT / "Cargo.toml", ROOT / "Cargo.lock", RUNNER]
    # Preserve the complete dependency-resolution graph for --locked even
    # though the CLI sibling is not a target of this correspondence check.
    paths += [ROOT / "crates/gobstopper-cli/Cargo.toml"]
    for crate in ("gobstopper-core", "gobstopper-adapters"):
        base = ROOT / "crates" / crate
        paths += [base / "Cargo.toml", *sorted((base / "src").rglob("*.rs"))]
    paths += [ROOT / "crates/gobstopper-adapters/tests/lean_correspondence.rs"]
    if (ROOT / "verify/tools.lock.json").exists():
        paths += [ROOT / "verify/tools.lock.json"]
    return sorted(set(paths))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lake", required=True, help="pinned Lean 4.34.0 bundle's lake executable")
    parser.add_argument("--output", required=True, type=Path, help="new private evidence directory")
    args = parser.parse_args()
    lake = Path(shutil.which(args.lake) or args.lake).resolve(strict=True)
    lean = lake.with_name("lean")
    checker = lake.with_name("leanchecker")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    output.chmod(0o700)
    sources = {str(path.relative_to(ROOT)): path.read_bytes() for path in input_paths()}
    local = {name.removeprefix("verify/transcript/"): data for name, data in sources.items()
             if name.startswith("verify/transcript/")}
    source_hygiene(local)
    codex_name = "crates/gobstopper-adapters/src/codex.rs"
    if sources[codex_name].decode().count(RUST_OLD) != 1:
        parser.error("production lowering mutation must have exactly one target")
    source_hashes = {name: digest(data) for name, data in sources.items()}
    # Preserve argv[0] for the initial rustup dispatcher; the bounded sysroot
    # query below selects the actual binaries subsequently hashed and invoked.
    cargo_dispatch = Path(shutil.which("cargo") or "cargo").absolute()
    rustc_dispatch = Path(shutil.which("rustc") or "rustc").absolute()
    tool_hashes = {str(path): runner.digest(path) for path in
                   (lake, lean, checker, cargo_dispatch, rustc_dispatch, Path(sys.executable).resolve())}
    env = dict(os.environ)
    for key in list(env):
        if (key.startswith(("LEAN", "LAKE", "GOBSTOPPER"))
                or key in {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC", "CARGO_BUILD_RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"}
                or key.startswith("CARGO_TARGET_") and key.endswith("_RUSTFLAGS")):
            env.pop(key)
    env.update(PATH=f"{lake.parent}{os.pathsep}{env.get('PATH', '')}",
               CARGO_NET_OFFLINE="true", CARGO_TERM_COLOR="never", CARGO_INCREMENTAL="0")
    results = []

    def run(name: str, argv: list[str], cwd: Path, seconds: int = 120,
            extra_env: dict[str, str] | None = None) -> tuple[int, str, bool, bool]:
        code, log, timeout, capped = runner.run_owned(argv, cwd, env | (extra_env or {}), seconds, 8 * 1024 * 1024)
        log_path = output / f"{name}.log"
        log_path.write_text(log)
        results.append({"case": name, "argv": argv, "exit_code": code, "timeout": timeout,
                        "log_limit": capped, "log_sha256": runner.digest(log_path),
                        "passed": code == 0 and not timeout and not capped})
        return code, log, timeout, capped

    receipt: dict = {"schema": "gobstopper.transcript-lean-check.v1", "observed_at": datetime.now(timezone.utc).isoformat(),
                    "scope": "unbounded structural list laws; finite synthetic production correspondence for three provider dialects",
                    "source_sha256": source_hashes, "tool_sha256": tool_hashes, "results": results, "passed": False}
    try:
        identity = run("rust-sysroot", [str(rustc_dispatch), "--print", "sysroot"], ROOT, 10)
        cargo, rustc = rust_tools(*identity)
        for path in (cargo, rustc):
            tool_hashes[str(path)] = runner.digest(path)
        env["RUSTC"] = str(rustc)
        code, log, timeout, capped = run("lean-version", [str(lean), "--version"], ROOT, 10)
        if code or timeout or capped or f"version {LEAN_VERSION}," not in log or LEAN_COMMIT not in log:
            raise ValueError("installed Lean differs from reviewed version/commit")
        for name, command in (("lake-version", [str(lake), "--version"]), ("rust-version", [str(rustc), "--version", "--verbose"]),
                              ("cargo-version", [str(cargo), "--version"])):
            run(name, command, ROOT, 10)
            if not results[-1]["passed"]:
                raise ValueError(f"cannot identify {name}")

        positive = output / "lean-positive"
        oracle_mutant = output / "lean-oracle-mutant"
        for target in (positive, oracle_mutant):
            target.mkdir()
            for name, data in local.items():
                if name.endswith(".lean") or name in {"lakefile.toml", "lake-manifest.json", "lean-toolchain"}:
                    if target == oracle_mutant and name == "Vectors.lean":
                        data = data.replace(ORACLE_OLD.encode(), ORACLE_NEW.encode())
                    (target / name).write_bytes(data)
        receipt["mutations"] = {
            "oracle": {"old": ORACLE_OLD, "new": ORACLE_NEW,
                       "source_sha256": runner.digest(oracle_mutant / "Vectors.lean")},
            "rust": {"old": RUST_OLD, "new": RUST_NEW},
        }
        run("lean-build", [str(lake), "build"], positive)
        if not results[-1]["passed"]:
            raise ValueError("Lean build failed")
        code, log, timeout, capped = run("axioms", [str(lake), "env", "lean", "Audit.lean"], positive)
        if code or timeout or capped:
            raise ValueError("axiom audit failed")
        receipt["axioms"] = admit_axioms(log)
        run("kernel-replay", [str(lake), "env", "leanchecker", "--fresh", "Transcript"], positive)
        if not results[-1]["passed"]:
            raise ValueError("fresh kernel replay failed")
        _, vectors, _, _ = run("vectors", [str(positive / ".lake/build/bin/vectors")], positive, 10)
        if not results[-1]["passed"] or vectors.encode() != local["vectors.json"]:
            raise ValueError("fresh Lean vectors differ from reviewed correspondence fixture")
        receipt["vectors"] = admit_vectors(vectors.encode())
        vector_path = output / "vectors.json"
        vector_path.write_text(vectors)
        test_argv = [str(cargo), "test", "-p", "gobstopper-adapters", "--locked", "--test", "lean_correspondence",
                     "--target-dir", str(output / "rust-target"), "--", "--test-threads=1"]
        result = run("rust-correspondence", test_argv, ROOT, 600, {"GOBSTOPPER_LEAN_VECTORS": str(vector_path)})
        results[-1]["passed"] = admit_cargo(*result)
        if not results[-1]["passed"]:
            raise ValueError("production correspondence failed")

        run("oracle-mutant-build", [str(lake), "build"], oracle_mutant)
        if not results[-1]["passed"]:
            raise ValueError("oracle mutant must build before correspondence can reject it")
        _, changed_vectors, _, _ = run("oracle-mutant-vectors", [str(oracle_mutant / ".lake/build/bin/vectors")], oracle_mutant, 10)
        if not results[-1]["passed"] or changed_vectors == vectors:
            raise ValueError("oracle mutation did not change the emitted vectors")
        mutant_vector_path = output / "oracle-mutant-vectors.json"
        mutant_vector_path.write_text(changed_vectors)
        result = run("oracle-mutant-correspondence", test_argv, ROOT, 600, {"GOBSTOPPER_LEAN_VECTORS": str(mutant_vector_path)})
        results[-1]["passed"] = admit_cargo(*result, mutant="oracle")
        if not results[-1]["passed"]:
            raise ValueError("oracle mutant was not rejected for the exact payload mismatch")

        rust_mutant = output / "rust-lowering-mutant"
        for name, data in sources.items():
            if name in {"Cargo.toml", "Cargo.lock", "verify/transcript/vectors.json"} or name.startswith("crates/"):
                if name == codex_name:
                    data = data.replace(RUST_OLD.encode(), RUST_NEW.encode())
                path = rust_mutant / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
        # Cargo requires the CLI manifest's declared target to exist while
        # resolving the original lockfile. This sibling is never compiled or
        # executed by the exact `-p gobstopper-adapters --test ...` command.
        cli_sibling = rust_mutant / "crates/gobstopper-cli/src/main.rs"
        cli_sibling.parent.mkdir(parents=True, exist_ok=True)
        cli_sibling.write_text("// Uncompiled workspace metadata target.\nfn main() {}\n")
        receipt["mutations"]["rust"]["uncompiled_cli_metadata_target"] = True
        receipt["mutations"]["rust"]["source_sha256"] = runner.digest(rust_mutant / codex_name)
        result = run("rust-mutant-correspondence", test_argv, rust_mutant, 600, {"GOBSTOPPER_LEAN_VECTORS": str(vector_path)})
        results[-1]["passed"] = admit_cargo(*result, mutant="rust")
        if not results[-1]["passed"]:
            raise ValueError("production mutant was not rejected for the exact payload mismatch")
        receipt["inputs_unchanged"] = (
            all(runner.digest(ROOT / name) == value for name, value in source_hashes.items())
            and all(runner.digest(Path(name)) == value for name, value in tool_hashes.items()))
        receipt["passed"] = receipt["inputs_unchanged"] and all(result["passed"] for result in results)
    except (OSError, ValueError, KeyError, TypeError, RuntimeError, subprocess.SubprocessError) as error:
        receipt["error"] = str(error)
    finally:
        (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        for result in results:
            print(f"{result['case']}: {'PASS' if result['passed'] else 'FAIL'}", flush=True)
        print(f"receipt: {output / 'receipt.json'}", flush=True)
    return 0 if receipt["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

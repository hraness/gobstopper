#!/usr/bin/env python3
"""Check the finite vault protocol and semantic negative controls with pinned TLC."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil


RUNNER = Path(__file__).resolve().parents[1] / "watch/check.py"
spec = importlib.util.spec_from_file_location("watch_runner", RUNNER)
assert spec and spec.loader
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


TLC_SHA256 = "936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
CASES = {
    "safe": None,
    "no-custody": "IndexedData",
    "no-pins": "PinnedRecoveryData",
    "witness-completion": "NoSuccessfulComposition",
    "witness-pending": "NoPendingRecovery",
    "publication-safe": None,
    "publication-no-intent": "OutputHasIntent",
    "publication-no-sync": "CompletedDurable",
    "publication-ignore-damage": "DamageBlocksCollection",
    "publication-ignore-unlink": "AllManifestsValid",
    "publication-recovery": "NoRecoveryWitness",
}


def expected_config(name: str) -> str:
    """Freeze each invariant set and enable only its reviewed semantic mutant."""
    expected = CASES[name]
    if name.startswith("publication-"):
        keys = ("RequireIntent", "RequireSync", "RejectDamage", "StopOnUnlink")
        disabled = {"publication-no-intent": "RequireIntent",
                    "publication-no-sync": "RequireSync",
                    "publication-ignore-damage": "RejectDamage",
                    "publication-ignore-unlink": "StopOnUnlink"}.get(name)
        safe = ("TypeOK IndexedData PinnedData AllManifestsValid ReaderData "
                "ExclusiveCustody OutputHasIntent CompletedDurable DamageBlocksCollection")
        lines = ["SPECIFICATION Spec", "CONSTANTS " + " ".join(
            f"{key} = {'FALSE' if key == disabled else 'TRUE'}" for key in keys),
            "CHECK_DEADLOCK FALSE"]
        if name in ("publication-safe", "publication-recovery"):
            lines.append(f"INVARIANTS {safe}")
            if expected:
                lines.append(f"INVARIANT {expected}")
        else:
            lines.append(f"INVARIANTS TypeOK {expected}")
    else:
        lines = [f"CONSTANT UseCustody = {'FALSE' if name == 'no-custody' else 'TRUE'}",
                 f"CONSTANT RespectPins = {'FALSE' if name == 'no-pins' else 'TRUE'}",
                 "SPECIFICATION Spec", "CHECK_DEADLOCK FALSE"]
        properties = "TypeOK IndexedData"
        if name != "no-custody":
            properties += " PinnedRecoveryData ReaderData"
        if name == "safe":
            properties += " ExclusiveCustody"
        elif name.startswith("witness-"):
            properties += f" {expected}"
        lines.append(f"INVARIANTS {properties}")
    return "\n".join(lines) + "\n"


def validate_configs(sources: dict[str, bytes]) -> None:
    for name in CASES:
        if sources[f"{name}.cfg"].decode("utf-8").strip() != expected_config(name).strip():
            raise ValueError(f"{name}.cfg differs from the reviewed invariant/mutation contract")


def digest(path: Path) -> str:
    sha = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            sha.update(block)
    return sha.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--java", default="java", help="Java executable (11 or newer)")
    parser.add_argument("--tlc-jar", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="new evidence directory")
    args = parser.parse_args()
    java = Path(shutil.which(args.java) or args.java).resolve(strict=True)
    jar = args.tlc_jar.resolve(strict=True)
    if digest(jar) != TLC_SHA256:
        parser.error("TLC jar checksum does not match the pinned v1.7.4 artifact")
    source = Path(__file__).resolve().parent
    sources = sorted([source / "check.py", source / "test_check.py", source / "README.md",
                      source / "Vault.tla", source / "Publication.tla",
                      *(source / f"{name}.cfg" for name in CASES)])
    source_bytes = {path.name: path.read_bytes() for path in sources}
    validate_configs(source_bytes)
    source_hashes = {name: hashlib.sha256(data).hexdigest()
                     for name, data in source_bytes.items()}
    java_sha256 = digest(java)
    runner_sha256 = digest(RUNNER)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    output.chmod(0o700)
    env = dict(os.environ)
    for key in ("JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "CLASSPATH"):
        env.pop(key, None)
    version_code, version_log, version_timeout, version_limit = runner.run_owned(
        [str(java), "-version"], output, env, 10, 65536)
    (output / "java-version.log").write_text(version_log)
    if version_code != 0 or version_timeout or version_limit:
        parser.error("Java could not report its version; see java-version.log")

    results = []
    for name, expected_invariant in CASES.items():
        case = output / name
        case.mkdir()
        model = "Publication" if name.startswith("publication-") else "Vault"
        for filename in (f"{model}.tla", f"{name}.cfg"):
            (case / filename).write_bytes(source_bytes[filename])
        java_home = case / "jvm-home"
        (java_home / ".tlaplus").mkdir(parents=True)
        (java_home / ".tlaplus" / "esc.txt").write_text("NO_STATISTICS\n")
        (case / "tmp").mkdir()
        argv = [
            str(java), "-XX:+UseParallelGC", "-Xmx256m",
            f"-Duser.home={java_home}", f"-Djava.io.tmpdir={case / 'tmp'}",
            "-cp", str(jar), "tlc2.TLC", "-workers", "1", "-seed", "1",
            "-fp", "0", "-coverage", "1", "-metadir", str(case / "states"),
            "-config", f"{name}.cfg", model,
        ]
        exit_code, raw, timeout, log_limit = runner.run_owned(argv, case, env)
        log = output / f"{name}.log"
        log.write_text(raw)
        admitted = runner.admit_result(raw, exit_code, timeout, log_limit, expected_invariant)
        results.append({
            "case": name, "expected_invariant_violation": expected_invariant,
            **admitted, "timeout": timeout, "log_limit": log_limit, "exit_code": exit_code,
            "argv": argv, "log": log.name,
            "log_sha256": digest(log),
        })
        print(f"{name}: {'PASS' if admitted['passed'] else 'FAIL'} {admitted['states']}", flush=True)

    inputs_unchanged = (
        all(digest(path) == source_hashes[path.name] for path in sources)
        and digest(jar) == TLC_SHA256 and digest(java) == java_sha256
        and digest(RUNNER) == runner_sha256
    )
    receipt = {
        "schema": "gobstopper.vault-model-check.v1",
        "scope": "finite custody/order model, not verification of production Rust or filesystem",
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "passed": inputs_unchanged and all(result["passed"] for result in results),
        "inputs_unchanged": inputs_unchanged,
        "source_sha256": source_hashes,
        "owned_runner_sha256": runner_sha256,
        "bounds": {"vault": {"publishers": 2, "readers": 1, "prune_cycles": 1,
                             "snapshot_identities": 3, "chunk_identities": 4},
                   "publication": {"publishers": 1, "readers": 1, "prune_cycles": 2,
                                   "publisher_restarts": 1},
                   "fairness": "none", "per_case_seconds": 45, "jvm_heap_mib": 256,
                   "log_bytes": runner.MAX_LOG_BYTES},
        "java": {"path": str(java), "sha256": java_sha256,
                 "version_log_sha256": digest(output / "java-version.log")},
        "tlc_jar": {"path": str(jar), "sha256": digest(jar), "release": "v1.7.4"},
        "results": results,
    }
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(f"receipt: {output / 'receipt.json'}", flush=True)
    return 0 if receipt["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

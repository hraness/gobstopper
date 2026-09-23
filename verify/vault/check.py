#!/usr/bin/env python3
"""Check the finite vault protocol and semantic negative controls with pinned TLC."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess


TLC_SHA256 = "936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
CASES = {
    "safe": None,
    "no-custody": "IndexedData",
    "no-pins": "PinnedRecoveryData",
    "witness-completion": "NoSuccessfulComposition",
    "witness-pending": "NoPendingRecovery",
}


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
    sources = sorted([source / "check.py", source / "Vault.tla", *source.glob("*.cfg")])
    source_bytes = {path.name: path.read_bytes() for path in sources}
    source_hashes = {name: hashlib.sha256(data).hexdigest()
                     for name, data in source_bytes.items()}
    java_sha256 = digest(java)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    for key in ("JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "CLASSPATH"):
        env.pop(key, None)
    version = subprocess.run(
        [str(java), "-version"], env=env, capture_output=True, text=True, timeout=10,
    )
    (output / "java-version.log").write_text(version.stdout + version.stderr)
    if version.returncode != 0:
        parser.error("Java could not report its version; see java-version.log")

    results = []
    for name, expected_invariant in CASES.items():
        case = output / name
        case.mkdir()
        for filename in ("Vault.tla", f"{name}.cfg"):
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
            "-config", f"{name}.cfg", "Vault",
        ]
        process = subprocess.Popen(
            argv, cwd=case, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, start_new_session=True,
        )
        timeout = False
        try:
            stdout, stderr = process.communicate(timeout=45)
        except subprocess.TimeoutExpired:
            timeout = True
            # Only the exact process group created for this case is terminated.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            stdout, stderr = process.communicate()
        raw = stdout + stderr
        log = output / f"{name}.log"
        log.write_text(raw)
        stats = re.search(
            r"([\d,]+) states generated, ([\d,]+) distinct states found, "
            r"([\d,]+) states left on queue", raw,
        )
        counts = {
            key: int(value.replace(",", ""))
            for key, value in zip(("generated", "distinct", "queued"), stats.groups())
        } if stats else None
        if expected_invariant is None:
            passed = (
                not timeout and process.returncode == 0 and counts is not None
                and counts["distinct"] > 100 and counts["queued"] == 0
                and "Model checking completed. No error has been found." in raw
            )
        else:
            passed = (
                not timeout and process.returncode == 12 and counts is not None
                and f"Invariant {expected_invariant} is violated" in raw
                and "The behavior up to this point is" in raw and "State 2:" in raw
            )
        results.append({
            "case": name, "expected_invariant_violation": expected_invariant,
            "passed": passed, "timeout": timeout, "exit_code": process.returncode,
            "states": counts, "argv": argv, "log": log.name,
            "log_sha256": digest(log),
        })
        print(f"{name}: {'PASS' if passed else 'FAIL'} {counts}", flush=True)

    inputs_unchanged = (
        all(digest(path) == source_hashes[path.name] for path in sources)
        and digest(jar) == TLC_SHA256 and digest(java) == java_sha256
    )
    receipt = {
        "schema": "gobstopper.vault-model-check.v1",
        "scope": "finite custody/order model, not verification of production Rust or filesystem",
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "passed": inputs_unchanged and all(result["passed"] for result in results),
        "inputs_unchanged": inputs_unchanged,
        "source_sha256": source_hashes,
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

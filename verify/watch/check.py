#!/usr/bin/env python3
"""Check bounded native-dispatch safety, semantic mutants and reachability."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import errno
import hashlib
import json
import os
from pathlib import Path
import re
import select
import shutil
import signal
import subprocess
import sys
import time


TLC_SHA256 = "936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
MAX_LOG_BYTES = 32 * 1024 * 1024
TIMEOUT_SECONDS = 45
SAFE_INVARIANTS = (
    "TypeOK", "SingleCustody", "JournalValid", "DispatchRecorded",
    "RecoveryPinned", "AtMostOnce", "NoUnresolvedReplay", "AppliedNeedsTerminal",
    "NoopNeedsTerminal", "ReconcileNeedsExactEvidence", "AtMostOneInflight", "NoFallback",
)
# name: (kind, enabled semantic mutation, expected invariant violation)
CASES = {
    "safe": ("safety", None, None),
    "safe-assumed-correlation": ("safety", None, None),
    "replay-after-expiry": ("mutant", "AllowExpiredReplay", "NoUnresolvedReplay"),
    "success-on-ack": ("mutant", "AllowAckSuccess", "AppliedNeedsTerminal"),
    "missing-pin": ("mutant", "OmitRecoveryPin", "RecoveryPinned"),
    "automatic-fallback": ("mutant", "AllowFallback", "NoFallback"),
    "fabricated-reconciliation": ("mutant", "AllowFabricatedReconciliation",
                                  "ReconcileNeedsExactEvidence"),
    "witness-applied": ("witness", None, "NoAppliedWitness"),
    "witness-noop": ("witness", None, "NoNoopWitness"),
    "witness-unknown": ("witness", None, "NoUnknownWitness"),
    "witness-reconciled": ("witness", None, "NoReconciledWitness"),
    "witness-crash": ("witness", None, "NoCrashWitness"),
    "witness-terminal-before-ack": ("witness", None, "NoTerminalBeforeAckWitness"),
}
MUTATIONS = ("AllowExpiredReplay", "AllowAckSuccess", "OmitRecoveryPin", "AllowFallback",
             "AllowFabricatedReconciliation")


def digest(path: Path) -> str:
    sha = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            sha.update(block)
    return sha.hexdigest()


def expected_config(name: str) -> str:
    _, mutation, expected = CASES[name]
    lines = ["SPECIFICATION Spec", "CONSTANTS",
             "    Watchers = {watcher1, watcher2}", "    MaxOperations = 2"]
    exact = "FALSE" if name == "safe-assumed-correlation" else "TRUE"
    lines.append(f"    ExactCorrelation = {exact}")
    lines.extend(f"    {key} = {'TRUE' if key == mutation else 'FALSE'}"
                 for key in MUTATIONS)
    lines.append("CHECK_DEADLOCK FALSE")
    lines.extend(f"INVARIANT {invariant}"
                 for invariant in (SAFE_INVARIANTS if expected is None else (expected,)))
    return "\n".join(lines) + "\n"


def validate_configs(sources: dict[str, bytes]) -> None:
    """Do not silently reduce bounds, omit an invariant or mutate a witness."""
    for name in CASES:
        if sources[f"{name}.cfg"].decode("utf-8").strip() != expected_config(name).strip():
            raise ValueError(f"{name}.cfg differs from the checker's explicit bounds/case")


def admit_result(raw: str, exit_code: int, timeout: bool, log_limit: bool,
                 expected_invariant: str | None) -> dict:
    matches = re.findall(
        r"([\d,]+) states generated, ([\d,]+) distinct states found, "
        r"([\d,]+) states left on queue", raw,
    )
    counts = {
        key: int(value.replace(",", ""))
        for key, value in zip(("generated", "distinct", "queued"), matches[-1])
    } if matches else None
    complete = (not timeout and not log_limit and len(matches) == 1
                and counts is not None and counts["generated"] >= counts["distinct"]
                and counts["distinct"] >= counts["queued"])
    errors = re.findall(r"^Error:.*$", raw, re.MULTILINE)
    if expected_invariant is None:
        passed = (
            complete and exit_code == 0 and counts["distinct"] > 100
            and counts["queued"] == 0
            and "Model checking completed. No error has been found." in raw
            and not errors
        )
    else:
        passed = (
            complete and exit_code == 12 and counts["distinct"] > 1
            and errors == [f"Error: Invariant {expected_invariant} is violated.",
                           "Error: The behavior up to this point is:"]
            and re.search(r"^Error: Invariant " + re.escape(expected_invariant)
                          + r" is violated\.$", raw, re.MULTILINE) is not None
            and "Error: The behavior up to this point is:" in raw
            and re.search(r"^State 2: ", raw, re.MULTILINE) is not None
        )
    return {"passed": bool(passed), "states": counts}


def run_owned(argv: list[str], cwd: Path, env: dict[str, str],
              timeout_seconds: float = TIMEOUT_SECONDS,
              max_log_bytes: int = MAX_LOG_BYTES) -> tuple[int, str, bool, bool]:
    """Bound output/time without pipe threads, reserving PID until group cleanup.

    Unix WNOWAIT observes the owned leader without reaping it. The exact group
    created by start_new_session is signalled before wait(), including normal
    completion. An escaped process is outside that group and cannot keep a
    blocking pipe reader alive: all reads are nonblocking and bounded.
    """
    if os.name != "posix" or not hasattr(os, "WNOWAIT"):
        raise RuntimeError("the model runner requires Unix waitid(WNOWAIT)")
    process = subprocess.Popen(
        argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True,
    )
    raw = bytearray()
    timeout = False
    log_limit = False
    exited_observed = False
    eof = False
    assert process.stdout is not None
    stream = process.stdout
    deadline = time.monotonic() + timeout_seconds

    def drain_once() -> bool:
        nonlocal log_limit, eof
        try:
            chunk = os.read(stream.fileno(), min(8192, max_log_bytes - len(raw) + 1))
        except BlockingIOError:
            return False
        if not chunk:
            eof = True
        if len(raw) + len(chunk) > max_log_bytes:
            log_limit = True
            raw.extend(chunk[:max_log_bytes - len(raw)])
            return False
        raw.extend(chunk)
        return bool(chunk)

    try:
        os.set_blocking(stream.fileno(), False)
        while True:
            drain_once()
            if log_limit:
                break
            # Do not use poll(), communicate() or wait() before group cleanup:
            # those may reap the leader and release its numeric PID identity.
            exited = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            if exited is not None:
                exited_observed = True
                break
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                timeout = True
                break
            select.select([stream], [], [], min(remaining, 0.01))
    finally:
        cleanup_error = None
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except OSError as error:
            cleanup_error = error
        try:
            process.wait(timeout=10)
            while not log_limit and drain_once():
                pass
        finally:
            stream.close()
        # Darwin killpg1 filters zombies before counting signalled members and
        # can return EPERM for a group containing only the unreaped leader.
        # Accept that edge only after WNOWAIT observed exit AND pipes reached
        # EOF. Genuine live-leader / inherited-pipe signal denial still fails.
        # This assumes the pinned Java does not spawn privilege-changing or
        # detached children; this runner is not a sandbox for arbitrary code.
        benign_zombie = (
            cleanup_error is not None and cleanup_error.errno == errno.EPERM
            and sys.platform == "darwin" and exited_observed and eof
        )
        if cleanup_error is not None and not benign_zombie:
            raise cleanup_error
    return process.returncode, raw.decode("utf-8", errors="replace"), timeout, log_limit


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
    paths = [source / "check.py", source / "test_check.py", source / "README.md", source / "Watch.tla",
             *(source / f"{name}.cfg" for name in CASES)]
    source_bytes = {path.name: path.read_bytes() for path in paths}
    validate_configs(source_bytes)
    source_hashes = {name: hashlib.sha256(data).hexdigest()
                     for name, data in source_bytes.items()}
    java_sha256 = digest(java)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    output.chmod(0o700)
    env = dict(os.environ)
    for key in ("JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "CLASSPATH"):
        env.pop(key, None)
    version_code, version_log, version_timeout, version_limit = run_owned(
        [str(java), "-version"], output, env, 10, 65536)
    (output / "java-version.log").write_text(version_log)
    if version_code != 0 or version_timeout or version_limit:
        parser.error("Java could not report its version; see java-version.log")

    results = []
    for name, (kind, mutation, expected) in CASES.items():
        case = output / name
        case.mkdir()
        for filename in ("Watch.tla", f"{name}.cfg"):
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
            "-config", f"{name}.cfg", "Watch",
        ]
        exit_code, raw, timeout, log_limit = run_owned(argv, case, env)
        log = output / f"{name}.log"
        log.write_text(raw)
        admitted = admit_result(raw, exit_code, timeout, log_limit, expected)
        result = {
            "case": name, "kind": kind, "mutation": mutation,
            "expected_invariant_violation": expected, **admitted,
            "timeout": timeout, "log_limit": log_limit, "exit_code": exit_code,
            "argv": argv, "log": log.name, "log_sha256": digest(log),
        }
        results.append(result)
        print(f"{name}: {'PASS' if result['passed'] else 'FAIL'} {result['states']}", flush=True)

    inputs_unchanged = (
        all(digest(path) == source_hashes[path.name] for path in paths)
        and digest(jar) == TLC_SHA256 and digest(java) == java_sha256
    )
    receipt = {
        "schema": "gobstopper.watch-model-check.v1",
        "scope": "finite native-dispatch protocol; no Rust/OS/provider refinement theorem",
        "bounds": {"targets": 1, "watchers": 2, "operations": 2,
                   "records_per_operation": 4,
                   "generation_changes": 1, "fairness": "none",
                   "durability": "atomic successful append; crashes release volatile custody"},
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "passed": inputs_unchanged and all(result["passed"] for result in results),
        "inputs_unchanged": inputs_unchanged, "source_sha256": source_hashes,
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

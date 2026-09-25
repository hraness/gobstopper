#!/usr/bin/env python3
"""Run the reviewed bounded synthetic fault/sequence suites and retain exact evidence."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import importlib.util
import json
import os
from pathlib import Path
import re
import resource
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
RUNNER = ROOT / "verify/watch/check.py"
spec = importlib.util.spec_from_file_location("watch_runner", RUNNER)
assert spec and spec.loader
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

TOTAL_SECONDS = 900
MAX_LOG_BYTES = 8 * 1024 * 1024
MAX_CHILD_RSS_BYTES = 2 * 1024 * 1024 * 1024
SEED = 0x6a09e667f3bcc909
SUITES = {
    "sequence": ("gobstopper-adapters", ["--test", "sequence"], 1, 150),
    "storage": ("gobstopper-adapters", ["--lib", "storage_tests::"], 16, 180),
    "vault-metadata": ("gobstopper-adapters", ["--lib", "vault::accounting::tests::"], 13, 90),
    "vault-accounting-cli": ("gobstopper", ["--test", "vault_accounting"], 2, 90),
    "journal": ("gobstopper", ["--bin", "gobstopper", "native_operations::tests::"], 10, 180),
    "watch": ("gobstopper", ["--test", "watch"], 29, 240),
    "claude-process": ("gobstopper-adapters", ["--test", "native_process"], 2, 60),
    "acp-process": ("gobstopper-adapters", ["--lib", "devin::tests::acp_"], 10, 90),
    "codex-process": ("gobstopper", ["--bin", "gobstopper", "tests::codex_"], 11, 90),
    "plugins": ("gobstopper-adapters", ["--test", "plugin_contract"], 11, 90),
    "events": ("gobstopper-core", ["--lib", "events::tests::"], 15, 60),
    "monitor": (None, ["scripts/test_monitor.py", "-v"], 40, 90),
}


def admitted_inventory(document: dict) -> list[dict]:
    suites = document["suites"]
    if document["schema"] != "gobstopper.stress-suites.v1" or len(suites) != len(SUITES):
        raise ValueError("unreviewed stress inventory schema/size")
    if [suite["name"] for suite in suites] != list(SUITES):
        raise ValueError("unreviewed stress suite order or names")
    for suite in suites:
        package, targets, count, seconds = SUITES[suite["name"]]
        expected = (["cargo", "test", "-p", package, "--locked", *targets,
                     "--", "--nocapture", "--test-threads=1"] if package
                    else ["python3", *targets])
        name_pattern = r"MonitorTests\.test_[A-Za-z0-9_]+" if not package else r"[A-Za-z_][A-Za-z0-9_:]*"
        source_prefix = "crates/" if package else "scripts/"
        if (suite["argv"] != expected or suite["seconds"] != seconds
                or len(suite["tests"]) != count or len(set(suite["tests"])) != count
                or not all(re.fullmatch(name_pattern, name) for name in suite["tests"])
                or not suite["source"].startswith(source_prefix) or ".." in Path(suite["source"]).parts):
            raise ValueError(f"unreviewed command/bounds/test inventory: {suite['name']}")
    return suites


def admit_tests(suite: dict, code: int, log: str, timeout: bool, capped: bool) -> dict:
    if suite["name"] == "monitor":
        return admit_python_tests(suite, code, log, timeout, capped)
    observed = {}
    pending = None
    malformed = False
    for line in log.splitlines():
        test = re.fullmatch(r"test ([A-Za-z_][A-Za-z0-9_:]*) \.\.\. (.*)", line)
        if test:
            if pending or test[1] in observed:
                malformed = True
            pending = test[1]
            status = test[2].strip()
        else:
            status = line.strip()
        if pending and status in {"ok", "FAILED", "ignored"}:
            observed[pending] = status
            pending = None
    summaries = re.findall(r"^test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored; "
                           r"(\d+) measured; (\d+) filtered out;", log, re.M)
    passed = (code == 0 and not timeout and not capped and not malformed and pending is None
              and set(observed) == set(suite["tests"]) and all(s == "ok" for s in observed.values())
              and len(summaries) == 1 and summaries[0][:5] == ("ok", str(len(observed)), "0", "0", "0")
              and "could not compile" not in log and not re.search(r"error\[E\d+\]", log))
    metrics = None
    if suite["name"] == "sequence":
        rows = re.findall(r"sequence receipt: seed=(\d+), steps=(\d+), corruption_recoveries=(\d+), "
                          r"peak_files=(\d+), peak_bytes=(\d+), elapsed_ms=(\d+)", log)
        if len(rows) == 1:
            metrics = dict(zip(("seed", "steps", "corruption_recoveries", "peak_files", "peak_bytes", "elapsed_ms"),
                               map(int, rows[0])))
        passed = (passed and metrics is not None and metrics["seed"] == SEED
                  and metrics["steps"] == 64 and metrics["corruption_recoveries"] == 16
                  and 0 < metrics["peak_files"] <= 1200 and 0 < metrics["peak_bytes"] <= 16 * 1024 * 1024
                  and 0 <= metrics["elapsed_ms"] < 90_000)
    return {"passed": bool(passed), "passed_tests": sorted(name for name, state in observed.items() if state == "ok"),
            "observed_tests": observed, "sequence_metrics": metrics}


def admit_python_tests(suite: dict, code: int, log: str, timeout: bool, capped: bool) -> dict:
    observed = {}
    duplicate = False
    for line in log.splitlines():
        result = re.fullmatch(r"(test_[A-Za-z0-9_]+) \(__main__\.(MonitorTests\.\1)\) \.\.\. (.*)", line)
        if result:
            if result[2] in observed:
                duplicate = True
            observed[result[2]] = result[3]
    summaries = re.findall(r"^Ran (\d+) tests? in [0-9.]+s$", log, re.M)
    passed = (code == 0 and not timeout and not capped and not duplicate
              and set(observed) == set(suite["tests"]) and all(status == "ok" for status in observed.values())
              and summaries == [str(len(observed))] and re.findall(r"^OK.*$", log, re.M) == ["OK"]
              and not re.search(r"^(FAILED|ERROR|FAIL|Traceback|error\[E\d+\])", log, re.M))
    return {"passed": passed, "passed_tests": sorted(name for name, state in observed.items() if state == "ok"),
            "observed_tests": observed, "sequence_metrics": None}


def source_paths() -> list[Path]:
    paths = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", RUNNER,
             ROOT / "scripts/monitor.py", ROOT / "scripts/test_monitor.py"]
    paths += [path for path in HERE.iterdir() if path.is_file() and not path.name.startswith(".")]
    # Include fixture scripts/data and all modules loaded by these test binaries.
    for crate in (ROOT / "crates").iterdir():
        if crate.is_dir():
            paths.append(crate / "Cargo.toml")
            for directory in ("src", "tests"):
                paths += [path for path in (crate / directory).rglob("*") if path.is_file()]
    return sorted(set(paths))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="new private evidence directory")
    args = parser.parse_args()
    if sys.platform not in {"linux", "darwin"}:
        parser.error("the reviewed process/custody stress suites require supported Unix")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    output.chmod(0o700)
    temp = output / "fixture-tmp"
    temp.mkdir(mode=0o700)
    suites = admitted_inventory(json.loads((HERE / "suites.json").read_text()))
    source_hashes = {str(path.relative_to(ROOT)): runner.digest(path) for path in source_paths()}
    # Preserve rustup's argv[0] dispatch name instead of invoking its resolved
    # symlink destination as `rustup test` / `rustup --version`.
    cargo = Path(shutil.which("cargo") or "cargo").absolute()
    rustc = Path(shutil.which("rustc") or "rustc").absolute()
    tool_hashes = {str(path): runner.digest(path) for path in (cargo, rustc, Path(sys.executable).resolve())}
    env = dict(os.environ)
    for key in list(env):
        if (key.startswith("GOBSTOPPER") or key in {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC", "CARGO_BUILD_RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"}
                or key.startswith("CARGO_TARGET_") and key.endswith("_RUSTFLAGS")):
            env.pop(key)
    env.update(CARGO_NET_OFFLINE="true", CARGO_BUILD_JOBS="2", CARGO_TERM_COLOR="never",
               RUST_BACKTRACE="0", TMPDIR=str(temp), TMP=str(temp), TEMP=str(temp))
    started = time.monotonic()
    deadline = started + TOTAL_SECONDS
    results = []
    identity_logs = {}
    receipt = {"schema": "gobstopper.bounded-stress.v1", "observed_at": datetime.now(timezone.utc).isoformat(),
               "source_sha256": source_hashes, "tool_sha256": tool_hashes, "platform": sys.platform,
               "passed": False, "results": results, "identity_log_sha256": identity_logs,
               "bounds": {"total_seconds": TOTAL_SECONDS, "max_log_bytes_per_command": MAX_LOG_BYTES,
                          "cargo_jobs": 2, "test_threads": 1, "max_observed_single_child_rss_bytes": MAX_CHILD_RSS_BYTES,
                          "sequence_steps": 64, "sequence_corruption_recoveries": 16,
                          "sequence_post_step_file_limit": 1200, "sequence_post_step_bytes_limit": 16 * 1024 * 1024,
                          "sequence_elapsed_ms_limit": 90_000},
               "limits": ["synthetic bounded fixtures only; no live provider qualification",
                          "RUSAGE_CHILDREN maximum is largest reaped-child high-water, not simultaneous tree RSS or an enforced OS allocation limit",
                          "child collection assertions cover owned fixture descendants, not a hard process-count sandbox",
                          "wall deadline cannot preempt uninterruptible kernel I/O; wait/cleanup failure fails admission",
                          "no physical power-loss, network-filesystem or real-full-volume qualification",
                          "native journal faults are covered; not every target-registry persistence boundary has a process-crash fault seam"]}
    try:
        # Record and invoke the actual selected compiler/Cargo, not merely the
        # rustup proxy bytes. The standard library, linker and OS remain TCB.
        code, log, timeout, capped = runner.run_owned([str(rustc), "--print", "sysroot"], ROOT, env, 10, 32768)
        identity_path = output / "rust-sysroot.log"
        identity_path.write_text(log)
        identity_logs[identity_path.name] = runner.digest(identity_path)
        if code or timeout or capped or len(log.splitlines()) != 1 or not Path(log.strip()).is_absolute():
            raise ValueError("cannot identify selected Rust sysroot")
        toolchain = Path(log.strip())
        cargo = toolchain / "bin/cargo"
        rustc = toolchain / "bin/rustc"
        for path in (cargo, rustc):
            tool_hashes[str(path)] = runner.digest(path)
        env["RUSTC"] = str(rustc)
        for name, argv in (("cargo-version", [str(cargo), "--version"]), ("rust-version", [str(rustc), "--version", "--verbose"])):
            code, log, timeout, capped = runner.run_owned(argv, ROOT, env, 10, 32768)
            identity_path = output / f"{name}.log"
            identity_path.write_text(log)
            identity_logs[identity_path.name] = runner.digest(identity_path)
            if code or timeout or capped:
                raise ValueError(f"cannot identify {name}")
        for suite in suites:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("stress aggregate deadline exceeded")
            executable = cargo if suite["argv"][0] == "cargo" else Path(sys.executable).resolve()
            argv = [str(executable), *suite["argv"][1:]]
            before = time.monotonic()
            code, log, timeout, capped = runner.run_owned(argv, ROOT, env, min(suite["seconds"], remaining), MAX_LOG_BYTES)
            elapsed = time.monotonic() - before
            log_path = output / f"{suite['name']}.log"
            log_path.write_text(log)
            result = admit_tests(suite, code, log, timeout, capped)
            rss = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
            rss_bytes = int(rss if sys.platform == "darwin" else rss * 1024)
            result["passed"] &= rss_bytes <= MAX_CHILD_RSS_BYTES and time.monotonic() <= deadline
            result.update(case=suite["name"], argv=argv, exit_code=code, timeout=timeout, log_limit=capped,
                          log_sha256=runner.digest(log_path), elapsed_seconds=elapsed,
                          largest_reaped_child_rss_bytes=rss_bytes)
            results.append(result)
            print(f"{suite['name']}: {'PASS' if result['passed'] else 'FAIL'} ({len(result['passed_tests'])} passed)", flush=True)
            if not result["passed"]:
                raise ValueError(f"stress admission failed: {suite['name']}")
        receipt["inputs_unchanged"] = (
            all(runner.digest(ROOT / name) == value for name, value in source_hashes.items())
            and all(runner.digest(Path(name)) == value for name, value in tool_hashes.items()))
        receipt["passed"] = receipt["inputs_unchanged"] and len(results) == len(suites)
    except (OSError, ValueError, KeyError, TypeError, RuntimeError, subprocess.SubprocessError) as error:
        receipt["error"] = str(error)
    finally:
        receipt["elapsed_seconds"] = time.monotonic() - started
        # Include the final source/tool rehash in admission. Receipt serialization
        # is bookkeeping outside this measured workload, not another child run.
        receipt["passed"] &= receipt["elapsed_seconds"] <= TOTAL_SECONDS
        (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        print(f"receipt: {output / 'receipt.json'}", flush=True)
    return 0 if receipt["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

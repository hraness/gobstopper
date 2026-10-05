import argparse
import datetime
import errno
import hashlib
import json
import math
import os
from pathlib import Path
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import time

SCHEMA = "gobstopper/offline-tail-comparison-v1"
MAX_SESSIONS = 8
MAX_SOURCE_BYTES = 16 * 1024 * 1024
MAX_BINARY_BYTES = 128 * 1024 * 1024
MAX_RECORD_BYTES = 1024 * 1024
MAX_RECORDS = 8192
MAX_CAPTURE_BYTES = 2 * 1024 * 1024
MAX_RECEIPT_BYTES = 1024 * 1024
MAX_THRESHOLD = 1_000_000
MAX_TIMEOUT = 120
READ_SECONDS = 10
STABILITY_SECONDS = 0.1
CLEANUP_SECONDS = 0.25
TAILS = (0, 40)
SHARED_OPTIONS = (
    ("--keep-recent", 3),
    ("--result-max-chars", 500),
    ("--carry-max-chars", 24_000),
    ("--evidence-max-bytes", 256 * 1024),
    ("--evidence-max-chars", 32_000),
    ("--fixed-tokens", 20_000),
)
COUNT_FIELDS = (
    "requests", "compacted", "reused_prefix", "over_threshold_after",
    "raised_threshold", "pairing_violations", "source_pairing_violations",
    "repeated_reads", "repeated_reads_covered", "back_to_back_compactions",
    "usage_requests", "calibration_samples",
)
ESTIMATE_FIELDS = (
    "peak_est_tokens_in", "peak_est_tokens_out", "last_est_tokens_in",
    "last_est_tokens_out", "total_est_tokens_in", "total_est_tokens_out",
    "est_cache_read_tokens", "est_cache_write_tokens",
)
PROJECTION_FIELDS = ("peak_reported_tokens_out", "reported_over_threshold")
NULLABLE_FIELDS = (
    "first_violation", "min_compaction_gap", "reported_ratio_min_permille",
    "reported_ratio_median_permille", "reported_ratio_max_permille",
)
METRIC_FIELDS = COUNT_FIELDS + ESTIMATE_FIELDS + PROJECTION_FIELDS + ("last_ratio_permille",) + NULLABLE_FIELDS
INVARIANT_FIELDS = (
    "requests", "peak_est_tokens_in", "last_est_tokens_in", "total_est_tokens_in",
    "source_pairing_violations", "repeated_reads", "usage_requests",
    "calibration_samples", "last_ratio_permille",
    "reported_ratio_min_permille", "reported_ratio_median_permille",
    "reported_ratio_max_permille",
)
COMPACTION_FIELDS = (
    "request", "est_tokens_before", "est_tokens_after", "messages_before",
    "messages_after", "head_tokens", "summary_tokens", "tail_tokens", "carry_chars",
)


class ComparisonError(Exception):
    pass


class ComparisonInterrupted(BaseException):
    pass


class Parser(argparse.ArgumentParser):
    def error(self, _message):
        raise ComparisonError("invalid_arguments")


def interrupt(_signal, _frame):
    for name in (signal.SIGINT, signal.SIGTERM):
        signal.signal(name, signal.SIG_IGN)
    raise ComparisonInterrupted()


def number(value):
    return type(value) is int and 0 <= value <= 2**64 - 1


def strict_json(raw):
    def fields(pairs):
        result = {}
        for key, value in pairs:
            key.encode("utf-8")
            if key in result:
                raise ValueError("duplicate_json_field")
            result[key] = value
        return result

    def invalid(_value):
        raise ValueError("nonfinite_json_number")

    def finite(value):
        result = float(value)
        if not math.isfinite(result):
            raise ValueError("nonfinite_json_number")
        return result

    return json.loads(raw.decode("utf-8"), object_pairs_hook=fields,
                      parse_constant=invalid, parse_float=finite,
                      parse_int=lambda value: -0.0 if value == "-0" else int(value))


def absolute_path(value):
    if (not isinstance(value, str) or not value or len(os.fsencode(value)) > 4096
            or any(ord(char) < 32 or ord(char) == 127 for char in value)):
        raise ComparisonError("invalid_path")
    path = Path(value)
    if not path.is_absolute() or ".." in path.parts or path == Path("/"):
        raise ComparisonError("absolute_paths_required")
    return path


def identity(path):
    return hashlib.sha256(os.fsencode(str(path))).hexdigest()


def open_directory(path):
    descriptor = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parts[1:]:
            next_descriptor = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
                                      | os.O_NONBLOCK, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = next_descriptor
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def fingerprint(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_uid, info.st_nlink,
            info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def read_file(path, limit, kind, deadline, destination=None, capture=False):
    directory = None
    descriptor = None
    try:
        directory = open_directory(path.parent)
        info = os.stat(path.name, dir_fd=directory, follow_symlinks=False)
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or info.st_mode & 0o022):
            raise ComparisonError(f"{kind}_unsafe")
        if not 0 < info.st_size <= limit:
            raise ComparisonError(f"{kind}_size_limit")
        if kind == "binary" and not info.st_mode & 0o111:
            raise ComparisonError("binary_not_executable")
        descriptor = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                             dir_fd=directory)
        before = fingerprint(os.fstat(descriptor))
        if before != fingerprint(info):
            raise ComparisonError(f"{kind}_changed")
        digest = hashlib.sha256()
        size = 0
        chunks = []
        read_deadline = min(deadline, time.monotonic() + READ_SECONDS)
        while True:
            if time.monotonic() >= read_deadline:
                raise ComparisonError("read_timeout")
            chunk = os.read(descriptor, min(65536, limit + 1 - size))
            if not chunk:
                break
            size += len(chunk)
            if size > limit:
                raise ComparisonError(f"{kind}_size_limit")
            digest.update(chunk)
            if destination is not None:
                view = memoryview(chunk)
                while view:
                    written = os.write(destination, view)
                    if written <= 0:
                        raise ComparisonError("snapshot_write_failed")
                    view = view[written:]
            if capture:
                chunks.append(chunk)
        after = fingerprint(os.fstat(descriptor))
        current = fingerprint(os.stat(path.name, dir_fd=directory, follow_symlinks=False))
        if before != after or before != current or size != info.st_size:
            raise ComparisonError(f"{kind}_changed")
        result = {"sha256": digest.hexdigest(), "bytes": size, "fingerprint": before}
        if capture:
            result["raw"] = b"".join(chunks)
        return result
    except OSError:
        raise ComparisonError(f"{kind}_unavailable") from None
    finally:
        if descriptor is not None:
            os.close(descriptor)
        if directory is not None:
            os.close(directory)


def copy_private(source, destination, limit, kind, deadline, executable=False, capture=False):
    descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        result = read_file(source, limit, kind, deadline, destination=descriptor, capture=capture)
        os.fchmod(descriptor, 0o500 if executable else 0o400)
        os.fsync(descriptor)
        return result
    finally:
        os.close(descriptor)


def transcript_shape(raw):
    dialect = None
    records = 0
    lines = raw.split(b"\n")
    if len(lines) > MAX_RECORDS + 1:
        raise ComparisonError("source_record_limit")
    for line in lines:
        if len(line) > MAX_RECORD_BYTES:
            raise ComparisonError("source_record_limit")
        if not line.strip():
            continue
        try:
            value = strict_json(line)
        except (ValueError, UnicodeError, RecursionError):
            raise ComparisonError("invalid_transcript_json") from None
        if not isinstance(value, dict):
            raise ComparisonError("invalid_transcript_json")
        records += 1
        if records > MAX_RECORDS:
            raise ComparisonError("source_record_limit")
        if records <= 24 and dialect is None:
            if "payload" in value and ("ordinal" in value or value.get("type") in (
                    "session_meta", "response_item", "compacted", "token_usage_record",
                    "event_msg", "turn_context")):
                dialect = "openai-responses"
            elif "sessionId" in value or "session_id" in value:
                dialect = "anthropic"
    if dialect is None:
        raise ComparisonError("unsupported_transcript")
    return dialect, records


def reserve_receipt(path):
    directory = None
    descriptor = None
    reserved = False
    try:
        directory = open_directory(path.parent)
        info = os.fstat(directory)
        if info.st_uid != os.getuid() or info.st_mode & 0o022:
            raise ComparisonError("unsafe_receipt_directory")
        descriptor = os.open(path.name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
                             | os.O_NONBLOCK, 0o600, dir_fd=directory)
        os.fchmod(descriptor, 0o600)
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_uid != os.getuid():
            raise ComparisonError("unsafe_receipt_file")
        reserved = True
        return directory, descriptor, path.name, path.parent
    except FileExistsError:
        raise ComparisonError("receipt_exists") from None
    except OSError:
        raise ComparisonError("receipt_unavailable") from None
    finally:
        if not reserved:
            if descriptor is not None:
                os.close(descriptor)
            if directory is not None:
                os.close(directory)


def verify_receipt_directory(directory, path):
    current = open_directory(path)
    try:
        before, after = os.fstat(directory), os.fstat(current)
        if ((before.st_dev, before.st_ino) != (after.st_dev, after.st_ino)
                or after.st_uid != os.getuid() or after.st_mode & 0o022):
            raise ComparisonError("receipt_directory_changed")
    finally:
        os.close(current)


def write_receipt(reservation, receipt):
    directory, descriptor, name, parent = reservation
    verify_receipt_directory(directory, parent)
    raw = (json.dumps(receipt, sort_keys=True, allow_nan=False, indent=2) + "\n").encode("utf-8")
    if len(raw) > MAX_RECEIPT_BYTES:
        raise ComparisonError("receipt_size_limit")
    info = os.fstat(descriptor)
    current = os.stat(name, dir_fd=directory, follow_symlinks=False)
    if (fingerprint(info) != fingerprint(current) or info.st_mode & 0o077
            or info.st_nlink != 1 or info.st_uid != os.getuid()):
        raise ComparisonError("receipt_changed")
    view = memoryview(raw)
    while view:
        written = os.write(descriptor, view)
        if written <= 0:
            raise ComparisonError("receipt_write_failed")
        view = view[written:]
    os.fsync(descriptor)
    current = os.stat(name, dir_fd=directory, follow_symlinks=False)
    info = os.fstat(descriptor)
    if (fingerprint(info) != fingerprint(current) or info.st_mode & 0o077
            or info.st_nlink != 1 or info.st_uid != os.getuid()):
        raise ComparisonError("receipt_changed")
    verify_receipt_directory(directory, parent)
    os.fsync(directory)
    return hashlib.sha256(raw).hexdigest()


def private_directory(parent, name):
    path = parent / name
    path.mkdir(mode=0o700)
    path.chmod(0o700)
    return path


def isolated_environment(root):
    paths = {name: private_directory(root, name) for name in (
        "home", "config", "data", "state", "cache", "codex", "claude", "tmp")}
    return {
        "HOME": str(paths["home"]), "XDG_CONFIG_HOME": str(paths["config"]),
        "XDG_DATA_HOME": str(paths["data"]), "XDG_STATE_HOME": str(paths["state"]),
        "XDG_CACHE_HOME": str(paths["cache"]), "CODEX_HOME": str(paths["codex"]),
        "CLAUDE_CONFIG_DIR": str(paths["claude"]), "TMPDIR": str(paths["tmp"]),
        "TMP": str(paths["tmp"]), "TEMP": str(paths["tmp"]), "PATH": "",
        "LC_ALL": "C", "LANG": "C", "TZ": "UTC", "NO_COLOR": "1",
        "PYTHONNOUSERSITE": "1", "PYTHONSAFEPATH": "1", "PYTHONDONTWRITEBYTECODE": "1",
        "GOBSTOPPER_MAX_TRANSCRIPT_BYTES": str(MAX_SOURCE_BYTES),
    }


def command_arguments(binary, session, threshold, tail):
    arguments = [str(binary), "--no-update", "proxy", "replay", str(session),
                 "--threshold", str(threshold), "--keep-tail-percent", str(tail)]
    for name, value in SHARED_OPTIONS:
        arguments.extend((name, str(value)))
    return arguments + ["--json"]


def run_command(arguments, environment, cwd, timeout, result):
    started = time.monotonic()
    deadline = started + timeout
    result.update({"status": "failed", "error": None, "exit_code": None,
                   "duration_ms": None, "stdout_bytes_observed": 0,
                   "stderr_bytes_observed": 0, "cleanup_complete": None})
    child = None
    streams = ()
    eof = set()
    output = bytearray()
    limited = False
    exited = False
    custody = True

    def drain():
        nonlocal limited
        for index, stream in enumerate(streams):
            if stream in eof:
                continue
            try:
                chunk = os.read(stream.fileno(), 65536)
            except BlockingIOError:
                continue
            if not chunk:
                eof.add(stream)
                continue
            field = "stdout_bytes_observed" if index == 0 else "stderr_bytes_observed"
            previous = result["stdout_bytes_observed"] + result["stderr_bytes_observed"]
            result[field] += len(chunk)
            left = max(0, MAX_CAPTURE_BYTES - previous)
            if index == 0:
                output.extend(chunk[:left])
            limited |= len(chunk) > left

    def observe_exit():
        nonlocal custody
        try:
            return os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None
        except ChildProcessError:
            custody = False
            raise ComparisonError("process_custody_lost") from None

    try:
        if (not all(hasattr(os, name) for name in ("waitid", "P_PID", "WEXITED", "WNOHANG", "WNOWAIT"))
                or signal.getsignal(signal.SIGCHLD) != signal.SIG_DFL):
            raise ComparisonError("process_custody_unavailable")
        if timeout <= 0:
            raise ComparisonError("timeout")
        child = subprocess.Popen(arguments, env=environment, cwd=cwd, shell=False,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, start_new_session=True, close_fds=True)
        streams = (child.stdout, child.stderr)
        with selectors.DefaultSelector() as selector:
            for stream in streams:
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ)
            while True:
                if time.monotonic() >= deadline:
                    raise ComparisonError("timeout")
                drain()
                if limited:
                    raise ComparisonError("output_limit")
                exited = observe_exit()
                if exited:
                    break
                for stream in eof:
                    try:
                        selector.unregister(stream)
                    except KeyError:
                        pass
                selector.select(min(0.01, max(0, deadline - time.monotonic())))
    except ComparisonError as error:
        result["error"] = str(error)
    except OSError:
        result["error"] = "command_unavailable"
    except BaseException:
        result["error"] = "interrupted"
        raise
    finally:
        if child is not None:
            errors = []
            reaped = False
            try:
                if custody:
                    for action in (signal.SIGTERM, signal.SIGKILL):
                        try:
                            exited = observe_exit()
                        except OSError:
                            exited = False
                        try:
                            os.killpg(child.pid, action)
                        except ProcessLookupError:
                            pass
                        except OSError as error:
                            errors.append(error.errno)
                        if action == signal.SIGTERM:
                            grace = time.monotonic() + CLEANUP_SECONDS
                            while time.monotonic() < grace:
                                drain()
                                time.sleep(0.005)
                    child.wait(timeout=0.5)
                    reaped = True
                    result["exit_code"] = child.returncode
                drain_deadline = time.monotonic() + CLEANUP_SECONDS
                while len(eof) < len(streams) and time.monotonic() < drain_deadline:
                    drain()
                    time.sleep(0.001)
            except (OSError, subprocess.TimeoutExpired, ComparisonError):
                if result["error"] is None:
                    result["error"] = "cleanup_failed"
            finally:
                for stream in streams:
                    stream.close()
            benign = (sys.platform == "darwin" and exited and len(eof) == len(streams)
                      and all(code == errno.EPERM for code in errors))
            result["cleanup_complete"] = reaped and len(eof) == len(streams) and (not errors or benign)
            if not result["cleanup_complete"] and result["error"] is None:
                result["error"] = "cleanup_failed"
            elif limited and result["error"] is None:
                result["error"] = "output_limit"
            elif result["exit_code"] != 0 and result["error"] is None:
                result["error"] = "command_failed"
        result["duration_ms"] = round((time.monotonic() - started) * 1000)
        if result["error"] is None:
            result["status"] = "complete"
    return bytes(output)


def replay_report(raw, dialect):
    try:
        value = strict_json(raw)
    except (ValueError, UnicodeError, RecursionError):
        raise ComparisonError("invalid_replay_json") from None
    if (not isinstance(value, dict) or value.get("dialect") != dialect
            or value.get("calibrate") is not True):
        raise ComparisonError("invalid_replay_report")
    for field in METRIC_FIELDS:
        if field not in value or not (number(value[field]) or field in NULLABLE_FIELDS and value[field] is None):
            raise ComparisonError("invalid_replay_report")
    requests = value["requests"]
    compacted = value["compacted"]
    usage = value["usage_requests"]
    if not 0 < requests <= MAX_RECORDS:
        raise ComparisonError("no_replay_requests" if requests == 0 else "invalid_replay_report")
    if (any(value[field] > requests for field in (
            "compacted", "reused_prefix", "over_threshold_after", "raised_threshold",
            "pairing_violations", "source_pairing_violations", "usage_requests"))
            or compacted + value["reused_prefix"] > requests
            or value["pairing_violations"] + value["source_pairing_violations"] > requests
            or value["repeated_reads_covered"] > value["repeated_reads"]
            or value["calibration_samples"] > usage
            or value["back_to_back_compactions"] > max(0, compacted - 1)
            or value["reported_over_threshold"] > usage
            or not 1000 <= value["last_ratio_permille"] <= 2000
            or value["est_cache_read_tokens"] + value["est_cache_write_tokens"] != value["total_est_tokens_out"]):
        raise ComparisonError("invalid_replay_report")
    first = value["first_violation"]
    gap = value["min_compaction_gap"]
    if ((first is None) != (value["pairing_violations"] == 0)
            or first is not None and first >= requests
            or (gap is None) != (compacted < 2)
            or gap is not None and not 1 <= gap < requests):
        raise ComparisonError("invalid_replay_report")
    for direction in ("in", "out"):
        if not (value[f"last_est_tokens_{direction}"] <= value[f"peak_est_tokens_{direction}"]
                <= value[f"total_est_tokens_{direction}"]):
            raise ComparisonError("invalid_replay_report")
    ratios = [value[field] for field in NULLABLE_FIELDS[2:]]
    if (usage == 0 and (any(ratio is not None for ratio in ratios)
                       or value["calibration_samples"] != 0
                       or any(value[field] != 0 for field in PROJECTION_FIELDS))
            or usage > 0 and (any(ratio is None for ratio in ratios) or ratios != sorted(ratios))
            or dialect == "openai-responses" and usage != 0):
        raise ComparisonError("invalid_replay_report")
    compactions = value.get("compactions")
    if not isinstance(compactions, list) or len(compactions) != compacted:
        raise ComparisonError("invalid_replay_report")
    sums = {field: 0 for field in ("head_tokens", "summary_tokens", "tail_tokens", "carry_chars")}
    measured = []
    previous = -1
    for item in compactions:
        if (not isinstance(item, dict) or any(not number(item.get(field)) for field in COMPACTION_FIELDS)
                or "reported_tokens_before" not in item
                or item["reported_tokens_before"] is not None and not number(item["reported_tokens_before"])
                or not previous < item["request"] < requests
                or item["messages_after"] > item["messages_before"]):
            raise ComparisonError("invalid_replay_report")
        previous = item["request"]
        for field in sums:
            sums[field] += item[field]
        if item["reported_tokens_before"] is not None:
            measured.append(item["reported_tokens_before"])
    if len(measured) > usage:
        raise ComparisonError("invalid_replay_report")
    metrics = {field: value[field] for field in METRIC_FIELDS}
    missing = {field: "not_applicable" for field in NULLABLE_FIELDS[:2] if metrics[field] is None}
    usage_reason = ("unsupported_by_builtin_codex_replay" if dialect == "openai-responses" else
                    "no_recorded_provider_usage" if usage == 0 else None)
    if usage == 0:
        for field in PROJECTION_FIELDS + NULLABLE_FIELDS[2:]:
            metrics[field] = None
            missing[field] = usage_reason
    return {
        "dialect": dialect, "calibrate": True, "metrics": metrics, "missing_metrics": missing,
        "recorded_usage_scope": {"status": "unavailable" if usage == 0 else "partial" if usage < requests else "observed",
                                 "requests_observed": usage, "requests_without_consumed_usage": requests - usage,
                                 "reason": usage_reason},
        "compaction_totals": sums,
        "recorded_input_tokens_at_compactions": sum(measured) if measured else None,
        "compactions_with_recorded_usage": len(measured),
        "compactions_without_recorded_usage": compacted - len(measured),
    }


def comparison(first, second):
    if (first["dialect"] != second["dialect"]
            or any(first["metrics"][field] != second["metrics"][field] for field in INVARIANT_FIELDS)):
        raise ComparisonError("replay_arms_not_comparable")
    deltas = {}
    missing = []
    for field in METRIC_FIELDS:
        left, right = first["metrics"][field], second["metrics"][field]
        deltas[field] = right - left if left is not None and right is not None else None
        if deltas[field] is None:
            missing.append(field)
    return {"direction": "tail40_minus_tail0", "metric_deltas": deltas,
            "unavailable_deltas": missing,
            "compaction_total_deltas": {field: second["compaction_totals"][field] - first["compaction_totals"][field]
                                        for field in first["compaction_totals"]}}


def verify_binding(binding, deadline):
    row = binding["row"]
    try:
        observed = read_file(binding["path"], binding["limit"], binding["kind"], deadline)
        row["sha256_after"] = observed["sha256"]
        unchanged = observed == binding["before"]
        row["stable"] = unchanged if row["stable"] is None else row["stable"] and unchanged
        if not unchanged:
            row["verification_error"] = f"{binding['kind']}_changed"
    except ComparisonError as error:
        row["sha256_after"] = None
        row["stable"] = False
        row["verification_error"] = str(error)
    return row["verification_error"]


def verify_all(bindings, deadline):
    errors = [verify_binding(binding, deadline) for binding in bindings]
    return next((error for error in errors if error is not None), None)


def verify_snapshot(path, expected, limit, deadline):
    observed = read_file(path, limit, "snapshot", deadline)
    if observed != expected:
        raise ComparisonError("snapshot_changed")


def experiment(options, receipt):
    deadline = time.monotonic() + 2 * len(options.session) * options.timeout + 60
    bindings = []
    try:
        with tempfile.TemporaryDirectory(prefix="gobstopper-offline-tails-") as temporary:
            root = Path(temporary).resolve(strict=True)
            root.chmod(0o700)
            executable = root / "gobstopper"
            tool = copy_private(options.binary, executable, MAX_BINARY_BYTES, "binary", deadline, executable=True)
            receipt["tool"].update({"sha256_before": tool["sha256"], "executed_sha256": tool["sha256"]})
            bindings.append({"path": options.binary, "limit": MAX_BINARY_BYTES,
                             "kind": "binary", "before": tool, "row": receipt["tool"]})
            executable_before = read_file(executable, MAX_BINARY_BYTES, "snapshot", deadline)
            if executable_before["sha256"] != tool["sha256"]:
                raise ComparisonError("snapshot_changed")
            prepared = []
            for index, path in enumerate(options.session):
                snapshot = root / f"source-{index}.jsonl"
                before = copy_private(path, snapshot, MAX_SOURCE_BYTES, "source", deadline, capture=True)
                raw = before.pop("raw")
                row = receipt["sources"][index]
                row.update({"sha256_before": before["sha256"], "bytes": before["bytes"]})
                binding = {"path": path, "limit": MAX_SOURCE_BYTES, "kind": "source",
                           "before": before, "row": row}
                bindings.append(binding)
                dialect, records = transcript_shape(raw)
                del raw
                row.update({"dialect": dialect, "records": records})
                frozen = read_file(snapshot, MAX_SOURCE_BYTES, "snapshot", deadline)
                if frozen["sha256"] != before["sha256"]:
                    raise ComparisonError("snapshot_changed")
                prepared.append((snapshot, frozen, row))
            time.sleep(STABILITY_SECONDS)
            error = verify_all(bindings, deadline)
            if error:
                raise ComparisonError(error)
            for index, (snapshot, frozen, row) in enumerate(prepared):
                for tail in TAILS:
                    arm = row["arms"][str(tail)]
                    arm["status"] = "failed"
                    error = verify_all(bindings, deadline)
                    if error:
                        arm["error"] = error
                        raise ComparisonError(error)
                    verify_snapshot(snapshot, frozen, MAX_SOURCE_BYTES, deadline)
                    verify_snapshot(executable, executable_before, MAX_BINARY_BYTES, deadline)
                    work = private_directory(root, f"session-{index}-tail-{tail}")
                    environment = isolated_environment(work)
                    cwd = private_directory(work, "workspace")
                    name = "rollout-replay.jsonl" if row["dialect"] == "openai-responses" else "claude-replay.jsonl"
                    arm_source = cwd / name
                    copied = copy_private(snapshot, arm_source, MAX_SOURCE_BYTES, "snapshot", deadline)
                    if copied["sha256"] != frozen["sha256"]:
                        raise ComparisonError("snapshot_changed")
                    arm_before = read_file(arm_source, MAX_SOURCE_BYTES, "snapshot", deadline)
                    arm["execution"] = {}
                    try:
                        raw_report = run_command(command_arguments(executable, arm_source, options.threshold, tail),
                                                 environment, cwd, min(options.timeout, max(0, deadline - time.monotonic())),
                                                 arm["execution"])
                        if arm["execution"]["error"]:
                            raise ComparisonError(arm["execution"]["error"])
                        arm["report"] = replay_report(raw_report, row["dialect"])
                        arm["status"] = "complete"
                    except ComparisonError as failure:
                        arm["error"] = str(failure)
                    except BaseException:
                        arm["error"] = "interrupted"
                        raise
                    finally:
                        try:
                            verify_snapshot(arm_source, arm_before, MAX_SOURCE_BYTES, deadline)
                            verify_snapshot(executable, executable_before, MAX_BINARY_BYTES, deadline)
                        except ComparisonError as failure:
                            arm["error"] = str(failure)
                        changed = verify_all(bindings, deadline)
                        if changed:
                            arm["error"] = changed
                        if arm["error"]:
                            arm["status"] = "invalidated" if arm["report"] is not None else "failed"
                    if arm["error"]:
                        raise ComparisonError(arm["error"])
                row["comparison"] = comparison(row["arms"]["0"]["report"], row["arms"]["40"]["report"])
            receipt["status"] = "complete"
    finally:
        changed = verify_all(bindings, deadline)
        if changed:
            receipt["status"] = "failed"
            receipt["error"] = changed
        if receipt["status"] != "complete":
            for row in receipt["sources"]:
                row["comparison"] = None


def new_receipt(options):
    return {
        "schema": SCHEMA, "status": "failed", "error": None,
        "created_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "configuration": {
            "threshold": options.threshold, "timeout_seconds_per_arm": options.timeout,
            "keep_tail_percent": list(TAILS), "calibrate": True,
            "shared_options": dict(SHARED_OPTIONS),
            "argument_templates": {str(tail): command_arguments("<private-executable-copy>", "<private-session-copy>",
                                                                 options.threshold, tail) for tail in TAILS},
            "parser": "gobstopper_builtin_claude_code_or_codex",
            "unexposed_engine_defaults": "bound_to_executable_sha256",
            "environment": "allowlist_only_empty_config_and_provider_homes_empty_path",
            "limits": {"sessions": MAX_SESSIONS, "source_bytes": MAX_SOURCE_BYTES,
                       "binary_bytes": MAX_BINARY_BYTES, "records": MAX_RECORDS,
                       "record_bytes": MAX_RECORD_BYTES, "captured_output_bytes_per_arm": MAX_CAPTURE_BYTES,
                       "receipt_bytes": MAX_RECEIPT_BYTES, "read_seconds": READ_SECONDS,
                       "total_seconds_budget": 2 * len(options.session) * options.timeout + 60},
        },
        "tool": {"identity_sha256": identity(options.binary), "sha256_before": None,
                 "sha256_after": None, "executed_sha256": None, "stable": None,
                 "verification_error": None, "execution": "private_byte_identical_copy"},
        "sources": [{"source_identity_sha256": identity(path), "sha256_before": None,
                     "sha256_after": None, "stable": None, "verification_error": None,
                     "bytes": None, "records": None, "dialect": None, "comparison": None,
                     "arms": {str(tail): {"keep_tail_percent": tail, "status": "not_run",
                                           "error": None, "execution": None, "report": None} for tail in TAILS}}
                    for path in options.session],
        "metric_basis": {
            "counts": {"fields": list(COUNT_FIELDS), "basis": "offline_replay_counts_from_recorded_history"},
            "token_estimates": {"fields": list(ESTIMATE_FIELDS),
                                "basis": "engine_size_and_cache_estimates_not_measured_tokens_or_billing"},
            "reported_usage_projections": {"fields": list(PROJECTION_FIELDS),
                                           "basis": "outgoing_estimate_scaled_by_recorded_input_ratio_not_measured_outgoing_usage"},
            "ratio_permille": "recorded_uncompacted_input_usage_divided_by_estimate_or_applied_calibration",
            "compaction_totals": "head_summary_tail_estimated_tokens_and_carried_character_counts",
            "recorded_input_tokens_at_compactions": "partial_recorded_input_only_at_compacted_requests_not_a_session_bill",
            "repeated_reads": "matching_tool_input_in_recorded_responses_coverage_is_verbatim_result_presence_not_quality_or_causality",
            "execution_duration_ms": "local_replay_and_owned_process_cleanup_not_provider_latency",
            "observed_output_bytes": "bounded_bytes_read_not_total_emitted_after_an_output_limit",
        },
        "unavailable": {name: {"status": "unavailable", "value": None,
                               "reason": "offline_replay_without_provider_calls_or_new_model_continuations"}
                        for name in ("quality", "billing", "cost", "continuation_effects")},
        "limitations": {
            "reconstruction": "built_in_retained_main_chain_after_recorded_compaction_not_complete_session_lifetime",
            "fixed_context": {"assumed_tokens": 20_000, "measured_tokens": None,
                              "reason": "system_prompt_and_tool_definitions_not_recorded"},
            "recorded_usage": "usage_projections_assume_recording_was_not_behind_a_proxy_not_verified",
            "usage_coverage": "builtin_replay_consumes_claude_code_usage_only_codex_recorded_usage_is_not_read",
            "responses": "recorded_responses_reused_without_regeneration_no_causal_continuation_effect",
            "sample": "explicit_bounded_inputs_not_random_or_representative",
        },
        "safety": {
            "source_files_written_by_runner": False, "inputs": "explicit_only_no_discovery",
            "processes": "at_most_two_sequential_offline_replay_arms_per_explicit_source",
            "network_sandbox": False,
            "executable_trust": "caller_must_supply_a_reviewed_gobstopper_binary_hash_is_identity_not_attestation",
            "source_stability_scope": "identity_metadata_and_sha256_checks_during_this_run_not_a_provider_lock_or_proof_session_is_closed",
        },
    }


def arguments(argv):
    parser = Parser(description="Compare offline proxy replay size estimates for tail 0 and tail 40. No quality or cost measurement.",
                    allow_abbrev=False)
    parser.add_argument("--binary", required=True, help="Absolute path to a reviewed Gobstopper executable.")
    parser.add_argument("--session", required=True, action="append", help="Explicit absolute closed transcript path; repeat up to 8 times.")
    parser.add_argument("--threshold", required=True, type=int, help="Threshold in tokens, 1 through 1000000.")
    parser.add_argument("--timeout", required=True, type=float, help="Wall-clock seconds per replay arm, 0.05 through 120.")
    parser.add_argument("--output", required=True, help="New private JSON results file in a directory you own that other users cannot write.")
    options = parser.parse_args(argv)
    if (not 1 <= len(options.session) <= MAX_SESSIONS or not 1 <= options.threshold <= MAX_THRESHOLD
            or not math.isfinite(options.timeout) or not 0.05 <= options.timeout <= MAX_TIMEOUT):
        raise ComparisonError("invalid_arguments")
    options.binary = absolute_path(options.binary)
    options.session = [absolute_path(path) for path in options.session]
    options.output = absolute_path(options.output)
    if len(set(options.session)) != len(options.session):
        raise ComparisonError("duplicate_session")
    if options.output == options.binary or options.output in options.session:
        raise ComparisonError("receipt_aliases_input")
    return options


def main(argv=None):
    receipt = None
    reservation = None
    receipt_sha256 = None
    error = None
    code = 2
    handlers = {}
    try:
        options = arguments(argv)
        if os.name != "posix" or not all(hasattr(os, name) for name in ("O_NOFOLLOW", "O_NONBLOCK", "O_DIRECTORY")):
            raise ComparisonError("unsupported_platform")
        reservation = reserve_receipt(options.output)
        receipt = new_receipt(options)
        for name in (signal.SIGINT, signal.SIGTERM):
            handlers[name] = signal.signal(name, interrupt)
        code = 1
        experiment(options, receipt)
        code = 0 if receipt["status"] == "complete" else 1
    except ComparisonError as failure:
        error = str(failure)
    except (ComparisonInterrupted, KeyboardInterrupt):
        error = "interrupted"
        code = 130
    except Exception:
        error = "internal_error"
        code = 1
    finally:
        try:
            if receipt is not None:
                if error:
                    receipt["error"] = receipt["error"] or error
                    receipt["status"] = "failed"
                    for row in receipt["sources"]:
                        row["comparison"] = None
                error = receipt["error"]
                receipt_sha256 = write_receipt(reservation, receipt)
        except Exception:
            error = "receipt_write_failed"
            code = 1
            if receipt is not None:
                receipt["status"] = "failed"
        finally:
            if reservation is not None:
                os.close(reservation[1])
                os.close(reservation[0])
            for name, handler in handlers.items():
                signal.signal(name, handler)
    arms = [arm for row in receipt["sources"] for arm in row["arms"].values()] if receipt else []
    summary = {"schema": SCHEMA, "ok": code == 0, "status": receipt["status"] if receipt else "rejected",
               "error": error, "sessions": len(receipt["sources"]) if receipt else 0,
               "arms_with_reports": sum(arm["report"] is not None for arm in arms),
               "receipt_written": receipt_sha256 is not None, "receipt_sha256": receipt_sha256}
    print(json.dumps(summary, sort_keys=True, allow_nan=False))
    return code


if __name__ == "__main__":
    sys.exit(main())

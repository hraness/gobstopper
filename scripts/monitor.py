#!/usr/bin/env python3
"""One local, read-only Gobstopper observation. No transcript text is retained."""

import argparse
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import tempfile
import time
import uuid

SCHEMA = "gobstopper/local-monitor-v1"
LOG_BYTES = 10 * 1024 * 1024
CAPTURE_BYTES = 8 * 1024 * 1024
TIMEOUT_SECONDS = 45
SESSION_ID = re.compile(r"[A-Za-z0-9_-]{1,128}\Z")
PLAN_LINE = re.compile(rb"^\[dry-run\] codex ([A-Za-z0-9_-]{1,12}): ")
WATCH_FAILURE = re.compile(rb"^(?:load|plan) [A-Za-z0-9_.-]{1,256} failed:")


class MonitorError(Exception):
    """Only closed, content-free error codes cross the logging boundary."""


class MonitorInterrupted(BaseException):
    """Cancellation unwinds cleanup instead of leaving a child process group."""


def interrupt(_signal, _frame):
    raise MonitorInterrupted()


def number(value):
    return value if type(value) is int and 0 <= value <= 2**64 - 1 else None


def private_file(fd):
    info = os.fstat(fd)
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
            or info.st_mode & 0o077 or info.st_nlink != 1):
        raise MonitorError("unsafe_output_file")


def open_file(directory, name, flags):
    fd = os.open(name, flags | os.O_NOFOLLOW, 0o600, dir_fd=directory)
    try:
        private_file(fd)
    except Exception:
        os.close(fd)
        raise
    return fd


def check_file(directory, name):
    try:
        fd = open_file(directory, name, os.O_RDONLY)
    except FileNotFoundError:
        return
    os.close(fd)


def open_directory(path):
    if not path.is_absolute():
        raise MonitorError("absolute_paths_required")
    # Refuse redirected output paths, including symlinks in ancestors.
    for component in reversed((path, *path.parents)):
        try:
            if stat.S_ISLNK(component.lstat().st_mode):
                raise MonitorError("unsafe_output_directory")
        except FileNotFoundError:
            component.mkdir(mode=0o700)
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    info = os.fstat(fd)
    if info.st_uid != os.getuid() or info.st_mode & 0o077:
        os.close(fd)
        raise MonitorError("unsafe_output_directory")
    return fd


def binary_hash(path):
    if not path.is_absolute():
        raise MonitorError("absolute_paths_required")
    resolved = path.resolve(strict=True)
    info = resolved.stat()
    if not stat.S_ISREG(info.st_mode) or not os.access(resolved, os.X_OK):
        raise MonitorError("invalid_executable")
    if info.st_size > 128 * 1024 * 1024:
        raise MonitorError("invalid_executable")
    digest = hashlib.sha256()
    with resolved.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return resolved, digest.hexdigest()


def run_command(arguments, environment, deadline):
    started = time.monotonic()
    result = {"exit_code": None, "duration_ms": 0, "error": None}
    output = [bytearray(), bytearray()]
    child = None
    try:
        if started >= deadline:
            raise MonitorError("timeout")
        child = subprocess.Popen(arguments, env=environment, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 start_new_session=True)
        with selectors.DefaultSelector() as selector:
            for index, stream in enumerate((child.stdout, child.stderr)):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, index)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise MonitorError("timeout")
                for key, _ in selector.select(min(remaining, 0.2)):
                    chunk = os.read(key.fd, 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    if sum(map(len, output)) + len(chunk) > CAPTURE_BYTES:
                        raise MonitorError("output_limit")
                    output[key.data].extend(chunk)
        result["exit_code"] = child.wait(timeout=max(0.001, deadline - time.monotonic()))
        if result["exit_code"] != 0:
            result["error"] = "command_failed"
    except (MonitorError, subprocess.TimeoutExpired) as error:
        result["error"] = str(error) if isinstance(error, MonitorError) else "timeout"
    except OSError:
        result["error"] = "command_unavailable"
    except BaseException:
        result["error"] = "interrupted"
        raise
    finally:
        if child is not None:
            if result["error"] is not None:
                # The child owns this new process group; never signal another session.
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            child.wait()
            child.stdout.close()
            child.stderr.close()
        result["duration_ms"] = round((time.monotonic() - started) * 1000)
    return result, bytes(output[0]), bytes(output[1])


def previous_observation(directory):
    try:
        fd = open_file(directory, "latest.json", os.O_RDONLY)
    except FileNotFoundError:
        return {}
    with os.fdopen(fd, "rb") as source:
        raw = source.read(1024 * 1024 + 1)
    try:
        value = json.loads(raw) if len(raw) <= 1024 * 1024 else None
    except (ValueError, UnicodeError):
        value = None
    if not isinstance(value, dict) or value.get("schema") != SCHEMA:
        raise MonitorError("invalid_previous_observation")
    return value


def session_rows(report, sessions, previous):
    old = {row.get("session_id"): row for row in previous.get("sessions", [])
           if isinstance(row, dict)}
    rows = []
    for session in sessions:
        matches = [row.get("gobstopper") for row in report.get("sessions", [])
                   if isinstance(row, dict) and row.get("provider") == "codex"
                   and isinstance(row.get("gobstopper"), dict)
                   and row["gobstopper"].get("sessionIdNative") == session]
        current = matches[0] if len(matches) == 1 else {}
        compact = current.get("compactions", {})
        compact = compact if isinstance(compact, dict) else {}
        before = old.get(session, {})
        # A zero is also emitted while provider usage is unknown immediately
        # after compaction. It cannot establish a measured drop to zero.
        context = number(current.get("contextTokens")) or None
        native = number(compact.get("nativeHookApplied"))
        old_context = number(before.get("context_tokens"))
        old_native = number(before.get("native_hook_applied"))
        comparable = bool(before.get("available")) and bool(current)
        rows.append({
            "session_id": session,
            "available": bool(current),
            "provider": "codex",
            "context_tokens": context,
            "model_context_window": number(current.get("modelContextWindow")) or None,
            "lifetime_input_tokens": number(current.get("lifetimeInputTokens")),
            "lifetime_cached_tokens": number(current.get("lifetimeCachedTokens")),
            "last_activity_ms": number(current.get("lastActivityMs")),
            "context_drop_tokens": (max(0, old_context - context) if comparable
                                    and old_context is not None and context is not None else None),
            "native_hook_applied": native,
            "native_hook_applied_delta": (native - old_native if comparable
                                          and native is not None and old_native is not None
                                          and native >= old_native else None),
            "native_counter_reset": (native < old_native if comparable
                                     and native is not None and old_native is not None else None),
        })
    return rows


def save_observation(directory, observation):
    line = (json.dumps(observation, separators=(",", ":"), sort_keys=True) + "\n").encode()
    if len(line) > 1024 * 1024:
        raise MonitorError("observation_limit")
    for name in ("latest.json", "observations.jsonl", "observations.1.jsonl"):
        check_file(directory, name)
    try:
        if os.stat("observations.1.jsonl", dir_fd=directory, follow_symlinks=False).st_size > LOG_BYTES:
            raise MonitorError("existing_log_limit")
    except FileNotFoundError:
        pass
    try:
        size = os.stat("observations.jsonl", dir_fd=directory, follow_symlinks=False).st_size
    except FileNotFoundError:
        size = 0
    if size > LOG_BYTES:
        raise MonitorError("existing_log_limit")
    if size + len(line) > LOG_BYTES:
        os.replace("observations.jsonl", "observations.1.jsonl",
                   src_dir_fd=directory, dst_dir_fd=directory)
    fd = open_file(directory, "observations.jsonl", os.O_WRONLY | os.O_APPEND | os.O_CREAT)
    with os.fdopen(fd, "ab") as log:
        log.write(line)
        log.flush()
        os.fsync(log.fileno())
    name = ".latest-" + uuid.uuid4().hex
    try:
        fd = open_file(directory, name, os.O_WRONLY | os.O_CREAT | os.O_EXCL)
        with os.fdopen(fd, "wb") as latest:
            latest.write(line)
            latest.flush()
            os.fsync(latest.fileno())
        os.replace(name, "latest.json", src_dir_fd=directory, dst_dir_fd=directory)
        os.fsync(directory)
    finally:
        try:
            os.unlink(name, dir_fd=directory)
        except FileNotFoundError:
            pass


def observe(binary, output_dir, sessions):
    if not sessions or len(sessions) > 128 or any(not SESSION_ID.fullmatch(s) for s in sessions):
        raise MonitorError("invalid_sessions")
    sessions = list(dict.fromkeys(sessions))
    executable, digest = binary_hash(binary)
    directory = open_directory(output_dir)
    lock = None
    try:
        lock = open_file(directory, ".monitor.lock", os.O_RDWR | os.O_CREAT)
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise MonitorError("already_running") from None
        previous = previous_observation(directory)
        deadline = time.monotonic() + TIMEOUT_SECONDS
        with tempfile.TemporaryDirectory(prefix=".monitor-config-", dir=output_dir) as config:
            environment = {key: value for key, value in os.environ.items()
                           if not key.startswith("GOBSTOPPER_")}
            environment["GOBSTOPPER_SCORER"] = "heuristic"
            # Even dry-run can execute configured extensions. Empty config
            # guarantees the deterministic built-in policy and no plugins.
            environment["XDG_CONFIG_HOME"] = config
            report_status, stdout, _ = run_command([str(executable), "report", "--active-only"], environment, deadline)
            report = {}
            if report_status["error"] is None:
                try:
                    report = json.loads(stdout)
                    if (not isinstance(report, dict) or report.get("schemaVersion") != 1
                            or report.get("profile") != "session-observations-v1"
                            or not isinstance(report.get("sessions"), list)):
                        raise ValueError()
                except (ValueError, UnicodeError):
                    report = {}
                    report_status["error"] = "invalid_report"
            watch_status, _, stderr = run_command(
                [str(executable), "watch", "--dry-run", "--active-only", "--once"],
                environment, deadline)
        # watch reports per-session failures on stderr but can still exit 0.
        # Classify only its known diagnostic shapes; never retain their text.
        if watch_status["error"] is None and any(WATCH_FAILURE.match(line) for line in stderr.splitlines()):
            watch_status["error"] = "watch_evaluation_failed"
        plans = set()
        ambiguous_plan = False
        known_ids = {row["gobstopper"]["sessionIdNative"] for row in report.get("sessions", [])
                     if isinstance(row, dict) and row.get("provider") == "codex"
                     and isinstance(row.get("gobstopper"), dict)
                     and isinstance(row["gobstopper"].get("sessionIdNative"), str)}
        if watch_status["error"] is None:
            for line in stderr.splitlines():
                match = PLAN_LINE.match(line)
                if match:
                    prefix = match[1].decode("ascii")
                    selected = [session for session in sessions if session[:12] == prefix]
                    known = [session for session in known_ids if session[:12] == prefix]
                    if len(selected) == 1 and known == selected:
                        plans.add(selected[0])
                    elif selected:
                        ambiguous_plan = True
        if binary_hash(executable)[1] != digest:
            raise MonitorError("binary_changed")
        report_status["available"] = report_status["error"] is None
        watch_status.update({"available": watch_status["error"] is None,
                             "plan_count": (len(plans) if watch_status["error"] is None
                                            and report_status["available"] and not ambiguous_plan else None),
                             "scope": "allowlisted_sessions", "policy": "built_in_defaults"})
        observation = {
            "schema": SCHEMA,
            "observed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "binary_sha256": digest,
            "attribution": "unknown_context_drops_are_not_gobstopper_savings",
            "report": report_status,
            "watch": watch_status,
            "sessions": session_rows(report, sessions, previous),
        }
        save_observation(directory, observation)
        return observation
    finally:
        if lock is not None:
            os.close(lock)
        os.close(directory)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--session", required=True, action="append")
    args = parser.parse_args()
    handlers = {kind: signal.signal(kind, interrupt) for kind in (signal.SIGTERM, signal.SIGINT)}
    try:
        observation = observe(args.binary, args.output_dir, args.session)
        error = observation["report"]["error"] or observation["watch"]["error"]
    except MonitorInterrupted:
        error = "interrupted"
    except MonitorError as failure:
        error = str(failure)
    except (OSError, ValueError, TypeError):
        error = "local_io_or_format_error"
    finally:
        for kind, handler in handlers.items():
            signal.signal(kind, handler)
    print(json.dumps({"status": "error" if error else "ok", "error": error}))
    return int(error is not None)


if __name__ == "__main__":
    raise SystemExit(main())

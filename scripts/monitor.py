#!/usr/bin/env python3
"""One local, read-only Gobstopper observation. No transcript text is retained."""

import argparse
import datetime
import errno
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid

try:
    import resource
except ImportError:
    resource = None

SCHEMA = "gobstopper/local-monitor-v1"
LOG_BYTES = 10 * 1024 * 1024
CAPTURE_BYTES = 8 * 1024 * 1024
EVENTS_LOG_BYTES = 16 * 1024 * 1024
EVENT_RECORD_BYTES = 64 * 1024
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


def child_resources():
    """Best-effort aggregate counters; this monitor runs one child at a time."""
    try:
        return resource.getrusage(resource.RUSAGE_CHILDREN)
    except Exception:
        return None


def resource_delta(before, after):
    """Never let optional diagnostics replace a command result or cleanup."""
    if before is None or after is None:
        return None
    try:
        result = {}
        for name, field, scale in (
            ("user_cpu_us", "ru_utime", 1_000_000),
            ("system_cpu_us", "ru_stime", 1_000_000),
            ("minor_page_faults", "ru_minflt", 1),
            ("major_page_faults", "ru_majflt", 1),
            ("voluntary_context_switches", "ru_nvcsw", 1),
            ("involuntary_context_switches", "ru_nivcsw", 1),
        ):
            start, end = getattr(before, field), getattr(after, field)
            value = None
            if scale == 1:
                if number(start) is not None and number(end) is not None:
                    value = number(end - start)
            elif (type(start) in (int, float) and type(end) in (int, float)
                  and math.isfinite(start) and math.isfinite(end) and 0 <= start <= end):
                delta = (end - start) * scale
                if math.isfinite(delta) and 0 <= delta <= 2**64 - 1:
                    value = number(round(delta))
            result[name] = value
        return result
    except Exception:
        return None


def private_file(fd):
    info = os.fstat(fd)
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
            or info.st_mode & 0o077 or info.st_nlink != 1):
        raise MonitorError("unsafe_output_file")


def open_file(directory, name, flags):
    # Validate the opened leaf before use. A FIFO must not block this open
    # before private_file can reject its kind; ordinary files are unaffected.
    fd = os.open(name, flags | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600, dir_fd=directory)
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
    result = {"exit_code": None, "duration_ms": 0, "error": None, "resources": None,
              "cleanup_complete": None}
    output = [bytearray(), bytearray()]
    child = None
    resources_before = None
    streams = ()
    eof = set()
    exited = False
    limited = False

    def drain_once():
        nonlocal limited
        progress = False
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
            progress = True
            left = max(0, CAPTURE_BYTES - sum(map(len, output)))
            output[index].extend(chunk[:left])
            limited |= len(chunk) > left
        return progress

    try:
        if not all(hasattr(os, name) for name in ("waitid", "WNOWAIT", "WNOHANG", "P_PID", "WEXITED")):
            raise MonitorError("process_custody_unavailable")
        if started >= deadline:
            raise MonitorError("timeout")
        resources_before = child_resources()
        if time.monotonic() >= deadline:
            raise MonitorError("timeout")
        child = subprocess.Popen(arguments, env=environment, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 start_new_session=True)
        streams = (child.stdout, child.stderr)
        with selectors.DefaultSelector() as selector:
            for index, stream in enumerate(streams):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, index)
            while True:
                drain_once()
                if limited:
                    raise MonitorError("output_limit")
                # Do not reap yet: the leader PID protects group identity until
                # cleanup. Successful leaders may still have live descendants.
                exited = os.waitid(os.P_PID, child.pid,
                                   os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None
                if exited:
                    break
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise MonitorError("timeout")
                for stream in eof:
                    try:
                        selector.unregister(stream)
                    except KeyError:
                        pass
                selector.select(min(remaining, 0.01))
    except (MonitorError, subprocess.TimeoutExpired) as error:
        result["error"] = str(error) if isinstance(error, MonitorError) else "timeout"
    except OSError:
        result["error"] = "command_unavailable"
    except BaseException:
        result["error"] = "interrupted"
        raise
    finally:
        if child is not None:
            cleanup_error = None
            reaped = False
            try:
                # The leader has not been reaped. Only this owned group may be
                # signaled, including on normal successful completion.
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except OSError as failure:
                cleanup_error = failure
            try:
                child.wait(timeout=1)
                reaped = True
                if exited:
                    result["exit_code"] = child.returncode
                # Drain all accepted trailing bytes, with a cleanup deadline
                # even when an escaped descendant keeps an inherited pipe open.
                drain_deadline = time.monotonic() + 0.25
                while len(eof) < len(streams) and time.monotonic() < drain_deadline:
                    if not drain_once():
                        time.sleep(0.001)
            except (OSError, subprocess.TimeoutExpired):
                if result["error"] is None:
                    result["error"] = "cleanup_failed"
            finally:
                for stream in streams:
                    stream.close()
            # Darwin may refuse signaling an exited zombie-only group. Only
            # observed exit plus both EOFs admits that platform exception.
            benign = (cleanup_error is not None and cleanup_error.errno == errno.EPERM
                      and sys.platform == "darwin" and exited and len(eof) == len(streams))
            complete = reaped and len(eof) == len(streams) and (cleanup_error is None or benign)
            result["cleanup_complete"] = complete
            if not complete and result["error"] is None:
                result["error"] = "cleanup_failed"
            if limited and result["error"] is None:
                result["error"] = "output_limit"
            if result["exit_code"] not in (None, 0) and result["error"] is None:
                result["error"] = "command_failed"
            if reaped:
                # Sample only after reaping, including timeout-killed children.
                result["resources"] = resource_delta(resources_before, child_resources())
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
        value = strict_json(raw) if len(raw) <= 1024 * 1024 else None
    except (ValueError, UnicodeError, RecursionError):
        value = None
    if not isinstance(value, dict) or value.get("schema") != SCHEMA:
        raise MonitorError("invalid_previous_observation")
    return value


def digest(value):
    return value if isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) else None


def strict_json(raw):
    def object_pairs(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate_json_key")
            result[key] = value
        return result

    def invalid_constant(_value):
        raise ValueError("invalid_json_constant")

    return json.loads(raw, object_pairs_hook=object_pairs, parse_constant=invalid_constant)


def event_log_path(environment):
    base = environment.get("XDG_DATA_HOME")
    if not base:
        base = str(Path(environment.get("HOME", ".")) / ".local" / "share")
    return Path(base) / "gobstopper" / "events.jsonl"


def accounting_fields(value):
    state = value.get("contextState")
    state = state if state in ("absent", "unknown", "reported", "reset") else "absent"
    scope = value.get("lifetimeScope")
    scope = scope if scope in ("absent", "partial", "full") else "absent"
    return {
        "source_identity_sha256": digest(value.get("sourceIdentitySha256")),
        "context_state": state,
        "context_tokens": number(value.get("reportedContextTokens")) if state == "reported" else None,
        "lifetime_scope": scope,
        "lifetime_input_tokens": number(value.get("lifetimeInputTokens")) if scope == "full" else None,
        "lifetime_cached_tokens": number(value.get("lifetimeCachedTokens")) if scope == "full" else None,
    }


def paired_evidence(event):
    identity = digest(event.get("source_identity_sha256"))
    provider = event.get("provider")
    session = event.get("session_id")
    if (identity is None or provider not in ("codex", "claude_code", "devin")
            or not isinstance(session, str) or not SESSION_ID.fullmatch(session)
            or event.get("schema") != "gobstopper/compaction-events-v1"
            or event.get("outcome") != "applied"
            or event.get("action") not in ("provider_compact", "transcript_compact")
            or "error_code" not in event or event["error_code"] is not None):
        return None
    pair = []
    for side in ("before", "after"):
        snapshot = digest(event.get(f"snapshot_{side}_sha256"))
        observation = event.get(f"{side}_observation")
        if (snapshot is None or not isinstance(observation, dict)
                or observation.get("source_identity_sha256") != identity
                or observation.get("snapshot_manifest_sha256") != snapshot
                or digest(observation.get("source_sha256")) is None):
            return None
        pair.append(snapshot)
    return (provider, session, identity), tuple(pair)


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
        accounting = accounting_fields(current)
        context = accounting["context_tokens"]
        native = number(compact.get("nativeHookApplied"))
        old_context = number(before.get("context_tokens"))
        old_native = number(before.get("native_hook_applied"))
        comparable = (bool(before.get("available")) and bool(current)
                      and accounting["source_identity_sha256"] is not None
                      and before.get("source_identity_sha256") == accounting["source_identity_sha256"])
        rows.append({
            "session_id": session,
            "available": bool(current),
            "provider": "codex",
            **accounting,
            "model_context_window": number(current.get("modelContextWindow")) or None,
            "last_activity_ms": number(current.get("lastActivityMs")),
            "closed_session_compact": (current.get("closedSessionCompact")
                                       if current.get("closedSessionCompact") in
                                       ("available", "unqualified", "unavailable:sub-agent") else None),
            "context_drop_tokens": (max(0, old_context - context) if comparable
                                    and before.get("context_state") == "reported"
                                    and old_context is not None and context is not None
                                    and old_context > 0 and context > 0 else None),
            "native_hook_applied": native,
            "native_hook_applied_delta": (native - old_native if comparable
                                          and native is not None and old_native is not None
                                          and native >= old_native else None),
            "native_counter_reset": (native < old_native if comparable
                                     and native is not None and old_native is not None else None),
        })
    return rows


def context_samples(report, sessions, providers):
    """Context rows for allowlisted sessions plus every active session of
    an opted-in provider — the trajectory substrate for cohort analysis.
    Identifiers and counts only; the allowlist stays the privacy boundary."""
    wanted = set(sessions)
    providers = set(providers)
    samples = []
    for row in report.get("sessions", []):
        if not isinstance(row, dict):
            continue
        gobstopper = row.get("gobstopper")
        if not isinstance(gobstopper, dict):
            continue
        session = gobstopper.get("sessionIdNative")
        provider = row.get("provider")
        if (not isinstance(session, str) or not SESSION_ID.fullmatch(session)
                or provider not in ("codex", "claude_code", "devin")
                or (session not in wanted and provider not in providers)):
            continue
        samples.append({
            "provider": provider,
            "session_id": session,
            **accounting_fields(gobstopper),
        })
        if len(samples) >= 256:
            break
    return samples


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


def retention_summary(log_path, selected):
    """Count distinct error-free evidence pairs for exact selected sources.

    Counts and flagged native IDs only. Duplicate records do not multiply a
    measurement; conflicting tallies for one exact pair exclude that pair.
    This reads one bounded local generation, not a lifetime retention total.
    """
    out = {"measured": 0, "checks": 0, "literal": 0, "lexical": 0,
           "lossy_sessions": [], "available": False, "error": None,
           "invalid_records": 0, "conflicting_pairs": 0}
    try:
        fd = os.open(log_path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, "rb") as log:
            info = os.fstat(log.fileno())
            if not stat.S_ISREG(info.st_mode):
                out["error"] = "event_log_not_regular"
                return out
            if info.st_size > EVENTS_LOG_BYTES:
                out["error"] = "event_log_limit"
                return out
            data = log.read(EVENTS_LOG_BYTES + 1)
        if len(data) > EVENTS_LOG_BYTES:
            out["error"] = "event_log_limit"
            return out
        if data and not data.endswith(b"\n"):
            out["error"] = "event_log_incomplete"
            return out
        lines = data.decode("utf-8").splitlines()
    except FileNotFoundError:
        out["error"] = "event_log_absent"
        return out
    except (OSError, UnicodeError):
        out["error"] = "event_log_unavailable"
        return out
    out["available"] = True
    pairs = {}
    for line in lines:
        try:
            if len(line.encode("utf-8")) > EVENT_RECORD_BYTES:
                raise ValueError("event_record_limit")
            event = strict_json(line)
        except (ValueError, RecursionError):
            out["invalid_records"] += 1
            continue
        if not isinstance(event, dict):
            out["invalid_records"] += 1
            continue
        evidence = paired_evidence(event)
        if evidence is None or evidence[0] not in selected:
            continue
        total, literal = event.get("retention_total"), event.get("retention_retained")
        lexical = event.get("retention_lexical")
        values = (total, literal, lexical)
        if not all(type(v) is int and 0 <= v <= 100_000 for v in values):
            continue
        if literal > total or lexical > total:
            continue
        # A manifest identifies one exact byte sequence. Conflicting source
        # hashes for the same manifests invalidate the pair, not a new sample.
        values += (event["before_observation"]["source_sha256"],
                   event["after_observation"]["source_sha256"])
        if evidence in pairs and pairs[evidence] != values:
            pairs[evidence] = None
        else:
            pairs[evidence] = values
    seen_lossy = set()
    for (identity, _pair), values in pairs.items():
        if values is None:
            out["conflicting_pairs"] += 1
            continue
        total, literal, lexical = values[:3]
        out["measured"] += 1
        out["checks"] += total
        out["literal"] += literal
        out["lexical"] += lexical
        session = identity[1]
        if total and lexical * 2 < total and session not in seen_lossy:
            seen_lossy.add(session)
            out["lossy_sessions"].append(session)
    return out


def observe(binary, output_dir, sessions, providers=()):
    if not sessions or len(sessions) > 128 or any(not SESSION_ID.fullmatch(s) for s in sessions):
        raise MonitorError("invalid_sessions")
    if len(providers) > 8 or any(p not in ("codex", "claude_code", "devin") for p in providers):
        raise MonitorError("invalid_providers")
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
                    report = strict_json(stdout)
                    if (not isinstance(report, dict) or report.get("schemaVersion") != 1
                            or report.get("profile") != "session-observations-v1"
                            or not isinstance(report.get("sessions"), list)):
                        raise ValueError()
                except (ValueError, UnicodeError, RecursionError):
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
        samples = context_samples(report, sessions, providers)
        # Retention uses the selected report's exact provider/store/session
        # identity, not a native ID that a foreign store can also contain.
        selected = {(s["provider"], s["session_id"], s["source_identity_sha256"])
                    for s in samples if s["source_identity_sha256"] is not None}
        observation = {
            "schema": SCHEMA,
            "observed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "binary_sha256": digest,
            "attribution": "unknown_context_drops_are_not_gobstopper_savings",
            "report": report_status,
            "watch": watch_status,
            "sessions": session_rows(report, sessions, previous),
            "context_samples": samples,
            "retention": retention_summary(event_log_path(environment), selected),
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
    parser.add_argument("--provider", action="append", default=[],
                        help="also observe every active session of this provider")
    args = parser.parse_args()
    handlers = {kind: signal.signal(kind, interrupt) for kind in (signal.SIGTERM, signal.SIGINT)}
    try:
        observation = observe(args.binary, args.output_dir, args.session, args.provider)
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

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
EVENT_RECORD_BYTES = 16 * 1024
WATCH_STATE_BYTES = 4 * 1024 * 1024
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
            # Rust's JSON object reader refuses unpaired surrogate keys even
            # for additive fields; Python otherwise accepts those escapes.
            key.encode("utf-8")
            if key in result:
                raise ValueError("duplicate_json_key")
            result[key] = value
        return result

    def invalid_constant(_value):
        raise ValueError("invalid_json_constant")

    def finite_float(value):
        number = float(value)
        if not math.isfinite(number):
            raise ValueError("invalid_json_constant")
        return number

    return json.loads(raw, object_pairs_hook=object_pairs, parse_constant=invalid_constant,
                      parse_float=finite_float,
                      # Serde rejects a signed zero token for unsigned fields;
                      # do not normalize it into an accepted Python integer.
                      parse_int=lambda value: -0.0 if value == "-0" else int(value))


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
    raw_reason = value.get("contextReason")
    reasons = {"missing_component", "null_component", "malformed_component", "overflow",
               "invalid_ancestry", "invalid_record", "read_limit", "source_changed", "source_unavailable"}
    reason = (raw_reason if isinstance(raw_reason, str) and raw_reason in reasons else
              None if raw_reason is None else "invalid_record")
    raw_components = value.get("contextComponents")
    component_keys = ("input_tokens", "cache_read_tokens", "cache_creation_tokens", "output_tokens")
    components = ({key: number(raw_components.get(key)) for key in
                   component_keys}
                  if isinstance(raw_components, dict) else None)
    if raw_components is not None and not isinstance(raw_components, dict):
        reason = "invalid_record"
    elif components is not None:
        if any(raw_components.get(key) is not None and components[key] is None for key in component_keys):
            reason = "malformed_component"
        elif reason is None:
            reason = "invalid_record"
    # Complete accounting and partial components are deliberately disjoint.
    reported = number(value.get("reportedContextTokens"))
    complete = state == "reported" and reason is None and components is None and reported is not None
    if state == "reported" and not complete and reason is None:
        reason = "invalid_record"
    subtotal = (sum(v for v in components.values() if v is not None)
                if components and any(v is not None for v in components.values()) else None)
    return {
        "source_identity_sha256": digest(value.get("sourceIdentitySha256")),
        "context_state": "unknown" if state == "reported" and not complete else state,
        "context_tokens": reported if complete else None,
        "context_reason": reason,
        "context_components": components,
        "measured_component_subtotal": number(subtotal),
        "component_subtotal_basis": "known_numeric_components_not_complete_occupancy",
        "lifetime_scope": scope,
        "lifetime_input_tokens": number(value.get("lifetimeInputTokens")) if scope == "full" else None,
        "lifetime_cached_tokens": number(value.get("lifetimeCachedTokens")) if scope == "full" else None,
    }


def valid_token_observation(value):
    """Mirror the retained observation schema and Rust accounting invariants."""
    keys = {"source_sha256", "source_identity_sha256", "snapshot_manifest_sha256",
            "context_state", "context_tokens", "estimated_context_tokens", "lifetime_scope",
            "lifetime_input_tokens", "lifetime_cached_tokens"}
    if (not isinstance(value, dict) or value.keys() - keys
            or digest(value.get("source_sha256")) is None
            or digest(value.get("source_identity_sha256")) is None
            or number(value.get("estimated_context_tokens")) is None
            or value.get("context_state") not in ("absent", "unknown", "reported", "reset")
            or value.get("lifetime_scope") not in ("absent", "partial", "full")):
        return False
    if (value.get("snapshot_manifest_sha256") is not None
            and digest(value["snapshot_manifest_sha256"]) is None):
        return False
    context = value.get("context_tokens")
    if (context is not None and number(context) is None
            or (value["context_state"] == "reported") != (context is not None)):
        return False
    lifetime, cached = value.get("lifetime_input_tokens"), value.get("lifetime_cached_tokens")
    if value["lifetime_scope"] == "full":
        return number(lifetime) is not None and number(cached) is not None and cached <= lifetime
    return lifetime is None and cached is None


def valid_compaction_event(event):
    """Match CompactionEvent deserialization plus events::valid_event.

    Optional fields may be absent or null, and unknown top-level fields remain
    additive. TokenObservation has its own closed field set. Schema validity is
    separate from whether an event can qualify as paired retention evidence.
    """
    if (not isinstance(event, dict)
            or event.get("schema") != "gobstopper/compaction-events-v1"
            or event.get("provider") not in ("codex", "claude_code", "devin")
            or event.get("action") not in ("provider_compact", "transcript_compact", "none")
            or event.get("outcome") not in ("applied", "planned", "failed", "skipped", "blocked")):
        return False
    for key, pattern in (("session_id", r"[A-Za-z0-9_.-]{1,256}"),
                         ("strategy", r"[A-Za-z0-9_.:-]{1,128}")):
        if not isinstance(event.get(key), str) or not re.fullmatch(pattern, event[key]):
            return False
    if any(number(event.get(key)) is None for key in
           ("ts", "trigger_tokens", "context_tokens_before", "context_tokens_after",
            "est_reclaimed_tokens", "items_covered", "duration_ms")):
        return False
    if event["est_reclaimed_tokens"] != max(0, event["context_tokens_before"] - event["context_tokens_after"]):
        return False
    if event.get("error_code") not in (None, "io", "provider_rejected", "apply_failed",
            "verification_failed", "custody_unavailable", "unattributed_provider_hook",
            "unresolved_context", "native_unqualified", "spawn_failed", "parent_thread",
            "provider_noop", "quota_limited"):
        return False
    if any(event.get(key) is not None and digest(event[key]) is None
           for key in ("source_identity_sha256", "binary_sha256", "config_sha256",
                       "experiment_sha256", "snapshot_before_sha256", "snapshot_after_sha256")):
        return False
    cohort, percent = event.get("decision_cohort"), event.get("rollout_percent")
    if not ((cohort in (None, "ungated") and percent is None)
            or (cohort in ("treatment", "control") and type(percent) is int and 0 <= percent <= 100)):
        return False
    for key in ("before_observation", "after_observation"):
        if event.get(key) is not None and not valid_token_observation(event[key]):
            return False
    total, literal, lexical = (event.get(key) for key in
                               ("retention_total", "retention_retained", "retention_lexical"))
    return ((total is None and literal is None and lexical is None)
            or (all(number(value) is not None for value in (total, literal, lexical))
                and literal <= total and lexical <= total))


def paired_evidence(event):
    if not valid_compaction_event(event):
        return None
    identity = event.get("source_identity_sha256")
    if (identity is None or event["outcome"] != "applied"
            or event["action"] not in ("provider_compact", "transcript_compact")
            or event.get("error_code") is not None):
        return None
    pair = []
    for side in ("before", "after"):
        snapshot = digest(event.get(f"snapshot_{side}_sha256"))
        observation = event.get(f"{side}_observation")
        if (snapshot is None or not valid_token_observation(observation)
                or observation.get("source_identity_sha256") != identity
                or observation.get("snapshot_manifest_sha256") != snapshot
                or digest(observation.get("source_sha256")) is None):
            return None
        pair.append(snapshot)
    return (event["provider"], event["session_id"], identity), tuple(pair)


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


def coverage_summary(report, sessions, providers, samples):
    """Coverage is distinct from command success and actual watcher decisions."""
    wanted, opted_in = set(sessions), set(providers)
    matched, eligible, identifier_omissions, states, reasons = set(), 0, 0, {}, {}
    for row in report.get("sessions", []):
        if not isinstance(row, dict) or not isinstance(row.get("gobstopper"), dict):
            continue
        fields = row["gobstopper"]
        session, provider = fields.get("sessionIdNative"), row.get("provider")
        if provider not in ("codex", "claude_code", "devin"):
            continue
        if not isinstance(session, str) or not SESSION_ID.fullmatch(session):
            if provider in opted_in:
                identifier_omissions += 1
            continue
        if session in wanted and provider == "codex":
            matched.add(session)
        if provider in ("codex", "claude_code", "devin") and (session in wanted or provider in opted_in):
            eligible += 1
    for sample in samples:
        state = sample["context_state"]
        states[state] = states.get(state, 0) + 1
        if sample["context_reason"]:
            reason = sample["context_reason"]
            reasons[reason] = reasons.get(reason, 0) + 1
    extension = report.get("gobstopper", {})
    raw_discovery = extension.get("discovery") if isinstance(extension, dict) else None
    discovery = []
    discovery_seen = set()
    if isinstance(raw_discovery, list) and len(raw_discovery) <= 3:
        for row in raw_discovery:
            if (not isinstance(row, dict) or row.get("provider") not in ("codex", "claude_code", "devin")
                    or row.get("source_state") not in ("available", "missing", "unavailable")
                    or row["provider"] in discovery_seen
                    or any(number(row.get(key)) is None for key in
                           ("scanned", "selected", "invalid_records", "io_errors", "omitted"))
                    or type(row.get("truncated")) is not bool
                    or row["selected"] > row["scanned"]):
                continue
            discovery_seen.add(row["provider"])
            discovery.append({"provider": row["provider"], "source_state": row["source_state"],
                              **{key: number(row.get(key)) for key in
                                 ("scanned", "selected", "invalid_records", "io_errors", "omitted")},
                              "truncated": row.get("truncated") if type(row.get("truncated")) is bool else None})
    exported = extension.get("coverage", {}) if isinstance(extension, dict) else {}
    if not isinstance(exported, dict):
        exported = {}
    issues = []
    if wanted and not matched:
        issues.append("selected_overlap_empty")
    if wanted - matched:
        issues.append("selected_sessions_unavailable")
    if any(s["context_tokens"] is None for s in samples):
        issues.append("incomplete_context_measurements")
    if identifier_omissions:
        issues.append("unsupported_session_identifiers")
    if len(discovery) != 3:
        issues.append("discovery_status_unavailable")
    if any(row["source_state"] != "available" or row["io_errors"] or row["invalid_records"] for row in discovery):
        issues.append("discovery_gaps")
    if eligible > len(samples) or exported.get("truncated") is True or any(row["truncated"] or row["omitted"] for row in discovery):
        issues.append("coverage_truncated")
    return {"selected_sessions": len(wanted), "selected_active_overlap": len(matched),
            "selected_unavailable": len(wanted - matched),
            "eligible_context_samples": eligible, "exported_context_samples": len(samples),
            "identifier_omissions": identifier_omissions,
            "sample_limit": 256, "samples_truncated": eligible > len(samples),
            "context_states": states, "context_reasons": reasons,
            "complete_context_samples": sum(s["context_tokens"] is not None for s in samples),
            "partial_component_samples": sum(s["measured_component_subtotal"] is not None for s in samples),
            "discovery": discovery, "discovery_available": len(discovery) == 3,
            "report_truncated": exported.get("truncated") if type(exported.get("truncated")) is bool else None,
            "issues": issues, "scope": "selected_codex_sessions_and_explicit_provider_opt_ins"}


def watcher_checkpoints(runtime, binary_sha256, now_ms):
    """Read private checkpoints only; do not load configuration or execute it."""
    result = []
    decisions = ("discovered", "legacy_unresolved", "native_unresolved", "settled",
                 "cooldown", "below_trigger", "native_unqualified")
    for provider in ("codex", "claude_code", "devin"):
        row = {"provider": provider, "available": False, "status": "unavailable"}
        path = runtime / ("watch-state-" + provider + ".json")
        try:
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(fd, "rb") as source:
                private_file(source.fileno())
                before = os.fstat(source.fileno())
                raw = source.read(WATCH_STATE_BYTES + 1)
                after = os.fstat(source.fileno())
            identity = lambda info: (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)
            if len(raw) > WATCH_STATE_BYTES or identity(before) != identity(after) or identity(after) != identity(path.lstat()):
                raise MonitorError("checkpoint_changed_or_limited")
            value = strict_json(raw)
            if (not isinstance(value, dict) or type(value.get("generation")) is not int or value["generation"] != 9
                    or type(value.get("checkpoint_schema")) is not int or value["checkpoint_schema"] != 1):
                raise MonitorError("checkpoint_schema_unavailable")
            artifact = digest(value.get("artifact_sha256"))
            config = digest(value.get("config_sha256"))
            started = number(value.get("pass_started_at_ms"))
            completed = number(value.get("pass_completed_at_ms"))
            interval = number(value.get("interval_secs"))
            if artifact is None or config is None or started is None or interval is None or interval == 0:
                raise MonitorError("invalid_checkpoint")
            if value.get("pass_completed_at_ms") is not None and completed is None:
                raise MonitorError("invalid_checkpoint")
            if started > now_ms or completed is not None and (completed < started or completed > now_ms):
                raise MonitorError("checkpoint_clock_mismatch")
            age = now_ms - (completed if completed is not None else started)
            status = ("artifact_mismatch" if artifact != binary_sha256 else
                      "stale" if age > max(180_000, interval * 3000) else
                      "in_progress" if completed is None else "fresh")
            raw_decisions = value.get("decisions", {})
            if (not isinstance(raw_decisions, dict) or any(number(raw_decisions.get(key)) is None for key in decisions)
                    or sum(raw_decisions[key] for key in decisions if key != "discovered") > raw_decisions["discovered"]
                    or type(value.get("active_only")) is not bool
                    or value.get("native_activation") not in ("unqualified", "isolated_fixtures_only")):
                raise MonitorError("invalid_checkpoint")
            row.update(available=True, status=status, artifact_sha256=artifact, config_sha256=config,
                       pass_started_at_ms=started, pass_completed_at_ms=completed, age_ms=age,
                       interval_secs=interval, active_only=value.get("active_only") if type(value.get("active_only")) is bool else None,
                       native_activation=value.get("native_activation") if value.get("native_activation") in
                       ("unqualified", "isolated_fixtures_only") else None,
                       decisions={key: number(raw_decisions.get(key)) for key in decisions})
        except FileNotFoundError:
            row["status"] = "missing"
        except (OSError, ValueError, TypeError, RecursionError, MonitorError):
            row["status"] = "unavailable"
        result.append(row)
    return result


def save_observation(directory, observation):
    line = (json.dumps(observation, separators=(",", ":"), sort_keys=True) + "\n").encode()
    if len(line) > min(1024 * 1024, LOG_BYTES):
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
    Invalid history makes qualification unavailable for the entire observation,
    since an invalid record may contradict an otherwise accepted evidence pair.
    This reads one bounded local generation, not a lifetime retention total.
    """
    out = {"measured": 0, "checks": 0, "literal": 0, "lexical": 0,
           "lossy_sessions": [], "available": False, "error": None,
           "invalid_records": 0, "oversized_records": 0, "conflicting_pairs": 0}
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
    except FileNotFoundError:
        out["error"] = "event_log_absent"
        return out
    except OSError:
        out["error"] = "event_log_unavailable"
        return out
    pairs = {}
    for line in data.split(b"\n"):
        if len(line) > EVENT_RECORD_BYTES:
            out["oversized_records"] += 1
            continue
        if not line.strip(b" \t\n\r\f"):
            continue
        try:
            event = strict_json(line.decode("utf-8"))
        except (ValueError, UnicodeError, RecursionError):
            out["invalid_records"] += 1
            continue
        if not valid_compaction_event(event):
            out["invalid_records"] += 1
            continue
        evidence = paired_evidence(event)
        if evidence is None or evidence[0] not in selected:
            continue
        total, literal = event.get("retention_total"), event.get("retention_retained")
        lexical = event.get("retention_lexical")
        values = (total, literal, lexical)
        if total is None:
            continue
        # A manifest identifies one exact byte sequence. Conflicting source
        # hashes for the same manifests invalidate the pair, not a new sample.
        values += (event["before_observation"]["source_sha256"],
                   event["after_observation"]["source_sha256"])
        if evidence in pairs and pairs[evidence] != values:
            pairs[evidence] = None
        else:
            pairs[evidence] = values
    if out["invalid_records"] or out["oversized_records"]:
        out["error"] = "event_log_invalid"
        return out
    out["available"] = True
    seen_lossy = set()
    for (identity, _pair), values in pairs.items():
        if values is None:
            out["conflicting_pairs"] += 1
            continue
        total, literal, lexical = values[:3]
        out["measured"] += 1
        # Preserve the CLI's saturating u64 aggregate contract even when each
        # individually valid denominator is near the schema's numeric limit.
        for key, value in (("checks", total), ("literal", literal), ("lexical", lexical)):
            out[key] = min(2**64 - 1, out[key] + value)
        session = identity[1]
        if total and lexical * 2 < total and session not in seen_lossy:
            seen_lossy.add(session)
            out["lossy_sessions"].append(session)
    return out


def observe(binary, output_dir, sessions, providers=()):
    if len(sessions) > 128 or any(not SESSION_ID.fullmatch(s) for s in sessions):
        raise MonitorError("invalid_sessions")
    if not sessions and not providers:
        raise MonitorError("no_sessions_or_providers")
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
        with tempfile.TemporaryDirectory(prefix=".monitor-config-", dir=output_dir) as config:
            environment = {key: value for key, value in os.environ.items()
                           if not key.startswith("GOBSTOPPER_")}
            environment["GOBSTOPPER_SCORER"] = "heuristic"
            # Even dry-run can execute configured extensions. Empty config
            # guarantees the deterministic built-in policy and no plugins.
            environment["XDG_CONFIG_HOME"] = config
            # Each command owns a full budget: a slow report must not leave
            # the dry-run watch an already-exhausted deadline.
            report_status, stdout, _ = run_command(
                [str(executable), "report", "--active-only", "--context-only"],
                environment, time.monotonic() + TIMEOUT_SECONDS)
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
            # --eval-budget bounds transcript load/evaluate inside the
            # command itself: under host load a full provider dry-run can
            # exceed the wrapper budget even when healthy, so the pass
            # defers its remaining sessions instead of hitting TIMEOUT.
            watch_status, _, stderr = run_command(
                [str(executable), "watch", "--dry-run", "--active-only", "--once",
                 "--eval-budget", str(TIMEOUT_SECONDS - 7)],
                environment, time.monotonic() + TIMEOUT_SECONDS)
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
        coverage = coverage_summary(report, sessions, providers, samples)
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
            "coverage": coverage,
            "watcher_checkpoints": watcher_checkpoints(event_log_path(environment).parent, digest,
                                                       time.time_ns() // 1_000_000),
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
    parser.add_argument("--session", action="append", default=[],
                        help="track this codex session id in detail")
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

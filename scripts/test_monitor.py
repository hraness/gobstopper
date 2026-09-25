import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("monitor", Path(__file__).with_name("monitor.py"))
monitor = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(monitor)

A = "01a00000-aaaa-7000-aaaa-aaaaaaaaaaaa"
B = "01a00000-bbbb-7000-bbbb-bbbbbbbbbbbb"
UNRELATED = "01a00000-cccc-7000-cccc-cccccccccccc"
RESOURCE_FIELDS = {
    "user_cpu_us", "system_cpu_us", "minor_page_faults", "major_page_faults",
    "voluntary_context_switches", "involuntary_context_switches",
}


def report(context=100000, native=2):
    return {
        "schemaVersion": 1, "profile": "session-observations-v1",
        "sessions": [{
            "provider": "codex", "secret": "SECRET_TRANSCRIPT_SENTINEL",
            "gobstopper": {
                "sessionIdNative": session,
                "contextTokens": context,
                "reportedContextTokens": context,
                "contextState": "reported",
                "sourceIdentitySha256": "a" * 64 if session == A else "b" * 64,
                "lifetimeScope": "full",
                "modelContextWindow": 258400,
                "lifetimeInputTokens": 500000,
                "lifetimeCachedTokens": 400000,
                "lastActivityMs": 1790000000000,
                "closedSessionCompact": "available",
                "compactions": {"applied": 10, "nativeHookApplied": native},
            },
        } for session in (A, UNRELATED)],
    }


def bind_event(event):
    if not isinstance(event, dict):
        return event
    return {"schema": "gobstopper/compaction-events-v1", "provider": "codex",
            "action": "provider_compact", "error_code": None,
            "strategy": "auto", "ts": 1, "trigger_tokens": 100,
            "context_tokens_before": 100, "context_tokens_after": 50,
            "est_reclaimed_tokens": 50, "items_covered": 0, "duration_ms": 1,
            "outcome": "applied", "source_identity_sha256": "a" * 64,
            "snapshot_before_sha256": "b" * 64, "snapshot_after_sha256": "c" * 64,
            "before_observation": {"source_identity_sha256": "a" * 64, "source_sha256": "d" * 64,
                                   "snapshot_manifest_sha256": "b" * 64, "context_state": "reported",
                                   "context_tokens": 100, "estimated_context_tokens": 100,
                                   "lifetime_scope": "absent", "lifetime_input_tokens": None,
                                   "lifetime_cached_tokens": None},
            "after_observation": {"source_identity_sha256": "a" * 64, "source_sha256": "e" * 64,
                                  "snapshot_manifest_sha256": "c" * 64, "context_state": "reported",
                                  "context_tokens": 50, "estimated_context_tokens": 50,
                                  "lifetime_scope": "absent", "lifetime_input_tokens": None,
                                  "lifetime_cached_tokens": None}, **event}


def selected_sources(*sessions):
    return {("codex", session, "a" * 64) for session in sessions}


class MonitorTests(unittest.TestCase):
    def test_partial_components_never_become_complete_context(self):
        fields = monitor.accounting_fields({"contextState": "unknown", "contextReason": "null_component",
            "reportedContextTokens": 9999, "contextComponents": {"input_tokens": 100,
            "cache_read_tokens": 200, "cache_creation_tokens": None, "output_tokens": 5}})
        self.assertIsNone(fields["context_tokens"])
        self.assertEqual(fields["measured_component_subtotal"], 305)
        self.assertEqual(fields["context_reason"], "null_component")
        fields = monitor.accounting_fields({"contextState": "reported", "reportedContextTokens": 99,
                                           "contextReason": {"private": "must not appear"}})
        self.assertNotIn("private", json.dumps(fields))
        self.assertIsNone(fields["context_tokens"])
        self.assertEqual(fields["context_state"], "unknown")
        fields = monitor.accounting_fields({"contextState": "unknown", "contextComponents": {
            "input_tokens": 2**64 - 1, "output_tokens": 1}})
        self.assertIsNone(fields["measured_component_subtotal"])

    def test_malformed_measurement_metadata_cannot_become_complete_context(self):
        for extra in ({"contextReason": "future_unknown_reason"}, {"contextComponents": []},
                      {"contextComponents": {"input_tokens": True}}, {"reportedContextTokens": True},
                      {"contextComponents": {"input_tokens": 100, "output_tokens": "private"}}):
            with self.subTest(extra=extra):
                fields = monitor.accounting_fields({"contextState": "reported", "reportedContextTokens": 99, **extra})
                self.assertEqual(fields["context_state"], "unknown")
                self.assertIsNone(fields["context_tokens"])
                self.assertNotIn("private", json.dumps(fields))
        zero = monitor.accounting_fields({"contextState": "reported", "reportedContextTokens": 0})
        self.assertEqual(zero["context_state"], "reported")
        self.assertEqual(zero["context_tokens"], 0)
        with self.assertRaises(ValueError):
            monitor.strict_json('{"extra":1e999}')

    def test_coverage_distinguishes_empty_selection_unknown_usage_and_caps(self):
        value = report()
        value["gobstopper"] = {"coverage": {"truncated": True}, "discovery": [
            {"provider": provider, "source_state": "unavailable" if provider == "devin" else "available",
             "scanned": 1, "selected": 1, "invalid_records": 0, "io_errors": int(provider == "devin"),
             "omitted": 0, "truncated": False} for provider in ("codex", "claude_code", "devin")]}
        coverage = monitor.coverage_summary(value, [B], ["codex"], monitor.context_samples(value, [B], ["codex"]))
        self.assertEqual(coverage["selected_active_overlap"], 0)
        self.assertEqual(coverage["selected_unavailable"], 1)
        self.assertEqual(coverage["complete_context_samples"], 2)
        self.assertTrue(coverage["report_truncated"])
        self.assertTrue(coverage["discovery_available"])
        self.assertIn("selected_overlap_empty", coverage["issues"])
        self.assertIn("discovery_gaps", coverage["issues"])
        self.assertIn("coverage_truncated", coverage["issues"])
        value["gobstopper"]["discovery"][2] = value["gobstopper"]["discovery"][0]
        duplicate = monitor.coverage_summary(value, [B], ["codex"], [])
        self.assertFalse(duplicate["discovery_available"])
        value["gobstopper"]["discovery"][2] = {
            "provider": "devin", "source_state": "available", "scanned": 0, "selected": 0,
            "invalid_records": 0, "io_errors": "private", "omitted": 0, "truncated": False}
        invalid = monitor.coverage_summary(value, [B], ["codex"], [])
        self.assertFalse(invalid["discovery_available"])
        self.assertIn("discovery_status_unavailable", invalid["issues"])
        self.assertNotIn("private", json.dumps(invalid))
        value["sessions"] *= 150
        samples = monitor.context_samples(value, [A], ["codex"])
        coverage = monitor.coverage_summary(value, [A], ["codex"], samples)
        self.assertEqual(coverage["eligible_context_samples"], 300)
        self.assertEqual(len(samples), 256)
        self.assertTrue(coverage["samples_truncated"])

    def test_checkpoint_freshness_is_separate_from_process_and_command_health(self):
        runtime = self.root / "runtime"
        runtime.mkdir(mode=0o700)
        path = runtime / "watch-state-codex.json"
        checkpoint = {"generation": 9, "checkpoint_schema": 1, "artifact_sha256": "a" * 64,
                      "config_sha256": "b" * 64, "pass_started_at_ms": 1000,
                      "pass_completed_at_ms": 1200, "interval_secs": 60, "active_only": False,
                      "native_activation": "unqualified", "decisions": {
                          "discovered": 2, "legacy_unresolved": 0, "native_unresolved": 0,
                          "settled": 0, "cooldown": 0, "below_trigger": 0, "native_unqualified": 2},
                      "settled": {"PRIVATE_SESSION_AND_PATH": "ignored"}}
        def write():
            path.write_text(json.dumps(checkpoint))
            path.chmod(0o600)
        write()
        rows = monitor.watcher_checkpoints(runtime, "a" * 64, 1500)
        self.assertEqual([row["status"] for row in rows], ["fresh", "missing", "missing"])
        self.assertNotIn("PRIVATE", json.dumps(rows))
        self.assertEqual(rows[0]["decisions"]["native_unqualified"], 2)
        checkpoint["settled"] = {"ignored": "x" * monitor.WATCH_STATE_BYTES}
        write()
        self.assertEqual(monitor.watcher_checkpoints(runtime, "a" * 64, 1500)[0]["status"], "unavailable")
        checkpoint["settled"] = {}
        write()
        self.assertEqual(monitor.watcher_checkpoints(runtime, "c" * 64, 1500)[0]["status"], "artifact_mismatch")
        self.assertEqual(monitor.watcher_checkpoints(runtime, "a" * 64, 200000)[0]["status"], "stale")
        checkpoint["pass_completed_at_ms"] = None
        write()
        self.assertEqual(monitor.watcher_checkpoints(runtime, "a" * 64, 1500)[0]["status"], "in_progress")
        checkpoint["decisions"]["settled"] = 1
        write()
        self.assertEqual(monitor.watcher_checkpoints(runtime, "a" * 64, 1500)[0]["status"], "unavailable")
        checkpoint["decisions"]["settled"] = 0
        checkpoint["interval_secs"] = 0
        write()
        self.assertEqual(monitor.watcher_checkpoints(runtime, "a" * 64, 1500)[0]["status"], "unavailable")
        checkpoint["interval_secs"] = 60
        checkpoint["pass_completed_at_ms"] = "PRIVATE_INVALID_TIME"
        write()
        self.assertEqual(monitor.watcher_checkpoints(runtime, "a" * 64, 1500)[0]["status"], "unavailable")
        checkpoint["pass_completed_at_ms"] = None
        checkpoint["pass_started_at_ms"] = 1600
        write()
        self.assertEqual(monitor.watcher_checkpoints(runtime, "a" * 64, 1500)[0]["status"], "unavailable")
        path.unlink()
        os.mkfifo(path, 0o600)
        started = time.monotonic()
        self.assertEqual(monitor.watcher_checkpoints(runtime, "a" * 64, 1500)[0]["status"], "unavailable")
        self.assertLess(time.monotonic() - started, 1)

    def test_coverage_records_identifier_omissions_without_exposing_values(self):
        value = report()
        value["sessions"][0]["gobstopper"]["sessionIdNative"] = "PRIVATE.with.unsupported.syntax"
        samples = monitor.context_samples(value, [A], ["codex"])
        coverage = monitor.coverage_summary(value, [A], ["codex"], samples)
        self.assertEqual(coverage["identifier_omissions"], 1)
        self.assertIn("unsupported_session_identifiers", coverage["issues"])
        self.assertNotIn("PRIVATE", json.dumps(coverage))
        value["sessions"][0]["provider"] = []
        self.assertEqual(monitor.coverage_summary(value, [A], ["codex"], samples)["identifier_omissions"], 0)

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="gob-monitor-test-")
        self.root = Path(self.scratch.name).resolve()
        self.output = self.root / "observations"
        self.report_file = self.root / "report.json"
        self.report_file.write_text(json.dumps(report()))
        self.calls = self.root / "calls.jsonl"
        self.transcript = self.root / "live.jsonl"
        self.transcript.write_text("PRIVATE_UNCHANGED_SESSION\n")
        self.binary = self.root / "gobstopper"
        self.binary.write_text(f"#!{sys.executable}\n" + r'''
import json, os, sys, time
from pathlib import Path
config = Path(os.environ["XDG_CONFIG_HOME"])
call = {
    "arguments": sys.argv[1:],
    "gobstopper_env": {k: v for k, v in os.environ.items() if k.startswith("GOBSTOPPER_")},
    "empty_config": not (config / "gobstopper/config.toml").exists(),
}
with open(os.environ["STUB_CALLS"], "a") as log:
    log.write(json.dumps(call) + "\n")
if os.environ.get("STUB_PID"):
    with open(os.environ["STUB_PID"], "a") as pids:
        pids.write(str(os.getpid()) + "\n")
if os.environ.get("STUB_SLEEP"):
    time.sleep(float(os.environ["STUB_SLEEP"]))
if sys.argv[1:] == ["report", "--active-only", "--context-only"]:
    sys.stdout.write(Path(os.environ["STUB_REPORT"]).read_text())
elif sys.argv[1:] == ["watch", "--dry-run", "--active-only", "--once", "--eval-budget", "38"]:
    if os.environ.get("STUB_WATCH_FAILURE"):
        print("load 01a00000-aaaa-7000-aaaa-aaaaaaaaaaaa failed: SECRET_ERROR_TEXT", file=sys.stderr)
        raise SystemExit(0)
    print("[dry-run] codex 01a00000-aaa: SECRET_TRANSCRIPT_SENTINEL", file=sys.stderr)
    print("[dry-run] codex 01a00000-ccc: UNRELATED_PRIVATE_PLAN", file=sys.stderr)
else:
    raise SystemExit(9)
''')
        self.binary.chmod(0o700)
        self.environment = patch.dict(os.environ, {
            "STUB_REPORT": str(self.report_file), "STUB_CALLS": str(self.calls),
            "GOBSTOPPER_SCORER": "jev", "GOBSTOPPER_DIGEST": "apple",
            "GOBSTOPPER_EVAL_JUDGE": "jev", "GOBSTOPPER_JEV_API_KEY": "SECRET_ENV_SENTINEL",
            "XDG_DATA_HOME": str(self.root / "data"),
        })
        self.environment.start()

    def tearDown(self):
        self.environment.stop()
        self.scratch.cleanup()

    def sample(self, sessions=None):
        return monitor.observe(self.binary, self.output, sessions or [A, B])

    def assert_resources(self, value):
        self.assertIsInstance(value, dict)
        self.assertEqual(set(value), RESOURCE_FIELDS)
        for counter in value.values():
            self.assertIs(type(counter), int)
            self.assertGreaterEqual(counter, 0)
            self.assertLessEqual(counter, 2**64 - 1)

    def test_successful_children_have_bounded_numeric_resource_deltas(self):
        observation = self.sample()
        for command in ("report", "watch"):
            self.assertIsNone(observation[command]["error"])
            self.assertEqual(observation[command]["exit_code"], 0)
            self.assert_resources(observation[command]["resources"])
        self.assertGreater(observation["report"]["resources"]["user_cpu_us"], 0)

    def test_each_timed_out_child_is_reaped_under_its_own_deadline(self):
        pid_file = self.root / "timeout.pid"
        with patch.dict(os.environ, {"STUB_SLEEP": "30", "STUB_PID": str(pid_file)}), \
                patch.object(monitor, "TIMEOUT_SECONDS", 3):
            observation = self.sample()
        for command in ("report", "watch"):
            self.assertEqual(observation[command]["error"], "timeout")
            self.assertIsNone(observation[command]["exit_code"])
            self.assert_resources(observation[command]["resources"])
        # The report's exhausted budget is not the watch's: watch still
        # started and ran its own full budget before being reaped.
        self.assertGreaterEqual(observation["watch"]["duration_ms"], 2500)
        calls = [json.loads(line)["arguments"][0] for line in self.calls.read_text().splitlines()]
        self.assertEqual(calls, ["report", "watch"])
        pids = [int(line) for line in pid_file.read_text().split()]
        self.assertEqual(len(pids), 2)
        for pid in pids:
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)

    def test_resource_read_failure_preserves_result_and_timeout_cleanup(self):
        environment = dict(os.environ, XDG_CONFIG_HOME=str(self.root))
        with patch.object(monitor.resource, "getrusage", side_effect=RuntimeError("PRIVATE_METRIC_ERROR")):
            result, _, _ = monitor.run_command(
                [str(self.binary), "unsupported"], environment, time.monotonic() + 3)
        self.assertEqual(result["error"], "command_failed")
        self.assertEqual(result["exit_code"], 9)
        self.assertIsNone(result["resources"])
        self.assertNotIn("PRIVATE", json.dumps(result))

        pid_file = self.root / "metric-failure.pid"
        environment.update(STUB_SLEEP="30", STUB_PID=str(pid_file))
        before = monitor.child_resources()
        with patch.object(monitor.resource, "getrusage", side_effect=[before, RuntimeError("PRIVATE_METRIC_ERROR")]):
            result, _, _ = monitor.run_command(
                [str(self.binary), "report", "--active-only", "--context-only"], environment, time.monotonic() + 0.5)
        self.assertEqual(result["error"], "timeout")
        self.assertIsNone(result["resources"])
        with self.assertRaises(ProcessLookupError):
            os.kill(int(pid_file.read_text()), 0)

    def test_unavailable_resource_module_does_not_hide_success(self):
        with patch.object(monitor, "resource", None):
            observation = self.sample()
        for command in ("report", "watch"):
            self.assertEqual(observation[command]["exit_code"], 0)
            self.assertIsNone(observation[command]["error"])
            self.assertIsNone(observation[command]["resources"])

    def test_resource_counter_validation_rejects_invalid_and_regressing_values(self):
        before = SimpleNamespace(ru_utime=1.25, ru_stime=0.5, ru_minflt=10,
                                 ru_majflt=2, ru_nvcsw=30, ru_nivcsw=7)
        after = SimpleNamespace(ru_utime=1.5, ru_stime=0.5, ru_minflt=14,
                                ru_majflt=2, ru_nvcsw=35, ru_nivcsw=9)
        self.assertEqual(monitor.resource_delta(before, after), {
            "user_cpu_us": 250000, "system_cpu_us": 0, "minor_page_faults": 4,
            "major_page_faults": 0, "voluntary_context_switches": 5,
            "involuntary_context_switches": 2,
        })
        for invalid in (float("nan"), float("inf"), -1, True, "PRIVATE", 2**128):
            with self.subTest(invalid_type=type(invalid).__name__):
                broken = SimpleNamespace(**{field: invalid for field in vars(after)})
                self.assertEqual(set(monitor.resource_delta(before, broken).values()), {None})
        self.assertEqual(set(monitor.resource_delta(after, before).values()), {None, 0})
        self.assertIsNone(monitor.resource_delta(before, object()))
        self.assertIsNone(monitor.resource_delta(None, after))

    def test_realistic_samples_are_private_allowlisted_and_nonmutating(self):
        source = self.transcript.read_bytes()
        with patch.object(monitor, "run_command", wraps=monitor.run_command) as commands:
            first = self.sample()
        self.assertEqual(monitor.TIMEOUT_SECONDS, 45)
        self.assertEqual(len(commands.call_args_list), 2)
        report_deadline, watch_deadline = (call.args[2] for call in commands.call_args_list)
        self.assertGreater(watch_deadline, report_deadline)
        self.assertLess(watch_deadline - report_deadline, monitor.TIMEOUT_SECONDS)
        self.assertEqual(first["binary_sha256"], hashlib.sha256(self.binary.read_bytes()).hexdigest())
        self.assertEqual(first["watch"]["plan_count"], 1)
        self.assertEqual(first["sessions"][0]["context_tokens"], 100000)
        self.assertIsNone(first["sessions"][0]["context_drop_tokens"])
        self.assertFalse(first["sessions"][1]["available"])
        self.assertIsNone(first["sessions"][1]["lifetime_input_tokens"])
        self.report_file.write_text(json.dumps(report(context=40000, native=3)))
        second = self.sample()
        self.assertEqual(second["sessions"][0]["context_drop_tokens"], 60000)
        self.assertEqual(second["sessions"][0]["native_hook_applied_delta"], 1)
        self.assertEqual(second["attribution"], "unknown_context_drops_are_not_gobstopper_savings")
        self.report_file.write_text(json.dumps(report(context=50000, native=3)))
        third = self.sample()
        self.assertEqual(third["sessions"][0]["context_drop_tokens"], 0)
        self.assertEqual(third["sessions"][0]["native_hook_applied_delta"], 0)
        self.assertEqual(self.transcript.read_bytes(), source)
        self.assertEqual(json.loads((self.output / "latest.json").read_text()), third)
        stored = (self.output / "observations.jsonl").read_text()
        self.assertEqual(len(stored.splitlines()), 3)
        for forbidden in ("SECRET", UNRELATED, str(self.root), "PRIVATE_PLAN"):
            self.assertNotIn(forbidden, stored)
        self.assertEqual(stat.S_IMODE(self.output.stat().st_mode), 0o700)
        for name in ("latest.json", "observations.jsonl", ".monitor.lock"):
            self.assertEqual(stat.S_IMODE((self.output / name).stat().st_mode), 0o600)
        calls = [json.loads(line) for line in self.calls.read_text().splitlines()]
        self.assertEqual(len(calls), 6)
        self.assertTrue(all(call["empty_config"] for call in calls))
        self.assertTrue(all(call["gobstopper_env"] == {"GOBSTOPPER_SCORER": "heuristic"} for call in calls))
        self.assertEqual([call["arguments"] for call in calls], [
            ["report", "--active-only", "--context-only"],
            ["watch", "--dry-run", "--active-only", "--once", "--eval-budget", "38"],
        ] * 3)

    def test_context_samples_respect_allowlist_and_provider_opt_in(self):
        value = report()
        devin_row = json.loads(json.dumps(value["sessions"][0]))
        devin_row["provider"] = "devin"
        devin_row["gobstopper"]["sessionIdNative"] = "devin-session-1"
        value["sessions"].append(devin_row)
        self.report_file.write_text(json.dumps(value))

        # Default: only allowlisted sessions; neither the unrelated codex
        # row nor the devin row is sampled.
        observation = self.sample()
        sampled = {(r["provider"], r["session_id"]) for r in observation["context_samples"]}
        self.assertEqual(sampled, {("codex", A)})

        # Opted-in provider rows join the allowlist; unrelated codex stays out.
        observation = monitor.observe(
            self.binary, self.output, [A, B], providers=["devin"])
        sampled = {(r["provider"], r["session_id"]) for r in observation["context_samples"]}
        self.assertEqual(sampled, {("codex", A), ("devin", "devin-session-1")})

        # An unknown provider name is rejected before any child runs.
        with self.assertRaises(monitor.MonitorError):
            monitor.observe(self.binary, self.output, [A], providers=["other"])

    def test_unavailable_fields_and_compaction_boundary_are_not_zero_savings(self):
        self.sample()
        value = report(context=0, native=None)
        value["sessions"][0]["gobstopper"]["contextState"] = "reset"
        self.report_file.write_text(json.dumps(value))
        sample = self.sample()["sessions"][0]
        self.assertIsNone(sample["context_tokens"])
        self.assertIsNone(sample["context_drop_tokens"])
        self.assertIsNone(sample["native_hook_applied_delta"])
        del value["sessions"][0]["gobstopper"]["compactions"]["nativeHookApplied"]
        self.report_file.write_text(json.dumps(value))
        self.assertIsNone(self.sample()["sessions"][0]["native_hook_applied"])

    def test_closed_session_compact_passthrough(self):
        row = self.sample()["sessions"][0]
        self.assertEqual(row["closed_session_compact"], "available")
        value = report()
        value["sessions"][0]["gobstopper"]["closedSessionCompact"] = "unavailable:sub-agent"
        self.report_file.write_text(json.dumps(value))
        row = self.sample()["sessions"][0]
        self.assertEqual(row["closed_session_compact"], "unavailable:sub-agent")
        # Unrecognized values stay absent, never pass through raw.
        value["sessions"][0]["gobstopper"]["closedSessionCompact"] = "unexpected"
        self.report_file.write_text(json.dumps(value))
        self.assertIsNone(self.sample()["sessions"][0]["closed_session_compact"])

    def test_retention_summary_counts_allowlisted_events_and_flags_lossy(self):
        log = self.root / "events.jsonl"
        events = [
            # measured, allowlisted, healthy
            {"session_id": A, "retention_total": 10, "retention_retained": 6,
             "retention_lexical": 8, "outcome": "applied"},
            # measured, allowlisted, lossy (lexical < half of checks)
            {"session_id": B, "retention_total": 20, "retention_retained": 1,
             "retention_lexical": 4},
            # unmeasured event — skipped
            {"session_id": A, "outcome": "applied"},
            # measured but not allowlisted — skipped
            {"session_id": UNRELATED, "retention_total": 5,
             "retention_retained": 5, "retention_lexical": 5},
        ]
        log.write_text("\n \t\r\n" + "".join(json.dumps(bind_event(e)) + "\n" for e in events))
        out = monitor.retention_summary(log, selected_sources(A, B))
        self.assertEqual(out["measured"], 2)
        self.assertEqual(out["checks"], 30)
        self.assertEqual(out["literal"], 7)
        self.assertEqual(out["lexical"], 12)
        self.assertEqual(out["lossy_sessions"], [B])
        self.assertTrue(out["available"])
        self.assertEqual(out["invalid_records"], 0)
        self.assertEqual(out["oversized_records"], 0)
        missing = monitor.retention_summary(log.with_name("nope"), selected_sources(A))
        self.assertEqual(missing["measured"], 0)
        self.assertFalse(missing["available"])
        self.assertEqual(missing["error"], "event_log_absent")

    def test_retention_rejects_missing_boolean_and_impossible_measurements(self):
        log = self.root / "retention.jsonl"
        valid = {"session_id": A, "retention_total": 10,
                 "retention_retained": 6, "retention_lexical": 8}
        invalid = []
        for field in ("retention_total", "retention_retained", "retention_lexical"):
            for value in (None, False, True, [], {}, "", -1, 2**64, 1.0):
                invalid.append({**valid, field: value})
            missing = dict(valid)
            del missing[field]
            invalid.append(missing)
        invalid.extend([{**valid, "retention_retained": 11},
                        {**valid, "retention_lexical": 11},
                        {**valid, "session_id": []},
                        {**valid, "session_id": {}}])
        for row in invalid:
            for events in ([valid, row], [row, valid], [valid, row, valid]):
                with self.subTest(row=row, order=events):
                    log.write_text("".join(json.dumps(bind_event(event)) + "\n" for event in events))
                    out = monitor.retention_summary(log, selected_sources(A))
                    self.assertFalse(out["available"])
                    self.assertEqual(out["error"], "event_log_invalid")
                    self.assertEqual(out["invalid_records"], 1)
                    self.assertEqual([out[key] for key in ("measured", "checks", "literal", "lexical")],
                                     [0, 0, 0, 0])

    def test_retention_read_is_bounded_and_invalid_utf8_is_unavailable(self):
        log = self.root / "retention.jsonl"
        log.write_bytes(b"\xff\n")
        result = monitor.retention_summary(log, selected_sources(A))
        self.assertEqual(result["measured"], 0)
        self.assertFalse(result["available"])
        self.assertEqual(result["error"], "event_log_invalid")
        self.assertEqual(result["invalid_records"], 1)
        log.write_bytes(b"x" * 33)
        with patch.object(monitor, "EVENTS_LOG_BYTES", 32):
            result = monitor.retention_summary(log, selected_sources(A))
        self.assertFalse(result["available"])
        self.assertEqual(result["error"], "event_log_limit")

    def test_reported_zero_and_foreign_source_are_not_context_savings(self):
        self.sample()
        value = report(context=0)
        self.report_file.write_text(json.dumps(value))
        row = self.sample()["sessions"][0]
        self.assertEqual(row["context_tokens"], 0)
        self.assertEqual(row["context_state"], "reported")
        self.assertIsNone(row["context_drop_tokens"])
        value = report(context=10)
        value["sessions"][0]["gobstopper"]["sourceIdentitySha256"] = "f" * 64
        value["sessions"][0]["gobstopper"]["lifetimeScope"] = "partial"
        self.report_file.write_text(json.dumps(value))
        row = self.sample()["sessions"][0]
        self.assertIsNone(row["context_drop_tokens"])
        self.assertIsNone(row["lifetime_input_tokens"])

    def test_legacy_or_foreign_retention_pairs_are_unqualified(self):
        log = self.root / "retention.jsonl"
        valid = bind_event({"session_id": A, "retention_total": 2,
                            "retention_retained": 2, "retention_lexical": 2})
        legacy = {key: value for key, value in valid.items()
                  if key not in ("source_identity_sha256", "snapshot_before_sha256",
                                 "snapshot_after_sha256", "before_observation", "after_observation")}
        foreign = json.loads(json.dumps(valid))
        foreign["after_observation"]["source_identity_sha256"] = "f" * 64
        log.write_text(json.dumps(legacy) + "\n" + json.dumps(foreign) + "\n")
        result = monitor.retention_summary(log, selected_sources(A))
        self.assertTrue(result["available"])
        self.assertEqual(result["invalid_records"], 0)
        self.assertEqual(result["measured"], 0)

    def test_retention_requires_valid_observations_and_event_provenance(self):
        log = self.root / "retention.jsonl"
        valid = bind_event({"session_id": A, "retention_total": 2,
                            "retention_retained": 1, "retention_lexical": 1})
        invalid = []
        for side in ("before", "after"):
            for key, value in (("context_state", "unknown"), ("context_tokens", True),
                               ("context_tokens", None), ("estimated_context_tokens", -1),
                               ("estimated_context_tokens", 2**64), ("lifetime_scope", "full"),
                               ("lifetime_cached_tokens", 1), ("unknown_field", "private")):
                row = json.loads(json.dumps(valid))
                row[f"{side}_observation"][key] = value
                invalid.append(row)
            for key in ("context_state", "lifetime_scope", "estimated_context_tokens"):
                row = json.loads(json.dumps(valid))
                del row[f"{side}_observation"][key]
                invalid.append(row)
            row = json.loads(json.dumps(valid))
            row[f"{side}_observation"].update(lifetime_scope="full",
                                             lifetime_input_tokens=1, lifetime_cached_tokens=2)
            invalid.append(row)
        for key, value in (("ts", None), ("items_covered", True), ("duration_ms", 2**64),
                           ("strategy", "private/field"), ("est_reclaimed_tokens", 100),
                           ("binary_sha256", "private"), ("config_sha256", []),
                           ("experiment_sha256", {}), ("decision_cohort", "future"),
                           ("decision_cohort", "control"), ("rollout_percent", 50)):
            invalid.append({**valid, key: value})
        for row in invalid:
            with self.subTest(row=row):
                log.write_text(json.dumps(row) + "\n")
                result = monitor.retention_summary(log, selected_sources(A))
                self.assertEqual(result["measured"], 0)
                self.assertFalse(result["available"])
                self.assertEqual(result["invalid_records"], 1)
        # Retention can use exact retained bytes even without provider context.
        # A real zero report also remains valid; neither is context savings.
        for state, count in (("unknown", None), ("reported", 0)):
            row = json.loads(json.dumps(valid))
            for side in ("before", "after"):
                row[f"{side}_observation"].update(context_state=state, context_tokens=count)
            log.write_text(json.dumps(row) + "\n")
            self.assertEqual(monitor.retention_summary(log, selected_sources(A))["measured"], 1)

    def test_retention_matches_selected_store_rejects_errors_and_deduplicates_pairs(self):
        log = self.root / "retention.jsonl"
        valid = bind_event({"session_id": A, "retention_total": 2,
                            "retention_retained": 1, "retention_lexical": 1})
        foreign = json.loads(json.dumps(valid))
        foreign["source_identity_sha256"] = "f" * 64
        for side in ("before", "after"):
            foreign[f"{side}_observation"]["source_identity_sha256"] = "f" * 64
        rows = [valid, valid, foreign, {**valid, "provider": "devin"},
                {**valid, "error_code": "unresolved_context"}]
        log.write_text("".join(json.dumps(row) + "\n" for row in rows))
        result = monitor.retention_summary(log, selected_sources(A))
        self.assertEqual(result["measured"], 1)
        self.assertEqual(result["checks"], 2)
        with log.open("a") as stream:
            stream.write(json.dumps({**valid, "retention_retained": 2}) + "\n")
            stream.write(json.dumps(valid) + "\n")  # conflict cannot be undone by replay
        result = monitor.retention_summary(log, selected_sources(A))
        self.assertEqual(result["measured"], 0)
        self.assertEqual(result["conflicting_pairs"], 1)

        conflicting_source = json.loads(json.dumps(valid))
        conflicting_source["after_observation"]["source_sha256"] = "f" * 64
        log.write_text("\n".join(json.dumps(row) for row in
                                [valid, conflicting_source, valid]) + "\n")
        result = monitor.retention_summary(log, selected_sources(A))
        self.assertEqual(result["measured"], 0)
        self.assertEqual(result["conflicting_pairs"], 1)

    def test_schema_invalid_counterparts_make_qualification_unavailable_in_any_order(self):
        log = self.root / "retention.jsonl"
        valid = bind_event({"session_id": A, "retention_total": 2,
                            "retention_retained": 1, "retention_lexical": 1})
        invalid = []
        required = ("schema", "provider", "session_id", "strategy", "action", "outcome",
                    "ts", "trigger_tokens", "context_tokens_before", "context_tokens_after",
                    "est_reclaimed_tokens", "items_covered", "duration_ms")
        for key in required:
            missing = dict(valid)
            del missing[key]
            invalid.append(missing)
            for value in (None, False, [], {}):
                invalid.append({**valid, key: value})
        for key in required[6:]:
            for value in (-1, 2**64, 1.0, "1"):
                invalid.append({**valid, key: value})
        for key, value in (("schema", "future"), ("provider", "future"),
                           ("action", "future"), ("outcome", "future"),
                           ("error_code", "private-response"), ("error_code", True),
                           ("session_id", "private/path"), ("session_id", "x" * 257),
                           ("strategy", "x" * 129), ("strategy", "private/path"),
                           ("source_identity_sha256", "invalid"),
                           ("binary_sha256", "invalid"), ("config_sha256", "invalid"),
                           ("experiment_sha256", "invalid"),
                           ("snapshot_before_sha256", "invalid"),
                           ("snapshot_after_sha256", "invalid"),
                           ("before_observation", False), ("after_observation", []),
                           ("decision_cohort", "future"), ("rollout_percent", True)):
            invalid.append({**valid, key: value})
        for row in invalid:
            for events in ([valid, row], [row, valid], [valid, row, valid]):
                with self.subTest(row=row, first_invalid=events[0] is row):
                    log.write_text("".join(json.dumps(event) + "\n" for event in events))
                    result = monitor.retention_summary(log, selected_sources(A))
                    self.assertFalse(result["available"])
                    self.assertEqual(result["error"], "event_log_invalid")
                    self.assertEqual(result["invalid_records"], 1)
                    self.assertEqual(result["oversized_records"], 0)
                    self.assertEqual([result[key] for key in ("measured", "checks", "literal", "lexical")],
                                     [0, 0, 0, 0])
                    self.assertEqual(result["lossy_sessions"], [])
                    self.assertNotIn("private-response", json.dumps(result))

    def test_malformed_or_oversized_history_cannot_keep_a_valid_counterpart(self):
        log = self.root / "retention.jsonl"
        valid = bind_event({"session_id": A, "retention_total": 2,
                            "retention_retained": 1, "retention_lexical": 1})
        raw = json.dumps(valid).encode()
        malformed = (b"{torn}", b"true", b"[]", b"null", b"\xff", b"\v",
                     json.dumps({**valid, "\ud800": 1}).encode(),
                     raw.replace(b'"items_covered": 0', b'"items_covered": -0'),
                     raw.replace(b'"items_covered": 0', b'"items_covered": 1e0'),
                     raw.replace(b'"items_covered": 0', b'"items_covered": 1e400'))
        for invalid in (*malformed, b"x" * (monitor.EVENT_RECORD_BYTES + 1)):
            oversized = int(len(invalid) > monitor.EVENT_RECORD_BYTES)
            for lines in ((raw, invalid), (invalid, raw), (raw, invalid, raw)):
                with self.subTest(invalid=invalid[:32], order=lines[0] is invalid):
                    log.write_bytes(b"\n".join(lines) + b"\n")
                    result = monitor.retention_summary(log, selected_sources(A))
                    self.assertFalse(result["available"])
                    self.assertEqual(result["error"], "event_log_invalid")
                    self.assertEqual(result["invalid_records"], 1 - oversized)
                    self.assertEqual(result["oversized_records"], oversized)
                    self.assertEqual(result["measured"], 0)
        # Invalid unrelated history cannot be assumed harmless without parsing it.
        invalid = {**valid, "session_id": UNRELATED, "binary_sha256": "invalid"}
        log.write_bytes(raw + b"\n" + json.dumps(invalid).encode() + b"\n")
        self.assertFalse(monitor.retention_summary(log, selected_sources(A))["available"])

    def test_valid_noncompaction_and_optional_legacy_events_are_unqualified_not_invalid(self):
        log = self.root / "retention.jsonl"
        valid = bind_event({"session_id": A, "retention_total": 2,
                            "retention_retained": 1, "retention_lexical": 1})
        rows = [{**valid, "action": "none"}]
        rows.extend({**valid, "outcome": outcome} for outcome in ("planned", "failed", "skipped", "blocked"))
        rows.extend({**valid, "error_code": code} for code in (
            "io", "provider_rejected", "apply_failed", "verification_failed", "custody_unavailable",
            "unattributed_provider_hook", "unresolved_context", "native_unqualified", "spawn_failed",
            "parent_thread", "provider_noop", "quota_limited"))
        for key in ("source_identity_sha256", "snapshot_before_sha256", "snapshot_after_sha256",
                    "before_observation", "after_observation"):
            rows.append({**valid, key: None})
            rows.append({name: value for name, value in valid.items() if name != key})
        rows.append({**valid, "retention_total": None, "retention_retained": None, "retention_lexical": None})
        rows.append({name: value for name, value in valid.items() if not name.startswith("retention_")})
        log.write_text("".join(json.dumps(row) + "\n" for row in rows))
        result = monitor.retention_summary(log, selected_sources(A))
        self.assertTrue(result["available"])
        self.assertIsNone(result["error"])
        self.assertEqual(result["invalid_records"], 0)
        self.assertEqual(result["measured"], 0)
        # Serde Option defaults, additive event fields, legal extended IDs and
        # full u64 retention denominators do not invent schema failures.
        for total in (0, 100_001, 2**64 - 1):
            row = {**valid, "session_id": "a." + "x" * 254, "strategy": "a:._-",
                   "retention_total": total, "retention_retained": total, "retention_lexical": total,
                   "unknown_additive_field": {"nested": True}, "binary_sha256": None,
                   "config_sha256": None, "experiment_sha256": None,
                   "decision_cohort": "ungated", "rollout_percent": None}
            del row["error_code"]
            for side in ("before", "after"):
                row[f"{side}_observation"] = {key: value for key, value in row[f"{side}_observation"].items()
                                               if key not in ("lifetime_input_tokens", "lifetime_cached_tokens")}
            log.write_text(json.dumps(row) + "\n")
            result = monitor.retention_summary(log, selected_sources(row["session_id"]))
            self.assertTrue(result["available"])
            self.assertEqual(result["measured"], 1)
            self.assertEqual(result["checks"], total)
        second = json.loads(json.dumps(row))
        second["snapshot_before_sha256"] = "f" * 64
        second["before_observation"]["snapshot_manifest_sha256"] = "f" * 64
        log.write_text(json.dumps(row) + "\n" + json.dumps(second) + "\n")
        result = monitor.retention_summary(log, selected_sources(row["session_id"]))
        self.assertTrue(result["available"])
        self.assertEqual(result["measured"], 2)
        self.assertEqual([result[key] for key in ("checks", "literal", "lexical")], [2**64 - 1] * 3)

    def test_retention_strict_duplicates_and_torn_tail_never_gain_credit(self):
        log = self.root / "retention.jsonl"
        valid = bind_event({"session_id": A, "retention_total": 2,
                            "retention_retained": 1, "retention_lexical": 1})
        raw = json.dumps(valid)
        duplicate = raw[:-1] + ', "retention_total": 2}'
        nested = raw.replace('"source_sha256": "' + "d" * 64 + '"',
                             '"source_sha256": "' + "d" * 64 + '", "source_sha256": "' + "e" * 64 + '"')
        log.write_text(duplicate + "\n" + nested + "\n" + raw + "\n")
        result = monitor.retention_summary(log, selected_sources(A))
        self.assertEqual(result["measured"], 0)
        self.assertFalse(result["available"])
        self.assertEqual(result["error"], "event_log_invalid")
        self.assertEqual(result["invalid_records"], 2)
        log.write_text(raw + "\n" + raw[:-1])
        result = monitor.retention_summary(log, selected_sources(A))
        self.assertEqual(result["measured"], 0)
        self.assertFalse(result["available"])
        self.assertEqual(result["error"], "event_log_incomplete")

    def test_retention_refuses_symlink_fifo_and_directory_without_blocking(self):
        target = self.root / "actual.jsonl"
        target.write_text("{}\n")
        link = self.root / "linked.jsonl"
        link.symlink_to(target)
        fifo = self.root / "events.fifo"
        os.mkfifo(fifo)
        for path in (link, fifo, self.root):
            started = time.monotonic()
            result = monitor.retention_summary(path, selected_sources(A))
            self.assertFalse(result["available"])
            self.assertEqual(result["measured"], 0)
            self.assertLess(time.monotonic() - started, 0.5)
        self.assertEqual(target.read_text(), "{}\n")

    def test_observe_retention_uses_same_xdg_root_and_exact_report_identity(self):
        log = self.root / "data" / "gobstopper" / "events.jsonl"
        log.parent.mkdir(parents=True)
        valid = bind_event({"session_id": A, "retention_total": 2,
                            "retention_retained": 1, "retention_lexical": 1})
        log.write_text(json.dumps(valid) + "\n")
        self.assertEqual(self.sample()["retention"]["measured"], 1)
        changed = report()
        changed["sessions"][0]["gobstopper"]["sourceIdentitySha256"] = "f" * 64
        self.report_file.write_text(json.dumps(changed))
        self.assertEqual(self.sample()["retention"]["measured"], 0)
        self.assertEqual(monitor.event_log_path({"HOME": "/synthetic/home"}),
                         Path("/synthetic/home/.local/share/gobstopper/events.jsonl"))
        self.assertEqual(monitor.event_log_path({"HOME": "/synthetic/home", "XDG_DATA_HOME": ""}),
                         Path("/synthetic/home/.local/share/gobstopper/events.jsonl"))

    def test_counter_reset_is_unknown_delta(self):
        self.sample()
        self.report_file.write_text(json.dumps(report(native=1)))
        row = self.sample()["sessions"][0]
        self.assertIsNone(row["native_hook_applied_delta"])
        self.assertTrue(row["native_counter_reset"])

    def test_ambiguous_plan_prefix_is_unavailable(self):
        value = report()
        other = json.loads(json.dumps(value["sessions"][0]))
        other["gobstopper"]["sessionIdNative"] = A[:-1] + "b"
        value["sessions"].append(other)
        self.report_file.write_text(json.dumps(value))
        self.assertIsNone(self.sample()["watch"]["plan_count"])

    def test_invalid_report_persists_only_error_class_and_unavailable_rows(self):
        self.report_file.write_text("SECRET_INVALID_REPORT")
        observation = self.sample()
        self.assertEqual(observation["report"]["error"], "invalid_report")
        self.assertFalse(observation["report"]["available"])
        self.assertTrue(all(not row["available"] for row in observation["sessions"]))
        self.assertNotIn("SECRET", (self.output / "latest.json").read_text())

    def test_watch_exit_zero_with_evaluation_failure_is_unavailable(self):
        with patch.dict(os.environ, {"STUB_WATCH_FAILURE": "1"}):
            observation = self.sample()
        self.assertEqual(observation["watch"]["exit_code"], 0)
        self.assertEqual(observation["watch"]["error"], "watch_evaluation_failed")
        self.assertFalse(observation["watch"]["available"])
        self.assertIsNone(observation["watch"]["plan_count"])
        self.assertNotIn("SECRET", (self.output / "latest.json").read_text())

    def test_sigterm_kills_and_reaps_owned_child_before_monitor_exits(self):
        pid_file = self.root / "child.pid"
        environment = dict(os.environ, STUB_SLEEP="30", STUB_PID=str(pid_file))
        child = subprocess.Popen([
            sys.executable, str(Path(monitor.__file__)), "--binary", str(self.binary),
            "--output-dir", str(self.output), "--session", A,
        ], env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 3
            while not pid_file.exists() and child.poll() is None and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(pid_file.exists(), "fixture child never started")
            owned_pid = int(pid_file.read_text())
            child.terminate()
            stdout, stderr = child.communicate(timeout=3)
            self.assertEqual(child.returncode, 1)
            self.assertEqual(json.loads(stdout)["error"], "interrupted")
            self.assertEqual(stderr, b"")
            with self.assertRaises(ProcessLookupError):
                os.kill(owned_pid, 0)
            self.assertFalse((self.output / "latest.json").exists())
            self.assertEqual(len(self.calls.read_text().splitlines()), 1)
        finally:
            if child.poll() is None:
                child.kill()
            child.communicate()

    def test_output_symlink_and_nonprivate_directory_are_refused(self):
        self.output.mkdir(mode=0o700)
        target = self.root / "preserve.txt"
        target.write_text("do not overwrite")
        (self.output / "latest.json").symlink_to(target)
        with self.assertRaises((monitor.MonitorError, OSError)):
            self.sample()
        self.assertEqual(target.read_text(), "do not overwrite")
        (self.output / "latest.json").unlink()
        self.output.chmod(0o755)
        with self.assertRaisesRegex(monitor.MonitorError, "unsafe_output_directory"):
            self.sample()
        self.assertEqual(stat.S_IMODE(self.output.stat().st_mode), 0o755)

    def test_existing_output_fifo_is_refused_before_read_or_child_dispatch(self):
        self.output.mkdir(mode=0o700)
        fifo = self.output / "latest.json"
        os.mkfifo(fifo, 0o600)
        before = fifo.stat()
        source = self.transcript.read_bytes()
        # A separate bounded process makes the old blocking open a test failure,
        # rather than hanging the runner when the FIFO has no writer.
        result = subprocess.run([
            sys.executable, str(Path(monitor.__file__)), "--binary", str(self.binary),
            "--output-dir", str(self.output), "--session", A,
        ], env=dict(os.environ), capture_output=True, timeout=3)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(json.loads(result.stdout),
                         {"status": "error", "error": "unsafe_output_file"})
        self.assertEqual(result.stderr, b"")
        self.assertFalse(self.calls.exists())
        self.assertFalse((self.output / "observations.jsonl").exists())
        self.assertEqual((fifo.stat().st_ino, fifo.stat().st_mode),
                         (before.st_ino, before.st_mode))
        self.assertEqual(self.transcript.read_bytes(), source)

    def test_single_owned_lock_prevents_overlapping_samples(self):
        directory = monitor.open_directory(self.output)
        lock = monitor.open_file(directory, ".monitor.lock", os.O_RDWR | os.O_CREAT)
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(monitor.MonitorError, "already_running"):
                self.sample()
            self.assertFalse(self.calls.exists())
        finally:
            os.close(lock)
            os.close(directory)

    def test_rotation_retains_at_most_two_bounded_logs(self):
        self.sample([A])
        log_limit = (self.output / "observations.jsonl").stat().st_size * 2 + 1024
        with patch.object(monitor, "LOG_BYTES", log_limit):
            for _ in range(5):
                self.sample([A])
        for name in ("observations.jsonl", "observations.1.jsonl"):
            self.assertTrue((self.output / name).exists())
            self.assertLessEqual((self.output / name).stat().st_size, log_limit)
        self.assertEqual(len(list(self.output.glob("observations*"))), 2)

    def test_child_timeout_and_capture_limit_are_bounded(self):
        environment = dict(os.environ, XDG_CONFIG_HOME=str(self.root), STUB_SLEEP="2")
        started = time.monotonic()
        result, _, _ = monitor.run_command([str(self.binary), "report", "--active-only", "--context-only"], environment, started + 0.05)
        self.assertEqual(result["error"], "timeout")
        self.assertLess(time.monotonic() - started, 1)
        environment.pop("STUB_SLEEP")
        with patch.object(monitor, "CAPTURE_BYTES", 32):
            # Isolate capture admission from the fresh report stub's filesystem
            # and interpreter startup. Timeout admission is tested above; this
            # child immediately exceeds the same 32-byte output bound.
            result, stdout, stderr = monitor.run_command(
                [sys.executable, "-c", "import os; os.write(1, b'x' * 1024)"],
                environment, time.monotonic() + 5)
        self.assertEqual(result["error"], "output_limit")
        self.assertLessEqual(len(stdout) + len(stderr), 32)

    def test_success_collects_silent_owned_descendants_before_reaping_leader(self):
        marker = self.root / "late-descendant"
        code = f"""import os,time,pathlib
pid=os.fork()
if pid == 0:
    os.close(1)
    os.close(2)
    time.sleep(0.5)
    pathlib.Path({str(marker)!r}).write_text("escaped")
    os._exit(0)
else:
    print("complete",flush=True)
"""
        signals = []
        real_killpg = os.killpg

        def observe_signal(pid, sig):
            observed = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            self.assertIsNotNone(observed, "leader must remain waitable until cleanup")
            signals.append(pid)
            return real_killpg(pid, sig)

        with patch.object(monitor.os, "killpg", side_effect=observe_signal):
            result, stdout, _ = monitor.run_command(
                [sys.executable, "-c", code], dict(os.environ), time.monotonic() + 3)
        self.assertEqual(result["exit_code"], 0)
        self.assertIsNone(result["error"])
        self.assertTrue(result["cleanup_complete"])
        self.assertEqual(stdout, b"complete\n")
        self.assertEqual(len(signals), 1)
        time.sleep(0.6)
        self.assertFalse(marker.exists())

    def test_success_drains_trailing_output_before_return(self):
        size = 512 * 1024
        result, stdout, stderr = monitor.run_command(
            [sys.executable, "-c", f"import os; os.write(1,b'x'*{size}); os.write(2,b'y'*20000)"],
            dict(os.environ), time.monotonic() + 3)
        self.assertIsNone(result["error"])
        self.assertEqual(stdout, b"x" * size)
        self.assertEqual(stderr, b"y" * 20000)
        self.assertTrue(result["cleanup_complete"])

    def test_escaped_pipe_holder_has_bounded_incomplete_cleanup(self):
        finished = self.root / "escaped-fixture-finished"
        code = f"""import os,time,pathlib
ready_read,ready_write=os.pipe()
pid=os.fork()
if pid == 0:
    os.close(ready_read)
    os.setsid()
    os.write(ready_write,b'1')
    os.close(ready_write)
    time.sleep(2)
    pathlib.Path({str(finished)!r}).write_text("finished")
    os._exit(0)
else:
    os.close(ready_write)
    os.read(ready_read,1)
    os.close(ready_read)
"""
        started = time.monotonic()
        result, _, _ = monitor.run_command(
            [sys.executable, "-c", code], dict(os.environ), started + 2)
        self.assertLess(time.monotonic() - started, 1.5)
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(result["error"], "cleanup_failed")
        self.assertFalse(result["cleanup_complete"])
        # This explicit escape fixture terminates itself; the monitor must not
        # signal a different group or claim complete custody of that process.
        deadline = time.monotonic() + 3
        while not finished.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue(finished.exists())


if __name__ == "__main__":
    unittest.main()

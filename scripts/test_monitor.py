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
                "modelContextWindow": 258400,
                "lifetimeInputTokens": 500000,
                "lifetimeCachedTokens": 400000,
                "lastActivityMs": 1790000000000,
                "compactions": {"applied": 10, "nativeHookApplied": native},
            },
        } for session in (A, UNRELATED)],
    }


class MonitorTests(unittest.TestCase):
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
    Path(os.environ["STUB_PID"]).write_text(str(os.getpid()))
if os.environ.get("STUB_SLEEP"):
    time.sleep(float(os.environ["STUB_SLEEP"]))
if sys.argv[1:] == ["report", "--active-only"]:
    sys.stdout.write(Path(os.environ["STUB_REPORT"]).read_text())
elif sys.argv[1:] == ["watch", "--dry-run", "--active-only", "--once"]:
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

    def test_timeout_child_is_reaped_and_unstarted_watch_has_no_resources(self):
        pid_file = self.root / "timeout.pid"
        with patch.dict(os.environ, {"STUB_SLEEP": "30", "STUB_PID": str(pid_file)}), \
                patch.object(monitor, "TIMEOUT_SECONDS", 0.5):
            observation = self.sample()
        self.assertEqual(observation["report"]["error"], "timeout")
        self.assert_resources(observation["report"]["resources"])
        self.assertEqual(observation["watch"]["error"], "timeout")
        self.assertIsNone(observation["watch"]["resources"])
        self.assertIsNone(observation["watch"]["exit_code"])
        self.assertEqual(len(self.calls.read_text().splitlines()), 1)
        with self.assertRaises(ProcessLookupError):
            os.kill(int(pid_file.read_text()), 0)

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
                [str(self.binary), "report", "--active-only"], environment, time.monotonic() + 0.5)
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
        first = self.sample()
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
        self.assertTrue(all(call["arguments"] in (["report", "--active-only"], ["watch", "--dry-run", "--active-only", "--once"]) for call in calls))

    def test_unavailable_fields_and_compaction_boundary_are_not_zero_savings(self):
        self.sample()
        value = report(context=0, native=None)
        self.report_file.write_text(json.dumps(value))
        sample = self.sample()["sessions"][0]
        self.assertIsNone(sample["context_tokens"])
        self.assertIsNone(sample["context_drop_tokens"])
        self.assertIsNone(sample["native_hook_applied_delta"])
        del value["sessions"][0]["gobstopper"]["compactions"]["nativeHookApplied"]
        self.report_file.write_text(json.dumps(value))
        self.assertIsNone(self.sample()["sessions"][0]["native_hook_applied"])

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
        with patch.object(monitor, "LOG_BYTES", 2048):
            for _ in range(5):
                self.sample([A])
        for name in ("observations.jsonl", "observations.1.jsonl"):
            self.assertTrue((self.output / name).exists())
            self.assertLessEqual((self.output / name).stat().st_size, 2048)
        self.assertEqual(len(list(self.output.glob("observations*"))), 2)

    def test_child_timeout_and_capture_limit_are_bounded(self):
        environment = dict(os.environ, XDG_CONFIG_HOME=str(self.root), STUB_SLEEP="2")
        started = time.monotonic()
        result, _, _ = monitor.run_command([str(self.binary), "report", "--active-only"], environment, started + 0.05)
        self.assertEqual(result["error"], "timeout")
        self.assertLess(time.monotonic() - started, 1)
        environment.pop("STUB_SLEEP")
        with patch.object(monitor, "CAPTURE_BYTES", 32):
            result, stdout, stderr = monitor.run_command([str(self.binary), "report", "--active-only"], environment, time.monotonic() + 1)
        self.assertEqual(result["error"], "output_limit")
        self.assertLessEqual(len(stdout) + len(stderr), 32)


if __name__ == "__main__":
    unittest.main()

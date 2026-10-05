import ast
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import socket
import stat
import sys
import tempfile
import textwrap
import time
import tokenize
import unittest
from unittest.mock import patch
import urllib.request

SPEC = importlib.util.spec_from_file_location("compare_proxy_tails", Path(__file__).with_name("compare_proxy_tails.py"))
COMPARE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COMPARE)
SECRET = "SECRET_TRANSCRIPT_SENTINEL"


def report(tail=0, dialect="anthropic"):
    extra = 20_000 if tail == 40 else 0
    return {
        "dialect": dialect, "requests": 3, "compacted": 1, "reused_prefix": 1,
        "over_threshold_after": 0, "raised_threshold": 0,
        "peak_est_tokens_in": 130_000, "peak_est_tokens_out": 90_000 + extra,
        "last_est_tokens_in": 130_000, "last_est_tokens_out": 80_000 + extra,
        "total_est_tokens_in": 360_000, "total_est_tokens_out": 220_000 + extra,
        "pairing_violations": 0, "source_pairing_violations": 0, "first_violation": None,
        "est_cache_read_tokens": 100_000, "est_cache_write_tokens": 120_000 + extra,
        "repeated_reads": 2, "repeated_reads_covered": 2 if tail == 40 else 0,
        "back_to_back_compactions": 0, "min_compaction_gap": None,
        "calibrate": True, "usage_requests": 0, "calibration_samples": 0,
        "last_ratio_permille": 1000, "reported_ratio_min_permille": None,
        "reported_ratio_median_permille": None, "reported_ratio_max_permille": None,
        "peak_reported_tokens_out": 0, "reported_over_threshold": 0,
        "compactions": [{"request": 1, "est_tokens_before": 120_000,
                         "est_tokens_after": 70_000 + extra, "messages_before": 5,
                         "messages_after": 3, "head_tokens": 20_000,
                         "summary_tokens": 10_000, "tail_tokens": 40_000 + extra,
                         "carry_chars": 100, "reported_tokens_before": None}],
        "private_payload": SECRET,
    }


class CompareProxyTailsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve(strict=True)
        self.counter = 0
        self.source = self.session("PRIVATE_SESSION_PATH_SENTINEL.jsonl")

    def tearDown(self):
        self.temp.cleanup()

    def session(self, name, dialect="anthropic"):
        path = self.root / name
        records = []
        for index in range(3):
            if dialect == "anthropic":
                for role in ("user", "assistant"):
                    records.append({"type": role, "sessionId": SECRET, "uuid": str(index),
                                    "message": {"role": role, "content": SECRET}})
            else:
                for role in ("user", "assistant"):
                    records.append({"type": "response_item", "payload": {"type": "message",
                                    "role": role, "content": [{"type": "input_text", "text": SECRET}]}})
        path.write_text("".join(json.dumps(record) + "\n" for record in records))
        return path

    def fake_cli(self, modes=None, reports=None, mutation_target=None):
        self.counter += 1
        folder = self.root / f"case-{self.counter}"
        folder.mkdir(mode=0o700)
        binary = folder / "fake-gobstopper"
        log = folder / "calls.jsonl"
        modes = modes or {}
        reports = reports or {str(tail): json.dumps(report(tail)) for tail in COMPARE.TAILS}
        body = textwrap.dedent('''
            import hashlib
            import json
            import os
            from pathlib import Path
            import signal
            import stat
            import subprocess
            import sys
            import time

            config = json.loads(CONFIG)
            args = sys.argv[1:]
            if args[:3] != ['--no-update', 'proxy', 'replay'] or args[-1] != '--json':
                sys.exit(91)
            tail = args[args.index('--keep-tail-percent') + 1]
            source = Path(args[3])
            assert source.is_absolute()
            assert source.parent == Path.cwd()
            assert os.environ['PATH'] == ''
            assert os.environ['GOBSTOPPER_MAX_TRANSCRIPT_BYTES'] == str(16 * 1024 * 1024)
            allowed = {'HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME',
                       'XDG_CACHE_HOME', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'TMPDIR',
                       'TMP', 'TEMP', 'PATH', 'LC_ALL', 'LANG', 'TZ', 'NO_COLOR',
                       'PYTHONNOUSERSITE', 'PYTHONSAFEPATH', 'PYTHONDONTWRITEBYTECODE',
                       'GOBSTOPPER_MAX_TRANSCRIPT_BYTES'}
            if sys.platform == 'darwin':
                allowed.add('__CF_USER_TEXT_ENCODING')
                assert os.environ.get('__CF_USER_TEXT_ENCODING') != config['secret']
            assert set(os.environ) <= allowed, sorted(set(os.environ) - allowed)
            homes = ['HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME',
                     'XDG_CACHE_HOME', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR']
            assert all(not list(Path(os.environ[name]).iterdir()) for name in homes)
            assert all(stat.S_IMODE(Path(os.environ[name]).stat().st_mode) == 0o700 for name in homes)
            index = Path.cwd().parent.name.split('-')[1]
            details = {'tail': int(tail), 'session_index': int(index), 'args': args,
                       'source': str(source), 'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
                       'source_mode': stat.S_IMODE(source.stat().st_mode),
                       'binary_mode': stat.S_IMODE(Path(sys.argv[0]).stat().st_mode),
                       'binary_sha256': hashlib.sha256(Path(sys.argv[0]).read_bytes()).hexdigest(),
                       'pid': os.getpid(), 'cwd': str(Path.cwd()),
                       'environment_keys': sorted(os.environ)}
            fd = os.open(config['log'], os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
            with os.fdopen(fd, 'w') as output:
                output.write(json.dumps(details) + '\\n')
            mode = config['modes'].get(index + ':' + tail, config['modes'].get(tail, 'valid'))
            marker = Path(config['log']).parent / ('term-' + index + '-' + tail)
            if mode in ('timeout', 'ignore_term'):
                if mode == 'ignore_term':
                    signal.signal(signal.SIGTERM, signal.SIG_IGN)
                else:
                    def terminated(_signal, _frame):
                        marker.write_text('graceful')
                        sys.exit(0)
                    signal.signal(signal.SIGTERM, terminated)
                time.sleep(60)
            elif mode in ('stdout_flood', 'stderr_flood'):
                fd = 1 if mode == 'stdout_flood' else 2
                while True:
                    os.write(fd, b'PRIVATE_FLOOD_SENTINEL' * 4096)
            elif mode == 'failure':
                os.write(2, (config['secret'] + config['mutation_target']).encode())
                sys.exit(7)
            elif mode in ('mutate_original', 'rewrite_original', 'replace_original'):
                target = Path(config['mutation_target'])
                raw = target.read_bytes()
                if mode == 'replace_original':
                    replacement = target.with_name('replacement.jsonl')
                    replacement.write_bytes(raw)
                    os.replace(replacement, target)
                else:
                    target.write_bytes(raw + b'\\n' if mode == 'mutate_original' else raw)
            elif mode in ('mutate_snapshot', 'mutate_binary', 'mutate_tool_origin'):
                target = (source if mode == 'mutate_snapshot' else Path(sys.argv[0])
                          if mode == 'mutate_binary' else Path(config['binary']))
                target.chmod(0o700)
                with target.open('ab') as changed:
                    changed.write(b'\\n')
            elif mode == 'descendant':
                ready = Path(config['log']).parent / ('ready-' + index + '-' + tail)
                code = ("import signal, sys, time; from pathlib import Path; "
                        "p=Path(sys.argv[1]); r=Path(sys.argv[2]); "
                        "signal.signal(signal.SIGTERM, lambda *_: (time.sleep(0.05), p.write_text('graceful'), sys.exit(0))); "
                        "r.write_text('ready'); time.sleep(60)")
                subprocess.Popen([sys.executable, '-c', code, str(marker), str(ready)])
                until = time.monotonic() + 2
                while not ready.exists() and time.monotonic() < until:
                    time.sleep(0.005)
                assert ready.exists()
            os.write(1, config['reports'][tail].encode())
        ''')
        config = {"log": str(log), "modes": modes, "reports": reports,
                  "mutation_target": str(mutation_target or self.source),
                  "binary": str(binary), "secret": SECRET}
        code = f"#!{sys.executable}\nCONFIG = {json.dumps(json.dumps(config))}\n" + body
        binary.write_text(code)
        binary.chmod(0o700)
        return binary, log, folder / "receipt.json"

    def invoke(self, binary, output, sessions=None, threshold="128000", timeout="3", extra=None):
        argv = ["--binary", str(binary), "--threshold", threshold, "--timeout", timeout, "--output", str(output)]
        for source in sessions if sessions is not None else [self.source]:
            argv.extend(("--session", str(source)))
        argv.extend(extra or [])
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = COMPARE.main(argv)
        self.assertEqual(stderr.getvalue(), "")
        lines = stdout.getvalue().splitlines()
        self.assertEqual(len(lines), 1)
        summary = json.loads(lines[0])
        self.assertIs(type(summary["ok"]), bool)
        self.assertIs(type(summary["receipt_written"]), bool)
        self.assertIs(type(summary["arms_with_reports"]), int)
        self.assertLess(len(lines[0]), 1024)
        self.assertNotIn(SECRET, lines[0])
        self.assertNotIn(str(self.root), lines[0])
        self.assertNotIn(self.source.name, lines[0])
        receipt = json.loads(output.read_text()) if summary["receipt_written"] else None
        if receipt is not None:
            raw = output.read_bytes()
            self.assertEqual(summary["receipt_sha256"], hashlib.sha256(raw).hexdigest())
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
            self.assertNotIn(SECRET, raw.decode())
            self.assertNotIn(str(self.root), raw.decode())
            self.assertNotIn(self.source.name, raw.decode())
        return code, summary, receipt

    def calls(self, log):
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    def assert_owned_cleanup(self, calls):
        for call in calls:
            with self.assertRaises(ProcessLookupError):
                os.kill(call["pid"], 0)
            self.assertFalse(Path(call["cwd"]).exists())
            self.assertFalse(Path(call["source"]).exists())

    def test_stable_comparison_preserves_sources_and_pins_only_tail_difference(self):
        binary, log, output = self.fake_cli()
        source_before = self.source.read_bytes()
        tool_before = binary.read_bytes()
        code, summary, receipt = self.invoke(binary, output)
        self.assertEqual(code, 0, summary)
        self.assertTrue(summary["ok"])
        self.assertEqual(summary["arms_with_reports"], 2)
        row = receipt["sources"][0]
        self.assertTrue(row["stable"])
        self.assertEqual(row["sha256_before"], row["sha256_after"])
        self.assertEqual(row["sha256_before"], hashlib.sha256(source_before).hexdigest())
        self.assertEqual(self.source.read_bytes(), source_before)
        self.assertEqual(binary.read_bytes(), tool_before)
        self.assertEqual(row["source_identity_sha256"], hashlib.sha256(os.fsencode(str(self.source))).hexdigest())
        self.assertEqual(receipt["tool"]["executed_sha256"], hashlib.sha256(tool_before).hexdigest())
        self.assertEqual(receipt["tool"]["sha256_before"], receipt["tool"]["sha256_after"])
        self.assertTrue(receipt["tool"]["stable"])
        deltas = row["comparison"]["metric_deltas"]
        self.assertEqual(row["comparison"]["direction"], "tail40_minus_tail0")
        self.assertEqual(deltas["total_est_tokens_out"], 20_000)
        self.assertEqual(deltas["repeated_reads_covered"], 2)
        self.assertEqual(deltas["requests"], 0)
        self.assertIsNone(deltas["peak_reported_tokens_out"])
        calls = self.calls(log)
        self.assertEqual([call["tail"] for call in calls], [0, 40])
        normalized = []
        for call in calls:
            args = call["args"][:]
            self.assertEqual(args[:3], ["--no-update", "proxy", "replay"])
            self.assertEqual(args[args.index("--threshold") + 1], "128000")
            self.assertEqual(args[-1], "--json")
            self.assertNotIn("--no-calibrate", args)
            for flag, value in COMPARE.SHARED_OPTIONS:
                self.assertEqual(args[args.index(flag) + 1], str(value))
            args[3] = "<snapshot>"
            args[args.index("--keep-tail-percent") + 1] = "<tail>"
            normalized.append(args)
            self.assertEqual(call["source_sha256"], row["sha256_before"])
            self.assertEqual(call["binary_sha256"], receipt["tool"]["executed_sha256"])
            self.assertEqual(call["source_mode"], 0o400)
            self.assertEqual(call["binary_mode"], 0o500)
        self.assertEqual(normalized[0], normalized[1])
        self.assert_owned_cleanup(calls)
        for name in ("quality", "billing", "cost", "continuation_effects"):
            self.assertEqual(receipt["unavailable"][name]["status"], "unavailable")
            self.assertIsNone(receipt["unavailable"][name]["value"])
        self.assertFalse(receipt["safety"]["source_files_written_by_runner"])
        self.assertFalse(receipt["safety"]["network_sandbox"])
        self.assertIn("not_quality", receipt["metric_basis"]["repeated_reads"])

    def test_codex_uses_builtin_dialect_and_reports_measured_usage_missingness(self):
        source = self.session("explicit-codex.jsonl", "openai-responses")
        reports = {str(tail): json.dumps(report(tail, "openai-responses")) for tail in COMPARE.TAILS}
        binary, log, output = self.fake_cli(reports=reports)
        code, _, receipt = self.invoke(binary, output, sessions=[source])
        self.assertEqual(code, 0)
        row = receipt["sources"][0]
        self.assertEqual(row["dialect"], "openai-responses")
        for arm in row["arms"].values():
            replay = arm["report"]
            self.assertEqual(replay["metrics"]["usage_requests"], 0)
            self.assertIsNone(replay["metrics"]["peak_reported_tokens_out"])
            self.assertEqual(replay["missing_metrics"]["peak_reported_tokens_out"], "unsupported_by_builtin_codex_replay")
            self.assertEqual(replay["recorded_usage_scope"]["status"], "unavailable")
            self.assertEqual(replay["recorded_usage_scope"]["requests_without_consumed_usage"], 3)
            self.assertIsNone(replay["recorded_input_tokens_at_compactions"])
            self.assertEqual(replay["compactions_without_recorded_usage"], 1)
        self.assert_owned_cleanup(self.calls(log))

    def test_recorded_usage_is_separated_from_outgoing_projections(self):
        reports = {}
        for tail in COMPARE.TAILS:
            value = report(tail)
            value.update({"usage_requests": 3, "calibration_samples": 3,
                          "reported_ratio_min_permille": 800, "reported_ratio_median_permille": 1000,
                          "reported_ratio_max_permille": 1200, "peak_reported_tokens_out": 108_000})
            value["compactions"][0]["reported_tokens_before"] = 120_000
            reports[str(tail)] = json.dumps(value)
        binary, _, output = self.fake_cli(reports=reports)
        code, _, receipt = self.invoke(binary, output)
        self.assertEqual(code, 0)
        arm = receipt["sources"][0]["arms"]["0"]["report"]
        self.assertEqual(arm["recorded_input_tokens_at_compactions"], 120_000)
        self.assertEqual(arm["compactions_with_recorded_usage"], 1)
        self.assertNotIn("peak_reported_tokens_out", arm["missing_metrics"])
        self.assertIn("not_measured_outgoing_usage", receipt["metric_basis"]["reported_usage_projections"]["basis"])
        self.assertIn("partial", receipt["metric_basis"]["recorded_input_tokens_at_compactions"])

    def test_tail0_result_is_retained_when_tail40_fails(self):
        binary, log, output = self.fake_cli(modes={"40": "failure"})
        code, summary, receipt = self.invoke(binary, output)
        self.assertEqual(code, 1)
        self.assertEqual(summary["error"], "command_failed")
        self.assertEqual(summary["arms_with_reports"], 1)
        row = receipt["sources"][0]
        self.assertEqual(row["arms"]["0"]["status"], "complete")
        self.assertIsNotNone(row["arms"]["0"]["report"])
        self.assertIsNone(row["arms"]["40"]["report"])
        self.assertIsNone(row["comparison"])
        self.assertTrue(row["stable"])
        self.assertGreater(row["arms"]["40"]["execution"]["stderr_bytes_observed"], 0)
        self.assert_owned_cleanup(self.calls(log))

    def test_timeout_gracefully_stops_owned_group_and_does_not_run_other_arm(self):
        binary, log, output = self.fake_cli(modes={"0": "timeout"})
        started = time.monotonic()
        code, summary, receipt = self.invoke(binary, output, timeout="1")
        self.assertLess(time.monotonic() - started, 4)
        self.assertEqual(code, 1)
        self.assertEqual(summary["error"], "timeout")
        execution = receipt["sources"][0]["arms"]["0"]["execution"]
        self.assertTrue(execution["cleanup_complete"])
        self.assertEqual(receipt["sources"][0]["arms"]["40"]["status"], "not_run")
        calls = self.calls(log)
        self.assertEqual(len(calls), 1)
        self.assertEqual((log.parent / "term-0-0").read_text(), "graceful")
        self.assert_owned_cleanup(calls)

    def test_timeout_escalates_only_the_owned_group_when_term_is_ignored(self):
        binary, log, output = self.fake_cli(modes={"0": "ignore_term"})
        code, summary, receipt = self.invoke(binary, output, timeout="1")
        self.assertEqual(code, 1)
        self.assertEqual(summary["error"], "timeout")
        execution = receipt["sources"][0]["arms"]["0"]["execution"]
        self.assertEqual(execution["exit_code"], -signal.SIGKILL)
        self.assertTrue(execution["cleanup_complete"])
        self.assert_owned_cleanup(self.calls(log))

    def test_descendants_receive_graceful_cleanup_even_after_successful_leader_exit(self):
        binary, log, output = self.fake_cli(modes={"0": "descendant", "40": "descendant"})
        code, _, receipt = self.invoke(binary, output)
        self.assertEqual(code, 0)
        for tail in COMPARE.TAILS:
            self.assertEqual((log.parent / f"term-0-{tail}").read_text(), "graceful")
            self.assertTrue(receipt["sources"][0]["arms"][str(tail)]["execution"]["cleanup_complete"])
        self.assert_owned_cleanup(self.calls(log))

    def test_stdout_and_stderr_floods_are_bounded_and_never_echoed(self):
        for mode in ("stdout_flood", "stderr_flood"):
            with self.subTest(mode=mode):
                binary, log, output = self.fake_cli(modes={"0": mode})
                started = time.monotonic()
                code, summary, receipt = self.invoke(binary, output)
                self.assertLess(time.monotonic() - started, 5)
                self.assertEqual(code, 1)
                self.assertEqual(summary["error"], "output_limit")
                execution = receipt["sources"][0]["arms"]["0"]["execution"]
                observed = execution["stdout_bytes_observed"] + execution["stderr_bytes_observed"]
                self.assertGreater(observed, COMPARE.MAX_CAPTURE_BYTES)
                self.assertLess(observed, 4 * COMPARE.MAX_CAPTURE_BYTES)
                self.assertTrue(execution["cleanup_complete"])
                self.assertNotIn("PRIVATE_FLOOD_SENTINEL", output.read_text())
                self.assert_owned_cleanup(self.calls(log))

    def test_malformed_duplicate_nonfinite_and_deep_replay_json_fail_closed(self):
        valid = json.dumps(report())
        for raw in (SECRET, "{", "[]", valid + valid,
                    valid[:-1] + ',"requests":4}',
                    valid[:-1] + ',"extra":NaN}', valid[:-1] + ',"extra":1e999}',
                    '[' * 2000, '{"bad":"\\ud800","bad":1}'):
            with self.subTest(raw=raw[:30]):
                binary, log, output = self.fake_cli(reports={"0": raw, "40": valid})
                code, summary, receipt = self.invoke(binary, output)
                self.assertEqual(code, 1)
                self.assertEqual(summary["error"], "invalid_replay_report" if raw == "[]" else "invalid_replay_json")
                self.assertEqual(len(self.calls(log)), 1)
                self.assertIsNone(receipt["sources"][0]["comparison"])
                self.assert_owned_cleanup(self.calls(log))

    def test_numeric_and_pairing_invariants_cannot_be_spoofed_by_fake_cli(self):
        mutations = (
            {"requests": True}, {"requests": -1}, {"requests": 1.0}, {"requests": 2**64},
            {"requests": 0}, {"repeated_reads_covered": 3}, {"calibrate": False},
            {"pairing_violations": 1}, {"min_compaction_gap": 1}, {"calibration_samples": 1},
            {"est_cache_write_tokens": 0}, {"usage_requests": 1}, {"compactions": []},
            {"compacted": "3"}, {"last_ratio_permille": 999}, {"first_violation": 3},
        )
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                value = report()
                value.update(mutation)
                binary, _, output = self.fake_cli(reports={"0": json.dumps(value), "40": json.dumps(report(40))})
                code, summary, receipt = self.invoke(binary, output)
                self.assertEqual(code, 1)
                self.assertIn(summary["error"], ("invalid_replay_report", "no_replay_requests"))
                self.assertIsNone(receipt["sources"][0]["comparison"])
        value = report()
        del value["total_est_tokens_out"]
        with self.assertRaisesRegex(COMPARE.ComparisonError, "invalid_replay_report"):
            COMPARE.replay_report(json.dumps(value).encode(), "anthropic")
        with self.assertRaisesRegex(COMPARE.ComparisonError, "invalid_replay_report"):
            COMPARE.replay_report(json.dumps(report()).replace('"requests": 3', '"requests": -0').encode(), "anthropic")

    def test_mismatched_arms_keep_both_reports_but_no_comparison(self):
        second = report(40)
        second["total_est_tokens_in"] += 1
        binary, _, output = self.fake_cli(reports={"0": json.dumps(report()), "40": json.dumps(second)})
        code, summary, receipt = self.invoke(binary, output)
        self.assertEqual(code, 1)
        self.assertEqual(summary["error"], "replay_arms_not_comparable")
        self.assertEqual(summary["arms_with_reports"], 2)
        self.assertIsNone(receipt["sources"][0]["comparison"])
        self.assertTrue(all(arm["report"] is not None for arm in receipt["sources"][0]["arms"].values()))

    def test_original_mutation_same_byte_rewrite_and_replacement_invalidate_comparison(self):
        for mode in ("mutate_original", "rewrite_original", "replace_original"):
            with self.subTest(mode=mode):
                source = self.session(f"source-{mode}.jsonl")
                before = source.read_bytes()
                binary, log, output = self.fake_cli(modes={"0": mode}, mutation_target=source)
                code, summary, receipt = self.invoke(binary, output, sessions=[source])
                self.assertEqual(code, 1)
                self.assertEqual(summary["error"], "source_changed")
                row = receipt["sources"][0]
                self.assertFalse(row["stable"])
                self.assertEqual(row["sha256_before"], hashlib.sha256(before).hexdigest())
                self.assertEqual(row["sha256_after"], hashlib.sha256(source.read_bytes()).hexdigest())
                self.assertIsNone(row["comparison"])
                self.assertEqual(row["arms"]["0"]["status"], "invalidated")
                self.assertIsNotNone(row["arms"]["0"]["report"])
                self.assertEqual(row["arms"]["40"]["status"], "not_run")
                self.assertEqual(len(self.calls(log)), 1)
                self.assert_owned_cleanup(self.calls(log))

    def test_snapshot_or_executable_copy_mutation_never_reaches_other_arm(self):
        for mode in ("mutate_snapshot", "mutate_binary", "mutate_tool_origin"):
            with self.subTest(mode=mode):
                before = self.source.read_bytes()
                binary, log, output = self.fake_cli(modes={"0": mode})
                code, summary, receipt = self.invoke(binary, output)
                self.assertEqual(code, 1)
                self.assertEqual(summary["error"], "binary_changed" if mode == "mutate_tool_origin" else "snapshot_changed")
                self.assertEqual(self.source.read_bytes(), before)
                self.assertTrue(receipt["sources"][0]["stable"])
                self.assertIsNone(receipt["sources"][0]["comparison"])
                self.assertEqual(len(self.calls(log)), 1)
                self.assert_owned_cleanup(self.calls(log))

    def test_change_to_an_earlier_source_invalidates_earlier_comparison(self):
        second = self.session("second-explicit.jsonl")
        binary, log, output = self.fake_cli(modes={"1:0": "mutate_original"}, mutation_target=self.source)
        code, summary, receipt = self.invoke(binary, output, sessions=[self.source, second])
        self.assertEqual(code, 1)
        self.assertEqual(summary["error"], "source_changed")
        self.assertEqual(summary["arms_with_reports"], 3)
        self.assertFalse(receipt["sources"][0]["stable"])
        self.assertTrue(receipt["sources"][1]["stable"])
        self.assertTrue(all(row["comparison"] is None for row in receipt["sources"]))
        self.assertIsNotNone(receipt["sources"][0]["arms"]["0"]["report"])
        self.assertEqual(len(self.calls(log)), 3)
        self.assert_owned_cleanup(self.calls(log))

    def test_quiescence_check_rejects_change_before_any_subprocess(self):
        binary, log, output = self.fake_cli()
        sleep = time.sleep

        def change(seconds):
            if seconds == COMPARE.STABILITY_SECONDS:
                with self.source.open("ab") as source:
                    source.write(b"\n")
            else:
                sleep(seconds)

        with patch.object(COMPARE.time, "sleep", side_effect=change), \
                patch.object(COMPARE.subprocess, "Popen", side_effect=AssertionError("unexpected process")) as spawn:
            code, summary, receipt = self.invoke(binary, output)
        self.assertEqual(code, 1)
        self.assertEqual(summary["error"], "source_changed")
        spawn.assert_not_called()
        self.assertFalse(receipt["sources"][0]["stable"])
        self.assertEqual(self.calls(log), [])

    def test_new_destination_refuses_existing_files_directories_and_symlinks(self):
        for kind in ("file", "directory", "symlink", "dangling_symlink"):
            with self.subTest(kind=kind):
                binary, log, output = self.fake_cli()
                if kind == "file":
                    output.write_text(SECRET)
                elif kind == "directory":
                    output.mkdir()
                else:
                    output.symlink_to(self.source if kind == "symlink" else self.root / "absent")
                before = self.source.read_bytes()
                with patch.object(COMPARE.subprocess, "Popen", side_effect=AssertionError("unexpected process")) as spawn:
                    code, summary, receipt = self.invoke(binary, output)
                self.assertEqual(code, 2)
                self.assertEqual(summary["error"], "receipt_exists")
                self.assertIsNone(receipt)
                self.assertEqual(self.source.read_bytes(), before)
                if kind == "file":
                    self.assertEqual(output.read_text(), SECRET)
                spawn.assert_not_called()
                self.assertEqual(self.calls(log), [])

    def test_receipt_cannot_alias_even_a_missing_input(self):
        binary, _, _ = self.fake_cli()
        missing = self.root / "missing-input.jsonl"
        code, summary, _ = self.invoke(binary, missing, sessions=[missing])
        self.assertEqual(code, 2)
        self.assertEqual(summary["error"], "receipt_aliases_input")
        self.assertFalse(missing.exists())

    def test_symlinked_ancestors_and_writable_output_directories_are_refused(self):
        binary, log, _ = self.fake_cli()
        linked = self.root / "redirected"
        linked.symlink_to(log.parent, target_is_directory=True)
        for output in (linked / "receipt.json", self.root / "group-writable" / "receipt.json"):
            with self.subTest(output=output.name):
                if output.parent.name == "group-writable":
                    output.parent.mkdir(mode=0o700)
                    output.parent.chmod(0o770)
                with patch.object(COMPARE.subprocess, "Popen", side_effect=AssertionError("unexpected process")) as spawn:
                    code, summary, receipt = self.invoke(binary, output)
                self.assertEqual(code, 2)
                self.assertIn(summary["error"], ("receipt_unavailable", "unsafe_receipt_directory"))
                self.assertIsNone(receipt)
                spawn.assert_not_called()

    def test_symlink_inputs_special_files_oversize_and_invalid_json_are_refused(self):
        for kind in ("symlink", "ancestor_symlink", "fifo", "oversize", "empty", "invalid_json", "unknown", "long_record", "too_many_records"):
            with self.subTest(kind=kind):
                source = self.root / f"unsafe-{kind}.jsonl"
                if kind == "symlink":
                    source.symlink_to(self.source)
                elif kind == "ancestor_symlink":
                    parent = self.root / "source-link"
                    parent.symlink_to(self.root, target_is_directory=True)
                    source = parent / self.source.name
                elif kind == "fifo":
                    os.mkfifo(source)
                elif kind == "oversize":
                    with source.open("wb") as stream:
                        stream.truncate(COMPARE.MAX_SOURCE_BYTES + 1)
                elif kind == "empty":
                    source.write_bytes(b"")
                elif kind == "invalid_json":
                    source.write_text('{"sessionId":"known"}\nnot json\n')
                elif kind == "unknown":
                    source.write_text('{"type":"unsupported"}\n')
                elif kind == "long_record":
                    source.write_bytes(b"x" * (COMPARE.MAX_RECORD_BYTES + 1))
                else:
                    source.write_bytes(b"{}\n" * (COMPARE.MAX_RECORDS + 1))
                binary, log, output = self.fake_cli()
                with patch.object(COMPARE.subprocess, "Popen", side_effect=AssertionError("unexpected process")) as spawn:
                    code, summary, receipt = self.invoke(binary, output, sessions=[source])
                self.assertEqual(code, 1)
                self.assertFalse(summary["ok"])
                self.assertTrue(summary["receipt_written"])
                self.assertIsNone(receipt["sources"][0]["comparison"])
                spawn.assert_not_called()
                self.assertEqual(self.calls(log), [])

    def test_untrusted_binary_shape_is_refused_without_execution(self):
        for kind in ("symlink", "not_executable", "oversize"):
            with self.subTest(kind=kind):
                original, log, output = self.fake_cli()
                binary = original
                if kind == "symlink":
                    binary = log.parent / "binary-link"
                    binary.symlink_to(original)
                elif kind == "not_executable":
                    binary.chmod(0o600)
                else:
                    with binary.open("wb") as stream:
                        stream.truncate(COMPARE.MAX_BINARY_BYTES + 1)
                with patch.object(COMPARE.subprocess, "Popen", side_effect=AssertionError("unexpected process")) as spawn:
                    code, summary, _ = self.invoke(binary, output)
                self.assertEqual(code, 1)
                self.assertIn(summary["error"], ("binary_unsafe", "binary_not_executable", "binary_size_limit"))
                spawn.assert_not_called()

    def test_explicit_source_count_numeric_and_path_bounds_precede_execution(self):
        binary, log, output = self.fake_cli()
        variants = (
            {"sessions": []}, {"sessions": [self.source] * 9}, {"sessions": [self.source, self.source]},
            {"sessions": ["session-id-prefix"]}, {"binary": "gobstopper"},
            {"threshold": "0"}, {"threshold": "1000001"}, {"threshold": "nope"},
            {"timeout": "0"}, {"timeout": "121"}, {"timeout": "nan"}, {"timeout": "inf"},
            {"sessions": [str(self.root) + "/../escape.jsonl"]},
            {"sessions": [str(self.source) + "\n"]}, {"extra": ["--allow-provider-calls"]},
            {"extra": ["--tail", "60"]},
        )
        for variant in variants:
            with self.subTest(variant=list(variant)):
                selected = {"binary": binary, "output": output}
                selected.update(variant)
                with patch.object(COMPARE.subprocess, "Popen", side_effect=AssertionError("unexpected process")) as spawn:
                    code, summary, receipt = self.invoke(**selected)
                self.assertEqual(code, 2)
                self.assertFalse(summary["receipt_written"])
                self.assertIsNone(receipt)
                self.assertFalse(output.exists())
                spawn.assert_not_called()
        self.assertEqual(self.calls(log), [])

    def test_multiple_explicit_sources_have_separate_stable_comparisons(self):
        sources = [self.source, self.session("other-explicit.jsonl")]
        binary, log, output = self.fake_cli()
        code, summary, receipt = self.invoke(binary, output, sessions=sources)
        self.assertEqual(code, 0)
        self.assertEqual(summary["sessions"], 2)
        self.assertEqual(summary["arms_with_reports"], 4)
        self.assertEqual(len({row["source_identity_sha256"] for row in receipt["sources"]}), 2)
        self.assertTrue(all(row["stable"] and row["comparison"] for row in receipt["sources"]))
        self.assertEqual([(call["session_index"], call["tail"]) for call in self.calls(log)], [(0, 0), (0, 40), (1, 0), (1, 40)])
        self.assert_owned_cleanup(self.calls(log))

    def test_no_network_provider_shell_or_inherited_trust_paths(self):
        binary, log, output = self.fake_cli()
        poison = {"HOME": str(self.root), "XDG_CONFIG_HOME": str(self.root),
                  "CODEX_HOME": str(self.root), "CLAUDE_CONFIG_DIR": str(self.root),
                  "GOBSTOPPER_SCORER": "llm", "GOBSTOPPER_DIGEST": "jev",
                  "GOBSTOPPER_LLM_API_KEY": SECRET, "ANTHROPIC_API_KEY": SECRET,
                  "OPENAI_API_KEY": SECRET, "AI_GATEWAY_API_KEY": SECRET,
                  "TYPESAFE_API_KEY": SECRET, "GOBSTOPPER_MAX_TRANSCRIPT_BYTES": str(2**63),
                  "PYTHONPATH": str(self.root), "DYLD_INSERT_LIBRARIES": SECRET,
                  "LD_PRELOAD": SECRET, "HTTPS_PROXY": SECRET, "GOBSTOPPER_PLUGIN": SECRET,
                  "__CF_USER_TEXT_ENCODING": SECRET}
        actual_spawn = COMPARE.subprocess.Popen

        def spawn(argv, **kwargs):
            self.assertIs(kwargs["shell"], False)
            self.assertTrue(kwargs["start_new_session"])
            self.assertTrue(kwargs["close_fds"])
            self.assertEqual(argv[1:4], ["--no-update", "proxy", "replay"])
            self.assertEqual(kwargs["env"]["PATH"], "")
            self.assertEqual(set(kwargs["env"]) & {"ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GOBSTOPPER_PLUGIN", "GOBSTOPPER_SCORER"}, set())
            return actual_spawn(argv, **kwargs)

        with patch.dict(os.environ, poison), \
                patch.object(COMPARE.subprocess, "Popen", side_effect=spawn) as launched, \
                patch.object(socket, "socket", side_effect=AssertionError("network forbidden")) as network, \
                patch.object(urllib.request, "urlopen", side_effect=AssertionError("network forbidden")) as http:
            code, _, _ = self.invoke(binary, output)
        self.assertEqual(code, 0)
        self.assertEqual(launched.call_count, 2)
        network.assert_not_called()
        http.assert_not_called()
        self.assert_owned_cleanup(self.calls(log))

    def test_interrupt_unwinds_owned_process_cleanup_and_restores_handlers(self):
        binary, log, output = self.fake_cli(modes={"0": "timeout"})
        original = {name: signal.getsignal(name) for name in (signal.SIGINT, signal.SIGTERM)}
        waitid = os.waitid
        first = True

        def cancelled(*args):
            nonlocal first
            if first:
                first = False
                raise COMPARE.ComparisonInterrupted()
            return waitid(*args)

        with patch.object(COMPARE.os, "waitid", side_effect=cancelled):
            code, summary, receipt = self.invoke(binary, output)
        self.assertEqual(code, 130)
        self.assertEqual(summary["error"], "interrupted")
        execution = receipt["sources"][0]["arms"]["0"]["execution"]
        self.assertTrue(execution["cleanup_complete"])
        self.assertEqual(execution["error"], "interrupted")
        for name, handler in original.items():
            self.assertEqual(signal.getsignal(name), handler)
        self.assert_owned_cleanup(self.calls(log))

    def test_private_copy_hash_must_match_original_before_any_execution(self):
        for selected_kind in ("binary", "source"):
            with self.subTest(kind=selected_kind):
                binary, log, output = self.fake_cli()
                actual_copy = COMPARE.copy_private
                changed = False
                source_before = self.source.read_bytes()

                def corrupted(source, destination, limit, kind, deadline, **kwargs):
                    nonlocal changed
                    result = actual_copy(source, destination, limit, kind, deadline, **kwargs)
                    if kind == selected_kind and not changed:
                        changed = True
                        destination.chmod(0o700)
                        with destination.open("ab") as stream:
                            stream.write(b"\n")
                        destination.chmod(0o500 if kind == "binary" else 0o400)
                    return result

                with patch.object(COMPARE, "copy_private", side_effect=corrupted), \
                        patch.object(COMPARE.subprocess, "Popen", side_effect=AssertionError("unexpected process")) as spawn:
                    code, summary, _ = self.invoke(binary, output)
                self.assertEqual(code, 1)
                self.assertEqual(summary["error"], "snapshot_changed")
                self.assertEqual(self.source.read_bytes(), source_before)
                spawn.assert_not_called()
                self.assertEqual(self.calls(log), [])

    def test_receipt_leaf_or_parent_replacement_never_overwrites_new_occupant(self):
        for kind in ("leaf", "parent"):
            with self.subTest(kind=kind):
                binary, log, output = self.fake_cli()

                def replaced(_options, receipt):
                    if kind == "leaf":
                        output.unlink()
                    else:
                        output.parent.rename(output.parent.with_name(output.parent.name + "-held"))
                        output.parent.mkdir(mode=0o700)
                    output.write_text(SECRET)
                    receipt["status"] = "complete"

                with patch.object(COMPARE, "experiment", side_effect=replaced), \
                        patch.object(COMPARE.subprocess, "Popen", side_effect=AssertionError("unexpected process")) as spawn:
                    code, summary, receipt = self.invoke(binary, output)
                self.assertEqual(code, 1)
                self.assertEqual(summary["status"], "failed")
                self.assertEqual(summary["error"], "receipt_write_failed")
                self.assertFalse(summary["receipt_written"])
                self.assertIsNone(receipt)
                self.assertEqual(output.read_text(), SECRET)
                spawn.assert_not_called()
                self.assertEqual(self.calls(log), [])

    def test_receipt_and_scratch_remain_private_under_permissive_umask(self):
        binary, log, output = self.fake_cli()
        previous = os.umask(0)
        try:
            code, _, receipt = self.invoke(binary, output)
        finally:
            os.umask(previous)
        self.assertEqual(code, 0)
        self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
        self.assertTrue(receipt["sources"][0]["stable"])
        self.assert_owned_cleanup(self.calls(log))

    def test_new_scripts_have_no_comments_and_use_only_stdlib_imports(self):
        for name in ("compare_proxy_tails.py", "test_compare_proxy_tails.py"):
            path = Path(__file__).with_name(name)
            raw = path.read_text()
            self.assertFalse(any(token.type == tokenize.COMMENT for token in tokenize.generate_tokens(io.StringIO(raw).readline)))
            tree = ast.parse(raw)
            imports = [node.name.split(".")[0] for item in ast.walk(tree)
                       if isinstance(item, ast.Import) for node in item.names]
            imports += [item.module.split(".")[0] for item in ast.walk(tree) if isinstance(item, ast.ImportFrom)]
            self.assertTrue(set(imports) <= sys.stdlib_module_names)
        tree = ast.parse(Path(COMPARE.__file__).read_text())
        forbidden = {"socket", "urllib", "requests", "http", "webbrowser"}
        self.assertFalse(any(isinstance(node, ast.Import) and any(item.name.split(".")[0] in forbidden for item in node.names)
                             for node in ast.walk(tree)))


if __name__ == "__main__":
    unittest.main()

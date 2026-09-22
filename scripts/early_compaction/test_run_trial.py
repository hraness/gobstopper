import collections
import json
from pathlib import Path
import tempfile
import types
import time
import unittest
from unittest import mock

import run_trial as driver
import synthetic_trial as trial


class FakeTransport:
    """Simulates only the owner JSONL contract; never starts a process."""
    def __init__(self, owner, fixture, arm="baseline", bad_final=False):
        self.owner, self.fixture, self.arm, self.bad_final = owner, fixture, arm, bad_final
        owner.mkdir()
        self.turn, self.snapshot_count, self.sequence = 0, 0, 0
        self.thread = "synthetic-owned-source"
        self.rows, self.totals, self.sent = [], {}, []
        self.events = collections.deque([{"type": "ready", "thread_id": self.thread, "activity_reporting": True}])

    def record(self, kind, data):
        self.sequence += 1
        driver.save(self.owner / f"receipt-{self.sequence:06}.json",
                    {"sequence": self.sequence, "timestamp_ms": self.sequence, "kind": kind, "data": data})

    def usage(self, label):
        usage = {"input_tokens": 100, "cached_input_tokens": 60, "output_tokens": 20,
                 "reasoning_output_tokens": 5}
        total = self.totals.setdefault(self.thread, {k: 0 for k in usage})
        for key in total:
            total[key] += usage[key]
        response_id = f"{self.thread}-{label}"
        self.rows.append({"type": "token_usage_record", "payload": {"thread_id": self.thread,
            "response_id": response_id, "usage": usage, "thread_token_usage": dict(total)}})
        self.snapshot_count += 1
        path = self.owner / f"snapshot-{self.snapshot_count:03}.jsonl"
        path.write_text("".join(json.dumps(row) + "\n" for row in self.rows))
        data = {"response_id_sha256": driver.sha(response_id), "thread_id": self.thread, "usage": usage}
        self.record("response_usage", data)
        self.events.append({"type": "response_usage", "data": data})

    def send(self, command):
        self.sent.append(command)
        if command["type"] == "inject":
            # Only this turn's record is sent, not the fixture or future records.
            expected = self.fixture["turns"][self.turn]["injected_items"]
            assert command["items"] == expected
            self.events.append({"type": "injected", "thread_id": self.thread})
        elif command["type"] == "prepare":
            assert self.turn == 3
            if self.arm != "baseline":
                self.record("policy", {"reason": "eligible", "completed_turns": 3, "context_tokens": 17000})
                self.record("native_compaction_intent" if self.arm == "native_early" else "custom_compaction_intent", {})
                if self.arm == "native_early":
                    self.usage("native")
                    self.record("native_compaction_completed", {})
                else:
                    self.thread = "synthetic-owned-continuation"
                    self.events.append({"type": "continuation", "thread_id": self.thread})
                    self.record("custom_injection_accepted", {})
            self.events.append({"type": "boundary_complete"})
        elif command["type"] == "turn":
            self.turn += 1
            self.usage(str(self.turn))
            if self.arm == "custom_early" and self.turn == 4:
                self.record("adoption_verified", {})
            self.events.append({"type": "item_activity", "data": {"item_type": "agentMessage"}})
            text = "Acknowledged synthetic facts."
            if self.turn == 6:
                artifact = trial.expected_artifact(self.fixture)
                if self.bad_final:
                    artifact["total_cost_cents"] += 1
                text = json.dumps(artifact)
            self.events.extend([{"type": "assistant", "text": text}, {"type": "turn_completed"}])

    def receive(self):
        return self.events.popleft()


class DriverTests(unittest.TestCase):
    def test_child_environment_drops_keys_provider_and_parent_thread(self):
        original = {"HOME": "/real/home", "PATH": "/bin", "OPENAI_API_KEY": "secret",
                    "CODEX_THREAD_ID": "parent", "OPENAI_BASE_URL": "override", "CODEX_HOME": "/old",
                    "GOBSTOPPER_CODEX_BIN": "/unexpected", "AWS_ACCESS_KEY_ID": "private",
                    "CODEX_SANDBOX": "seatbelt", "CODEX_SANDBOX_NETWORK_DISABLED": "1",
                    "APP_SANDBOX_CONTAINER_ID": "existing-boundary"}
        self.assertEqual(driver.child_environment(original, Path("/isolated")),
                         {"HOME": "/real/home", "PATH": "/bin", "CODEX_HOME": "/isolated",
                          "CODEX_SANDBOX": "seatbelt", "CODEX_SANDBOX_NETWORK_DISABLED": "1",
                          "APP_SANDBOX_CONTAINER_ID": "existing-boundary"})

    def test_shared_workspace_requires_private_empty_directory(self):
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            workspace = base / "workspace"
            workspace.mkdir(mode=0o700)
            self.assertEqual(driver.validate_workspace(workspace), workspace.resolve())
            (workspace / "unexpected").write_text("synthetic")
            with self.assertRaises(driver.TrialFailure):
                driver.validate_workspace(workspace)
            (workspace / "unexpected").unlink()
            workspace.chmod(0o755)
            with self.assertRaises(driver.TrialFailure):
                driver.validate_workspace(workspace)
            link = base / "link"
            link.symlink_to(workspace)
            with self.assertRaises(driver.TrialFailure):
                driver.validate_workspace(link)

    def test_shared_workspace_is_same_in_all_owner_arguments(self):
        args = types.SimpleNamespace(gobstopper=Path("/gob"), codex_home=Path("/isolated"), codex_bin=Path("/codex"),
                                     workspace=Path("/shared/workspace"), model="test", effort="test", arm="baseline")
        for arm in trial.ARMS:
            args.arm = arm
            argv = driver.owner_argv(args, Path("/run") / arm)
            self.assertEqual(argv[argv.index("--cwd") + 1], "/shared/workspace")

    def test_stalled_writer_respects_deadline(self):
        class Stalled:
            def write(self, value):
                time.sleep(.2)
            def flush(self):
                pass
        transport = object.__new__(driver.ProcessTransport)
        transport.deadline = time.monotonic() + .02
        transport.process = types.SimpleNamespace(stdin=Stalled())
        started = time.monotonic()
        with self.assertRaisesRegex(driver.TrialFailure, "write deadline"):
            transport.send({"type": "turn", "text": "synthetic"})
        self.assertLess(time.monotonic() - started, .1)

    def exercise(self, arm="baseline", bad_final=False):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        fixture = trial.make_fixture(ballast_rows=8)
        owner = Path(temp.name) / "owner"
        receipt = trial.receipt_template(fixture, arm, "fixture-model", "fixture-effort")
        evidence = driver.Evidence(receipt)
        transport = FakeTransport(owner, fixture, arm, bad_final)
        return fixture, owner, receipt, evidence, transport

    def test_all_arms_round_trip_and_exact_boundary(self):
        for arm in trial.ARMS:
            fixture, owner, receipt, evidence, transport = self.exercise(arm)
            artifact = driver.drive(transport, evidence, fixture, owner)
            self.assertEqual(artifact, trial.expected_artifact(fixture))
            self.assertTrue(evidence.usage_reconciled())
            self.assertEqual(receipt["completed_task_turns"], 6)
            boundary_index = next(i for i, c in enumerate(transport.sent) if c["type"] == "prepare")
            self.assertEqual(sum(c["type"] == "turn" for c in transport.sent[:boundary_index]), 3)
            self.assertEqual(transport.sent[boundary_index + 1]["items"], fixture["turns"][3]["injected_items"])
            self.assertEqual(len(receipt["responses"]), 7 if arm == "native_early" else 6)
            driver.map_intervention(receipt, driver.owner_receipts(owner), require_adoption=True)

    def test_custom_preserves_source_and_continues_new_thread(self):
        fixture, owner, receipt, evidence, transport = self.exercise("custom_early")
        driver.drive(transport, evidence, fixture, owner)
        self.assertNotEqual(receipt["source_thread_sha256"], receipt["continuation_thread_sha256"])
        self.assertEqual(len(evidence.thread_ids), 2)

    def test_native_overhead_counted_and_classified(self):
        fixture, owner, receipt, evidence, transport = self.exercise("native_early")
        driver.drive(transport, evidence, fixture, owner)
        rows = [r for r in receipt["responses"] if r["phase"] == "compaction"]
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["usage"]["input_tokens"], 100)

    def test_failed_artifact_retains_usage_and_failed_checks(self):
        fixture, owner, receipt, evidence, transport = self.exercise(bad_final=True)
        with self.assertRaisesRegex(driver.TrialFailure, "deterministic verification"):
            driver.drive(transport, evidence, fixture, owner)
        self.assertEqual(len(receipt["responses"]), 6)
        self.assertFalse(receipt["artifact_verification"]["passed"])
        self.assertTrue(evidence.last_assistant)

    def test_old_owner_or_external_activity_rejected(self):
        _, _, _, evidence, _ = self.exercise()
        with self.assertRaises(driver.TrialFailure):
            evidence.event({"type": "ready", "thread_id": "old"})
        for item_type in ("commandExecution", "webSearch", "collabAgentToolCall", "dynamicToolCall", "fileChange"):
            with self.assertRaises(driver.TrialFailure):
                evidence.event({"type": "item_activity", "data": {"item_type": item_type}})

    def test_approval_request_rejected(self):
        _, _, _, evidence, _ = self.exercise()
        with self.assertRaises(driver.TrialFailure):
            evidence.event({"type": "approval_required", "request": {"untrusted": "not printed"}})

    def test_unplanned_native_compaction_rejected(self):
        _, _, receipt, evidence, _ = self.exercise()
        with self.assertRaises(driver.TrialFailure):
            evidence.event({"type": "item_activity", "data": {"item_type": "contextCompaction"}})
        self.assertTrue(receipt["native_compaction_observed"])

    def test_cumulative_notifications_not_added_to_exact_responses(self):
        fixture, owner, receipt, evidence, transport = self.exercise()
        driver.drive(transport, evidence, fixture, owner)
        event = {"type": "usage", "data": {"thread_id": "synthetic-owned-source", "total": {"inputTokens": 600}}}
        evidence.event(event)
        evidence.event(event)
        self.assertEqual(sum(r["usage"]["input_tokens"] for r in receipt["responses"]), 600)

    def test_missing_or_inconsistent_usage_cannot_qualify(self):
        fixture, owner, _, evidence, transport = self.exercise()
        driver.drive(transport, evidence, fixture, owner)
        evidence.response_totals["synthetic-owned-source"]["input_tokens"] += 1
        self.assertFalse(evidence.usage_reconciled())
        evidence.response_totals["synthetic-owned-source"] = None
        self.assertFalse(evidence.usage_reconciled())

    def test_partial_response_usage_recorded_without_completion(self):
        _, _, receipt, evidence, _ = self.exercise()
        evidence.current_phase, evidence.current_task_turn = "pre_intervention", 1
        data = {"thread_id": "owned", "response_id_sha256": "hash", "usage": {
            "input_tokens": 100, "cached_input_tokens": 50, "output_tokens": 10}}
        evidence.event({"type": "response_usage", "data": data})
        evidence.event({"type": "response_usage", "data": data})
        self.assertEqual(len(receipt["responses"]), 1)
        self.assertEqual(receipt["completed_task_turns"], 0)
        self.assertFalse(evidence.usage_reconciled())

    def test_failed_main_collects_usage_without_turn_completed(self):
        class FailedTransport(FakeTransport):
            def send(self, command):
                if command["type"] == "turn":
                    self.usage("partial")
                    self.events.append(driver.TrialFailure("synthetic owned turn failed"))
                else:
                    super().send(command)
            def receive(self):
                value = self.events.popleft()
                if isinstance(value, Exception):
                    raise value
                return value
            def close(self):
                pass
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            binary = base / "fake-owner-binary"
            binary.write_text("offline placeholder; never executed")
            output = base / "failed-run"
            fixture = trial.make_fixture()
            argv = ["run_trial.py", "--gobstopper", str(binary), "--codex-bin", str(binary),
                    "--codex-home", str(base / "isolated"), "--output", str(output),
                    "--model", "test-model", "--effort", "test-effort", "--arm", "baseline", "--execution-order", "1", "--execute"]
            with mock.patch("sys.argv", argv), mock.patch.object(driver, "ProcessTransport",
                    side_effect=lambda *args: FailedTransport(output / "owner", fixture)), mock.patch("builtins.print"):
                with self.assertRaises(SystemExit) as raised:
                    driver.main()
            self.assertEqual(raised.exception.code, 1)
            receipt = json.loads((output / "receipt.json").read_text())
            self.assertEqual(receipt["status"], "failed")
            self.assertEqual(receipt["completed_task_turns"], 0)
            self.assertEqual(len(receipt["responses"]), 1)
            self.assertEqual(receipt["responses"][0]["usage"]["input_tokens"], 100)
            self.assertFalse(receipt["usage_coverage_complete"])

    def test_no_silent_second_intervention(self):
        fixture, owner, receipt, evidence, transport = self.exercise("native_early")
        driver.drive(transport, evidence, fixture, owner)
        transport.record("native_compaction_intent", {})
        with self.assertRaises(driver.TrialFailure):
            driver.map_intervention(receipt, driver.owner_receipts(owner))

    def test_budget_stop_and_reservation(self):
        _, _, receipt, evidence, _ = self.exercise()
        receipt["responses"] = [{"usage": {"input_tokens": 250001, "output_tokens": 0}}]
        with self.assertRaises(driver.TrialFailure):
            evidence.check_budgets()
        receipt["responses"][0]["usage"]["input_tokens"] = 249000
        with self.assertRaises(driver.TrialFailure):
            evidence.reserve([], "next prompt")

    def test_only_json_or_single_json_fence_accepted(self):
        self.assertEqual(driver.artifact_from_text('```json\n{"a":1}\n```'), {"a": 1})
        for text in ('[]', 'Here it is: {"a":1}', '{"a":1}\nextra'):
            with self.assertRaises((driver.TrialFailure, ValueError)):
                driver.artifact_from_text(text)


if __name__ == "__main__":
    unittest.main()

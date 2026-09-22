#!/usr/bin/env python3
"""One bounded synthetic Codex arm. Dry-run by default; no credential handling."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import threading
import time

import synthetic_trial as trial

MAX_FRAME = 8 * 1024 * 1024
MAX_SNAPSHOT = 64 * 1024 * 1024
POLICY = {"trigger": 12000, "floor": 8000, "keep-recent-outputs": 2,
          "min-savings-tokens": 2048, "min-prefix-tokens": 1024,
          "min-interval-secs": 600, "min-interval-turns": 3,
          "min-growth-tokens": 8192, "timeout-secs": 180}


class TrialFailure(Exception):
    pass


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def save(path, data):
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    path.chmod(0o600)


def child_environment(source, codex_home):
    allowed = {"HOME", "PATH", "USER", "LOGNAME", "LANG", "LC_ALL", "LC_CTYPE", "TMPDIR",
               "SYSTEMROOT", "WINDIR", "__CF_USER_TEXT_ENCODING", "CODEX_SANDBOX",
               "CODEX_SANDBOX_NETWORK_DISABLED", "APP_SANDBOX_CONTAINER_ID"}
    env = {key: value for key, value in source.items() if key in allowed}
    env["CODEX_HOME"] = str(codex_home)
    return env


def validate_workspace(path):
    if not path.is_absolute() or path.is_symlink() or not path.is_dir():
        raise TrialFailure("shared workspace must be an existing absolute nonsymlink directory")
    stat = path.stat()
    if stat.st_uid != os.getuid() or stat.st_mode & 0o077:
        raise TrialFailure("shared workspace must be private and owned by this user")
    if next(path.iterdir(), None) is not None:
        raise TrialFailure("shared workspace must be empty before each arm")
    return path.resolve()


def artifact_from_text(text):
    text = text.strip()
    if text.startswith("```json\n") and text.endswith("\n```"):
        text = text[len("```json\n"):-len("\n```")]
    value = json.loads(text)
    if not isinstance(value, dict):
        raise TrialFailure("final artifact must be a JSON object")
    return value


class ProcessTransport:
    def __init__(self, argv, codex_home, deadline):
        self.deadline, self.events = deadline, queue.Queue(maxsize=64)
        env = child_environment(os.environ, codex_home)
        self.process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, env=env, start_new_session=True)
        def reader():
            try:
                while True:
                    line = self.process.stdout.readline(MAX_FRAME + 1)
                    if not line:
                        self.events.put(TrialFailure("owner exited before expected event"))
                        return
                    if len(line) > MAX_FRAME:
                        self.events.put(TrialFailure("owner frame exceeded limit"))
                        return
                    self.events.put(json.loads(line))
            except Exception:
                self.events.put(TrialFailure("invalid owner event stream"))
        threading.Thread(target=reader, daemon=True).start()

    def send(self, value):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TrialFailure("arm deadline reached")
        encoded = (trial.canonical(value) + "\n").encode()
        if len(encoded) > MAX_FRAME:
            raise TrialFailure("command frame exceeds bound")
        completed = queue.Queue(maxsize=1)
        def write():
            try:
                self.process.stdin.write(encoded)
                self.process.stdin.flush()
                completed.put(None)
            except (OSError, ValueError) as error:
                completed.put(error)
        threading.Thread(target=write, daemon=True).start()
        try:
            result = completed.get(timeout=min(remaining, 180))
        except queue.Empty as error:
            raise TrialFailure("owner input write deadline reached") from error
        if result is not None:
            raise TrialFailure("owner input write failed") from result

    def receive(self):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TrialFailure("arm deadline reached")
        try:
            result = self.events.get(timeout=min(remaining, 180))
        except queue.Empty as error:
            raise TrialFailure("owner response deadline reached") from error
        if isinstance(result, Exception):
            raise result
        return result

    def close(self):
        try:
            self.send({"type": "stop"})
            self.process.stdin.close()
            self.process.wait(timeout=5)
        except (OSError, ValueError, TrialFailure, subprocess.TimeoutExpired):
            # This process created the private process group, which contains only its owner/runtime.
            try:
                if self.process.poll() is None and os.getpgid(self.process.pid) == self.process.pid:
                    os.killpg(self.process.pid, signal.SIGTERM)
                    try:
                        self.process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(self.process.pid, signal.SIGKILL)
                        self.process.wait(timeout=5)
            except ProcessLookupError:
                self.process.wait(timeout=5)


class Evidence:
    def __init__(self, receipt):
        self.receipt, self.notifications = receipt, []
        self.owner_events, self.last_assistant = [], ""
        self.seen_responses, self.seen_snapshots = set(), set()
        self.total_by_thread, self.item_types = {}, set()
        self.response_totals, self.recorded_by_thread = {}, {}
        self.stream_response_hashes = set()
        self.thread_ids = set()
        self.current_phase = "setup"
        self.current_task_turn = None
        self.response_by_id = {}

    def event(self, event):
        kind = event.get("type")
        if kind in ("approval_required", "error", "blocked"):
            raise TrialFailure("owner reported " + kind)
        if kind == "ready":
            if event.get("activity_reporting") is not True:
                raise TrialFailure("owner lacks activity reporting required by trial")
            self.thread_ids.add(event["thread_id"])
            self.receipt["source_thread_sha256"] = sha(event["thread_id"])
            self.receipt["continuation_thread_sha256"] = sha(event["thread_id"])
        elif kind == "continuation":
            self.thread_ids.add(event["thread_id"])
            self.receipt["continuation_thread_sha256"] = sha(event["thread_id"])
        elif kind == "assistant":
            self.last_assistant = event.get("text") or ""
        elif kind == "item_activity":
            item_type = event.get("data", {}).get("item_type")
            self.item_types.add(item_type)
            if item_type not in ("agentMessage", "reasoning", "contextCompaction", "userMessage"):
                raise TrialFailure("unexpected provider tool or agent activity")
            if item_type == "contextCompaction" and not (self.receipt["arm"] == "native_early" and self.current_phase == "compaction"):
                self.receipt["native_compaction_observed"] = True
                raise TrialFailure("unexpected native compaction contaminated arm")
        elif kind == "response_usage":
            data = event["data"]
            self.stream_response_hashes.add(data["response_id_sha256"])
            self.add_response(data["response_id_sha256"], data["thread_id"], data.get("usage", {}),
                              self.current_phase, self.current_task_turn)
        elif kind == "usage":
            data = event["data"]
            self.notifications.append(data)
            self.total_by_thread[data["thread_id"]] = data.get("total", {})
        self.owner_events.append({k: v for k, v in event.items() if k not in ("text", "data", "request")})
        self.check_budgets()

    def add_response(self, response_hash, thread, raw, phase, task_turn):
        usage = {key: raw.get(key) if trial.nonnegative_integer(raw.get(key)) else None for key in trial.USAGE_KEYS}
        if response_hash in self.response_by_id:
            if self.response_by_id[response_hash]["usage"] != usage:
                raise TrialFailure("conflicting usage for one response identity")
            return
        self.seen_responses.add(response_hash)
        row = {"response_id": response_hash, "thread_sha256": sha(thread), "turn": task_turn,
               "phase": phase, "usage_basis": "per_response_delta", "usage": usage}
        self.response_by_id[response_hash] = row
        self.receipt["responses"].append(row)
        self.recorded_by_thread.setdefault(thread, []).append(usage)

    def check_budgets(self):
        receipt = self.receipt
        values = {key: sum((response.get("usage", {}).get(key) or 0)
                          for response in receipt["responses"]) for key in trial.USAGE_KEYS}
        # Notification cumulative totals provide an additional live stop signal, not billable response accounting.
        input_observed = max(values["input_tokens"], sum((v.get("inputTokens") or 0) for v in self.total_by_thread.values()))
        output_observed = max(values["output_tokens"], sum((v.get("outputTokens") or 0) for v in self.total_by_thread.values()))
        if len(receipt["responses"]) > trial.LIMITS["max_provider_responses"]:
            raise TrialFailure("provider response cap exceeded")
        if input_observed > trial.LIMITS["max_total_input_tokens"]:
            raise TrialFailure("input token cap exceeded")
        if output_observed > trial.LIMITS["max_total_output_tokens"]:
            raise TrialFailure("output token cap exceeded")

    def snapshots(self, owner_dir, phase, task_turn):
        files = sorted(owner_dir.glob("snapshot-*.jsonl"))
        if len(files) > 32:
            raise TrialFailure("snapshot count cap exceeded")
        for path in files:
            if path.name in self.seen_snapshots:
                continue
            if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_SNAPSHOT:
                raise TrialFailure("invalid owned snapshot")
            self.seen_snapshots.add(path.name)
            with path.open() as stream:
                for line in stream:
                    row = json.loads(line)
                    if row.get("type") != "token_usage_record":
                        continue
                    payload = row.get("payload", {})
                    if payload.get("thread_id") not in self.thread_ids:
                        continue
                    response_id = payload.get("response_id")
                    if not isinstance(response_id, str) or not response_id:
                        raise TrialFailure("usage record lacks response identity")
                    raw = payload.get("usage", {})
                    thread = payload["thread_id"]
                    self.add_response(sha(response_id), thread, raw, phase, task_turn)
                    self.response_totals[thread] = payload.get("thread_token_usage")
        self.check_budgets()

    def reserve(self, injected_items, text):
        input_so_far = sum((r["usage"].get("input_tokens") or 0) for r in self.receipt["responses"])
        output_so_far = sum((r["usage"].get("output_tokens") or 0) for r in self.receipt["responses"])
        last = self.notifications[-1].get("last", {}) if self.notifications else {}
        # Admission estimate only: a remote call can exceed an estimate before usage is reported.
        estimate = (last.get("inputTokens") or 6000) + (last.get("outputTokens") or 0)
        estimate += (len(trial.canonical(injected_items).encode()) + len(text.encode())) // 3 + 1024
        self.receipt.setdefault("reservations", []).append({"input_estimate": estimate,
            "output_allowance": 2000, "input_observed_before": input_so_far, "output_observed_before": output_so_far,
            "hard_provider_bound": False})
        if input_so_far + estimate > trial.LIMITS["max_total_input_tokens"] or output_so_far + 2000 > trial.LIMITS["max_total_output_tokens"]:
            raise TrialFailure("remaining budget cannot admit next estimated request")

    def usage_reconciled(self):
        task_turns = {r.get("turn") for r in self.receipt["responses"] if r.get("phase") != "compaction"}
        if task_turns != set(range(1, 7)):
            return False
        if self.seen_responses != self.stream_response_hashes:
            return False
        for thread, rows in self.recorded_by_thread.items():
            total = self.response_totals.get(thread)
            if not isinstance(total, dict):
                return False
            for key in ("input_tokens", "cached_input_tokens", "output_tokens"):
                if not trial.nonnegative_integer(total.get(key)) or any(r.get(key) is None for r in rows):
                    return False
                if sum(r[key] for r in rows) != total[key]:
                    return False
        return bool(self.recorded_by_thread)


def expect(transport, evidence, expected):
    while True:
        event = transport.receive()
        evidence.event(event)
        if event.get("type") == expected:
            return event


def owner_receipts(owner_dir):
    receipts = []
    paths = sorted(owner_dir.glob("receipt-*.json"))
    if len(paths) > 2048:
        raise TrialFailure("owner receipt count cap exceeded")
    for path in paths:
        if path.is_symlink() or path.stat().st_size > 1048576:
            raise TrialFailure("invalid owned receipt")
        receipts.append(json.loads(path.read_text()))
    return receipts


def map_intervention(receipt, records, require_adoption=False):
    policy = [r for r in records if r.get("kind") == "policy" and r["data"].get("reason") == "eligible"]
    intents = [r for r in records if r.get("kind") in ("native_compaction_intent", "custom_compaction_intent")]
    outcomes = [r for r in records if r.get("kind") in ("native_compaction_completed", "custom_injection_accepted")]
    if receipt["arm"] == "baseline":
        if intents:
            raise TrialFailure("baseline intervention contamination")
        return
    if len(policy) != 1 or len(intents) != 1 or len(outcomes) != 1:
        raise TrialFailure("expected exactly one admitted and completed intervention")
    if policy[0]["data"].get("completed_turns") != 3:
        raise TrialFailure("intervention occurred at wrong boundary")
    outcome_status = "succeeded"
    if receipt["arm"] == "custom_early":
        adopted = [r for r in records if r.get("kind") == "adoption_verified"]
        if require_adoption and len(adopted) != 1:
            raise TrialFailure("custom continuation adoption was not verified")
        if adopted:
            outcomes = adopted
        else:
            outcome_status = "accepted"
    decision_id, operation_id = "policy-" + str(policy[0]["sequence"]), "operation-" + str(intents[0]["sequence"])
    receipt["events"] = [
        {"type": "policy_decision", "decision_id": decision_id, "context_tokens": policy[0]["data"].get("context_tokens"),
         "trigger_tokens": POLICY["trigger"], "reason": "eligible", "timestamp_ms": policy[0]["timestamp_ms"]},
        {"type": "dispatch", "decision_id": decision_id, "operation_id": operation_id,
         "after_completed_turn": 3, "action": "native_compact" if receipt["arm"] == "native_early" else "new_thread_inject",
         "timestamp_ms": intents[0]["timestamp_ms"]},
        {"type": "outcome", "operation_id": operation_id, "status": outcome_status,
         "timestamp_ms": outcomes[0]["timestamp_ms"]}]


def drive(transport, evidence, fixture, owner_dir):
    expect(transport, evidence, "ready")
    for turn in fixture["turns"]:
        number = turn["turn"]
        if number == 4:
            evidence.current_phase = "compaction"
            evidence.current_task_turn = None
            transport.send({"type": "prepare"})
            expect(transport, evidence, "boundary_complete")
            evidence.snapshots(owner_dir, "compaction", None)
            map_intervention(evidence.receipt, owner_receipts(owner_dir))
        evidence.current_phase = "pre_intervention" if number <= 3 else "post_intervention"
        evidence.current_task_turn = number
        transport.send({"type": "inject", "items": turn["injected_items"]})
        expect(transport, evidence, "injected")
        instructions = fixture["task_instructions"] + "\n\n" if number == 1 else ""
        if number == 6:
            extra = "\nReturn only the final JSON object in this response; do not write a file or call tools."
        else:
            extra = "\nRespond briefly using only the supplied evidence. Do not call tools or write files."
        evidence.last_assistant = ""
        text = instructions + turn["user"] + extra
        evidence.reserve(turn["injected_items"], text)
        transport.send({"type": "turn", "text": text})
        expect(transport, evidence, "turn_completed")
        evidence.receipt["completed_task_turns"] = number
        evidence.snapshots(owner_dir, "pre_intervention" if number <= 3 else "post_intervention", number)
    artifact = artifact_from_text(evidence.last_assistant)
    evidence.receipt["artifact_verification"] = trial.verify_artifact(fixture, artifact)
    if not evidence.receipt["artifact_verification"]["passed"]:
        raise TrialFailure("final artifact failed deterministic verification")
    return artifact


def owner_argv(args, run_dir):
    argv = [str(args.gobstopper), "codex-session", "--experimental", "--state-dir", str(run_dir / "owner"),
            "--codex-home", str(args.codex_home), "--codex-bin", str(args.codex_bin),
            "--cwd", str(args.workspace or run_dir / "provider-workspace"), "--model", args.model, "--effort", args.effort,
            "--mode", {"baseline": "off", "native_early": "native", "custom_early": "custom"}[args.arm]]
    for key, value in POLICY.items():
        argv.extend(["--" + key, str(value)])
    return argv


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("gobstopper", "codex-bin", "codex-home", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--effort", required=True)
    parser.add_argument("--arm", choices=trial.ARMS, required=True)
    parser.add_argument("--seed", type=int, default=20260921)
    parser.add_argument("--workspace", type=Path, help="Existing empty private directory shared by sequential arms")
    parser.add_argument("--execution-order", type=int, choices=(1, 2, 3), required=True)
    parser.add_argument("--block-id", help="Shared identifier for one matched three-arm block")
    parser.add_argument("--execute", action="store_true")
    args = parser.parse_args()
    for field in ("gobstopper", "codex_bin", "codex_home", "output"):
        if not getattr(args, field).is_absolute():
            parser.error("all paths must be absolute")
    if args.codex_home.resolve() == (Path.home() / ".codex").resolve():
        parser.error("requires isolated Codex home supplied by the controller")
    if args.output.exists():
        parser.error("output must not already exist; retain failed runs")
    if args.workspace:
        try:
            args.workspace = validate_workspace(args.workspace)
        except TrialFailure as error:
            parser.error(str(error))
    workspace = args.workspace or args.output / "provider-workspace"
    block_id = args.block_id or "canary-" + str(args.seed)
    fixture = trial.make_fixture(args.seed)
    argv = owner_argv(args, args.output)
    plan = {"schema": trial.SCHEMA, "status": "prepared", "fixture_sha256": trial.digest(fixture),
            "seed": args.seed, "arm": args.arm, "argv": argv, "policy": POLICY, "limits": trial.LIMITS,
            "workspace_sha256": sha(str(workspace)), "execution_order": args.execution_order, "block_id": block_id,
            "warm_cache_uncertainty": "Shared provider cache is uncontrolled; earlier arms may warm later prefixes.",
            "provider_calls": 0, "isolation": "controller-supplied; driver never reads credentials",
            "note": "Execute only after payload, isolation and exact runtime review; no live Desktop access."}
    if not args.execute:
        print(json.dumps(plan, indent=2))
        return
    args.output.mkdir(mode=0o700, parents=False)
    if not args.workspace:
        workspace.mkdir(mode=0o700)
    save(args.output / "plan.json", plan)
    save(args.output / "fixture-controller-only.json", fixture)
    receipt = trial.receipt_template(fixture, args.arm, args.model, args.effort)
    receipt.update(status="running", codex_version="codex-cli 0.155.0", transport="app_server",
                   workspace_path=str(workspace), workspace_sha256=sha(str(workspace)),
                   execution_order=args.execution_order, block_id=block_id,
                   gobstopper_sha256=hashlib.sha256(args.gobstopper.read_bytes()).hexdigest())
    evidence, transport = Evidence(receipt), None
    start = time.monotonic()
    try:
        transport = ProcessTransport(argv, args.codex_home, start + trial.LIMITS["max_seconds"])
        artifact = drive(transport, evidence, fixture, args.output / "owner")
        save(args.output / "final-artifact.json", artifact)
        records = owner_receipts(args.output / "owner")
        map_intervention(receipt, records, require_adoption=True)
        receipt["status"] = "complete"
        receipt["usage_coverage_complete"] = evidence.usage_reconciled()
        receipt["usage_missing_response_count"] = 0 if receipt["usage_coverage_complete"] else None
    except (TrialFailure, OSError, ValueError, KeyError) as error:
        receipt["status"], receipt["stop_reason"] = "failed", str(error)
    finally:
        if transport is not None:
            try:
                transport.close()
            except (OSError, ValueError, TrialFailure, subprocess.TimeoutExpired) as error:
                receipt.setdefault("cleanup_errors", []).append(type(error).__name__)
                receipt["status"], receipt["stop_reason"] = "failed", receipt["stop_reason"] or "owner cleanup incomplete"
        try:
            # Failure receipts can contain incurred usage even without a completed-turn acknowledgement.
            for record in owner_receipts(args.output / "owner"):
                if record.get("kind") == "response_usage":
                    evidence.event({"type": "response_usage", "data": record["data"]})
            evidence.snapshots(args.output / "owner", evidence.current_phase, evidence.current_task_turn)
        except (TrialFailure, OSError, ValueError, KeyError) as error:
            receipt.setdefault("evidence_collection_errors", []).append(str(error))
            receipt["status"], receipt["stop_reason"] = "failed", receipt["stop_reason"] or "evidence collection failed"
        receipt["usage_coverage_complete"] = evidence.usage_reconciled() and not receipt.get("evidence_collection_errors")
        receipt["usage_missing_response_count"] = 0 if receipt["usage_coverage_complete"] else None
        receipt["elapsed_seconds"] = round(time.monotonic() - start, 3)
        receipt["observed_item_types"] = sorted(evidence.item_types)
        receipt["live_budget_notifications"] = evidence.notifications
        (args.output / "last-assistant.txt").write_text(evidence.last_assistant)
        (args.output / "last-assistant.txt").chmod(0o600)
        save(args.output / "receipt.json", receipt)
        save(args.output / "summary.json", trial.summarize_receipt(receipt))
    print(json.dumps({"status": receipt["status"], "stop_reason": receipt["stop_reason"],
                      "completed_task_turns": receipt["completed_task_turns"],
                      "recorded_responses": len(receipt["responses"]),
                      "quality_passed": (receipt["artifact_verification"] or {}).get("passed")}, indent=2))
    raise SystemExit(0 if receipt["status"] == "complete" else 1)


if __name__ == "__main__":
    main()

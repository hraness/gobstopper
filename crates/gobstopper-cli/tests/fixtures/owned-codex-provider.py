#!/usr/bin/env python3
"""Immutable offline app-server fixture. No network or provider implementation."""
import json
import os
import pathlib
import sys
import time

if "--version" in sys.argv:
    print("codex-cli 0.155.0")
    raise SystemExit

home = pathlib.Path(os.environ["CODEX_HOME"])
home.mkdir(exist_ok=True)
threads = {}
sequence = 0


def send(value):
    print(json.dumps(value), flush=True)


def append(thread, kind, payload):
    with thread["path"].open("a") as stream:
        stream.write(json.dumps({"type": kind, "payload": payload}) + "\n")
        stream.flush()


def finish(thread_id, native=False):
    global sequence
    sequence += 1
    thread = threads[thread_id]
    turn = "turn-" + str(sequence)
    append(thread, "response_item", {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "synthetic result"}]})
    thread["total"] += 10000
    usage = {"input_tokens": 10000, "cached_input_tokens": 8000, "output_tokens": 10, "reasoning_output_tokens": 0, "total_tokens": 10010}
    total = dict(usage, input_tokens=thread["total"], cached_input_tokens=thread["total"] * 8 // 10)
    record = {"usage": usage, "thread_token_usage": total, "model_context_window": 200000, "turn_id": turn, "thread_id": thread_id, "response_id": "response-" + str(sequence)}
    append(thread, "token_usage_record", record)
    if thread["model"] == "fixture-duplicate-usage":
        append(thread, "token_usage_record", record)
    if thread["model"] == "fixture-conflicting-usage":
        append(thread, "token_usage_record", dict(record, usage=dict(usage, input_tokens=11000)))
    failed=thread["model"] in ("fixture-failed-turn", "fixture-unknown-turn") or (native and thread["model"]=="fixture-failed-native")
    if not failed:
        append(thread, "event_msg", {"type": "task_complete", "task_id": turn})
    if native:
        send({"method": "turn/completed", "params": {"threadId": thread_id, "turn": {"id": "unrelated-old-turn", "status": "completed"}}})
        send({"method": "item/started", "params": {"threadId": thread_id, "turnId": turn, "item": {"id": "compact-1", "type": "contextCompaction"}}})
    value = {"method": "thread/tokenUsage/updated", "params": {"threadId": thread_id, "turnId": turn, "tokenUsage": {"last": {"inputTokens": 10000, "cachedInputTokens": 8000, "outputTokens": 10, "reasoningOutputTokens": 0, "totalTokens": 10010}, "total": {"inputTokens": thread["total"], "cachedInputTokens": thread["total"] * 8 // 10, "outputTokens": 10, "reasoningOutputTokens": 0, "totalTokens": thread["total"] + 10}, "modelContextWindow": 200000}}}
    send(value)
    send(value)  # Repeated provider usage notification must not double count.
    if failed:
        with thread["path"].open("ab") as stream:
            stream.write(b'{"unterminated_private_content":"not ledger data')
    if thread["model"] == "fixture-unknown-turn":
        raise SystemExit
    send({"method": "turn/completed", "params": {"threadId": thread_id, "turn": {"id": turn, "status": "failed" if failed else "completed"}}})
    return turn


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    params = request.get("params", {})
    if method == "initialized":
        continue
    result = {}
    if method == "thread/start":
        thread_id = "owned-" + str(len(threads))
        directory = home / "sessions"
        directory.mkdir(exist_ok=True)
        path = directory / (thread_id + ".jsonl")
        path.touch(exist_ok=False)
        thread = {"path": path, "model": params["model"], "total": 0}
        threads[thread_id] = thread
        append(thread, "session_meta", {"id": thread_id})
        result = {"model": params["model"], "cwd": params["cwd"], "instructionSources": [], "reasoningEffort": params["config"]["model_reasoning_effort"], "approvalPolicy": "on-request", "sandbox": {"type": "workspaceWrite" if params["sandbox"] == "workspace-write" else "readOnly"}, "thread": {"id": thread_id, "path": str(path)}}
        if params["model"] == "fixture-mismatch":
            result["model"] = "unexpected-model"
        if params["model"] == "fixture-outside":
            result["thread"]["path"] = "/private/tmp/not-owned.jsonl"
    elif method == "thread/inject_items":
        thread = threads[params["threadId"]]
        if thread["model"] == "fixture-corrupt-candidate" and params["threadId"] != "owned-0":
            append(thread, "response_item", {"type":"message","role":"user","content":[{"type":"input_text","text":"unexpected candidate prefix"}]})
        if thread["model"] == "fixture-async-injection":
            thread.setdefault("pending_items", []).extend(params["items"])
        else:
            for item in params["items"]:
                append(thread, "response_item", item)
        if thread["model"] == "fixture-inject-unknown" and params["threadId"] != "owned-0":
            raise SystemExit  # The write happened; the acknowledgement did not.
    elif method == "turn/start":
        thread_id = params["threadId"]
        thread = threads[thread_id]
        for item in thread.pop("pending_items", []):
            append(thread,"response_item",item)
        append(thread, "response_item", {"type": "message", "role": "user", "content": [{"type": "input_text", "text": params["input"][0]["text"]}]})
        if thread["model"] == "fixture-approval":
            send({"id": 1001, "method": "item/commandExecution/requestApproval", "params": {"threadId": thread_id, "command": "synthetic test only"}})
            continue
        result = {"turn": {"id": finish(thread_id)}}
    elif method == "thread/compact/start":
        send({"id": request["id"], "result": {}})
        finish(params["threadId"], native=True)
        continue
    elif method != "initialize":
        send({"id": request["id"], "error": {"code": -32601, "message": "unexpected fixture method"}})
        continue
    send({"id": request["id"], "result": result})
    if method == "thread/start" and params["model"] == "fixture-stalled-reader":
        time.sleep(30)

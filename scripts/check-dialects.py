#!/usr/bin/env python3
"""Validate frozen synthetic corpus pins and run a bounded deterministic driver."""
import argparse
import hashlib
import json
from pathlib import Path
import os
import subprocess

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / "crates/gobstopper-adapters/tests/fixtures/dialects-v1"


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate corpus key")
        result[key] = value
    return result


def require(condition, message="invalid corpus contract"):
    if not condition:
        raise ValueError(message)


def check_corpus():
    manifest = json.loads((CORPUS / "manifest.json").read_text(), object_pairs_hook=unique_object)
    require(manifest["schema"] == 1)
    require(manifest["source"] == "hand-authored synthetic contracts")
    require(manifest["provider_qualification"] == "unqualified")
    seen = set()
    providers = set()
    for case in manifest["fixtures"]:
        name = case["file"]
        require(Path(name).name == name and name.endswith(".jsonl") and name not in seen)
        seen.add(name)
        providers.add(case["provider"])
        raw = (CORPUS / name).read_bytes()
        require(hashlib.sha256(raw).hexdigest() == case["sha256"], f"hash mismatch: {name}")
        records = [json.loads(line, object_pairs_hook=unique_object) if line.strip() else None for line in raw.decode().splitlines()]
        indexes = [item["line"] for item in case["items"]]
        require(len(indexes) == len(set(indexes)) and indexes == sorted(indexes))
        require(all(0 <= index < len(records) for index in indexes))
        eligible = {item["line"] for item in case["items"] if item["elidable_bytes"] is not None}
        require(eligible == {change["line"] for change in case["changes"]})
        for change in case["changes"]:
            value = records[change["line"]]
            require(change["pointer"].startswith("/"))
            for component in change["pointer"].split("/")[1:]:
                key = component.replace("~1", "/").replace("~0", "~")
                value = value[int(key)] if isinstance(value, list) else value[key]
            require(isinstance(value, str) and isinstance(change["value"], str))
    require(providers == {"codex", "claude-code", "devin"})
    require(seen == {path.name for path in CORPUS.glob("*.jsonl")})
    print(f"frozen corpus PASS fixtures={len(seen)} provider_qualification=none", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cases", type=int, default=512)
    parser.add_argument("--seed", type=int, default=0x475320260923)
    parser.add_argument("--corpus-only", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.cases <= 4096 or not 0 <= args.seed <= (1 << 64) - 1:
        parser.error("cases must be 1..4096 and seed must be a u64")
    check_corpus()
    if not args.corpus_only:
        env = os.environ.copy()
        env["DIALECT_FUZZ_CASES"] = str(args.cases)
        env["DIALECT_FUZZ_SEED"] = str(args.seed)
        subprocess.run(["cargo", "test", "-p", "gobstopper-adapters", "--test", "dialects", "--locked", "--", "--nocapture"], cwd=ROOT, env=env, check=True, timeout=900)


if __name__ == "__main__":
    main()

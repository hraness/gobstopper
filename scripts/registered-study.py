"""Execute frozen public measurement fixtures; never dispatch a provider/model."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import time

ROOT = Path(__file__).resolve().parent
CORPUS = ROOT / "fixtures/measurement-v1"
SPEC = importlib.util.spec_from_file_location("bounded_study", ROOT / "compaction-study.py")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


def require(value, reason):
    if not value:
        raise RuntimeError(reason)


def strict_json(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "duplicate_json_field")
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=unique)


def validate_corpus(root=CORPUS):
    raw = (root / "registration.json").read_bytes()
    require(len(raw) <= 1024 * 1024, "registration_limit")
    registration = strict_json(raw)
    require(registration["schema"] == "gobstopper-preregistered-measurement-v1", "registration_schema")
    require(registration["provider_calls_allowed"] == 0 and registration["outcome_dependent_selection"] is False,
            "registration_authority")
    require(len(registration["cases"]) == 4 and {case["id"] for case in registration["cases"]}
            == {"negation", "superseded_approval", "pending_effect", "tool_state"}, "case_count")
    require(registration["policy"] == {"rounds": 1, "trigger_tokens": 1, "floor_tokens": 64,
            "keep_recent_tool_outputs": 1, "min_savings_tokens": 0}, "policy_changed")
    for name, expected in registration["asset_sha256"].items():
        require(Path(name).name == name and not (root / name).is_symlink(), "asset_path")
        data = (root / name).read_bytes()
        require(len(data) <= 1024 * 1024 and RUNNER.sha(data) == expected, "asset_changed")
    for case in registration["cases"]:
        require(all(case[key] in registration["asset_sha256"] for key in
                    ("source", "manifest", "literal_false_assurance_control")), "case_asset_missing")
    return registration, RUNNER.sha(raw)


def validate_report(report, case, registration, control=False):
    require(report["source_sha256"] == registration["asset_sha256"][case["source"]], "source_binding")
    require(report["manifest_sha256"] == registration["asset_sha256"][case["manifest"]], "manifest_binding")
    require(report["provider_calls"] == 0 and report["billed_cost_usd"] is None
            and report["continuation_success"] is None and report["semantic_equivalence_qualified"] is False,
            "unsupported_measurement_claim")
    expected = {"realized_after"} if control else {arm["id"] for arm in registration["arms"] if arm["mode"].startswith("offline_")}
    rows = report["rows"]
    require(len(rows) == len(expected) and {row["arm"] for row in rows} == expected, "incomplete_arms")
    for row in rows:
        retention = row["retention"]
        total = retention["total"]
        require(type(total) is int and 0 < total <= 256, "invalid_denominator")
        for field in ("retained", "lexical_retained", "same_origin_retained", "source_bound_retained"):
            count = retention[field]
            require(type(count) is int and 0 <= count <= total, "invalid_counter")
        require(row["round"] == 1 and row["verify_errors"] == 0 and row["new_verify_errors"] == 0, "invalid_replay")
        if row["arm"] == "no_compaction":
            require(row["source_sha256"] == row["result_sha256"] and retention["retained"] == total, "baseline_changed")
        if control:
            # The intentional contradiction still contains the old literal.
            # Passing demonstrates why this metric cannot establish obedience.
            require(retention["retained"] == case["control_expected_literal_retained"], "literal_control_mismatch")


def run(binary, output, corpus=CORPUS):
    registration, registered_hash = validate_corpus(corpus)
    os.umask(0o077)
    output.mkdir(mode=0o700, parents=False, exist_ok=False)
    pinned = output / "gobstopper"
    binary_bytes = binary.resolve(strict=True).read_bytes()
    require(0 < len(binary_bytes) <= 256 * 1024 * 1024, "binary_limit")
    pinned.write_bytes(binary_bytes)
    pinned.chmod(0o700)
    (output / "config/gobstopper").mkdir(parents=True, mode=0o700)
    (output / "config/gobstopper/config.toml").write_text(
        "[policy]\nadaptive=false\nkeep_recent_tool_outputs=1\nmin_savings_tokens=0\n")
    for home in ("codex", "claude", "data"):
        (output / home).mkdir(mode=0o700)
    environment = {key: value for key, value in os.environ.items() if not key.startswith(
        ("GOBSTOPPER_", "TYPESAFE_", "AI_GATEWAY_", "OPENAI_", "ANTHROPIC_", "VERCEL_", "DEVIN_"))}
    environment.update(XDG_CONFIG_HOME=str(output / "config"), XDG_DATA_HOME=str(output / "data"))
    receipt = {"registration": registration, "registration_sha256": registered_hash,
               "runner_sha256": RUNNER.sha(Path(__file__).read_bytes()),
               "command_runner_sha256": RUNNER.sha(Path(RUNNER.__file__).read_bytes()),
               "binary_sha256": RUNNER.sha(binary_bytes), "registered_before_outcomes": True}
    RUNNER.save(output / "registration.json", receipt)
    RUNNER.DEADLINE = time.monotonic() + 180
    results = []
    base = [str(pinned), "--codex-home", str(output / "codex"), "--claude-home", str(output / "claude"),
]
    for case in registration["cases"]:
        for control in (False, True):
            label = case["id"] + ("-literal-control" if control else "-replay")
            result = {"case": case["id"], "kind": "literal_false_assurance_control" if control else "offline_arms",
                      "execution_state": "incomplete", "passed": False}
            try:
                source, manifest = corpus / case["source"], corpus / case["manifest"]
                argv = [*base, "eval-study", str(source), "--manifest", str(manifest), "--json"]
                if control:
                    argv += ["--against", str(corpus / case["literal_false_assurance_control"])]
                else:
                    argv += ["--rounds", "1", "--trigger", "1", "--floor", "64"]
                code = RUNNER.command(argv, output / f"{label}.json", output / f"{label}.err", environment, timeout=20)
                require(code == 0, "study_command_failed")
                report = strict_json((output / f"{label}.json").read_bytes())
                validate_report(report, case, registration, control)
                result.update(execution_state="completed", passed=True, rows=report["rows"])
            except (OSError, ValueError, KeyError, TypeError, RuntimeError) as error:
                result["failure"] = str(error) if isinstance(error, RuntimeError) else "invalid_or_unavailable_result"
            results.append(result)
            # Persist each outcome, including failures, before the next trial.
            RUNNER.save(output / f"{label}.receipt.json", result)
    try:
        require(validate_corpus(corpus)[1] == registered_hash, "registration_changed")
        require(RUNNER.sha(pinned.read_bytes()) == receipt["binary_sha256"], "binary_changed")
        integrity = True
    except (OSError, RuntimeError):
        integrity = False
    unexecuted = [{"arm": arm["id"], "execution_state": "incomplete", "reason": "provider_qualification_unavailable"}
                  for arm in registration["arms"] if not arm["mode"].startswith("offline_")]
    report = {"schema": "gobstopper-preregistered-measurement-result-v1", "registration_sha256": registered_hash,
              "binary_sha256": receipt["binary_sha256"], "cases": results, "unexecuted_arms": unexecuted,
              "provider_calls": 0, "integrity_preserved": integrity, "semantic_equivalence": None,
              "continuation_task_success": None, "charged_tokens": None, "cache_hits": None, "refetches": None,
              "comparison_qualified": False, "qualification_limit": "offline_mechanics_only_native_and_task_studies_incomplete"}
    RUNNER.save(output / "results.json", report)
    return 0 if integrity and all(row["passed"] for row in results) else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--corpus-only", action="store_true")
    args = parser.parse_args()
    if args.corpus_only:
        registration, digest = validate_corpus()
        print(json.dumps({"cases": len(registration["cases"]), "registration_sha256": digest}))
        return 0
    if args.binary is None or args.output is None:
        parser.error("--binary and a new --output directory are required")
    code = run(args.binary, args.output.resolve())
    print(json.dumps({"offline_checks_passed": code == 0, "live_or_semantic_qualification": False}))
    return code


if __name__ == "__main__":
    raise SystemExit(main())

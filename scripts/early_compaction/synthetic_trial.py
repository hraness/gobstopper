#!/usr/bin/env python3
"""Public synthetic trial data and offline checks; never invokes a provider."""
import argparse
import hashlib
import json
from pathlib import Path
import random

SCHEMA = "gobstopper-early-synthetic-v1"
ARMS = ("baseline", "native_early", "custom_early")
USAGE_KEYS = ("input_tokens", "cached_input_tokens", "output_tokens",
              "reasoning_output_tokens", "cache_write_input_tokens")
LIMITS = {"max_provider_responses": 18, "max_task_turns": 6,
          "max_compaction_dispatches": 1, "max_total_input_tokens": 250000,
          "max_total_output_tokens": 12000, "max_seconds": 600,
          "max_fixture_bytes": 196608, "max_subagents": 0}


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def digest(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


def make_fixture(seed=20260921, ballast_rows=384):
    if not 0 <= ballast_rows <= 384:
        raise ValueError("ballast_rows must be between 0 and 384")
    rng = random.Random(seed)
    skus, depots = ["amber", "cobalt", "jade"], ["north", "east", "west"]
    rows = []
    for sku in skus:
        base = rng.randrange(80, 150)
        for i, depot in enumerate(depots):
            rows.append({"sku": sku, "depot": depot, "units": rng.randrange(9, 15),
                         "unit_cost_cents": base + i * 17})
    permits = [{"depot": d, "permit": "permit-" + d, "valid_through_tick": 6}
               for d in depots]
    inventory = {"result_id": "inventory-v1", "tick": 1,
                 "rows": rows, "permits": permits}
    amber_east = next(r for r in rows if (r["sku"], r["depot"]) == ("amber", "east"))
    corrections = {"result_id": "corrections-v2", "tick": 2, "corrections": [
        {"sku": "amber", "depot": "east", "units": amber_east["units"] - 3},
        {"sku": "cobalt", "depot": "west", "unit_cost_cents": 57}],
        "supersedes": "inventory-v1"}
    revocation = {"result_id": "permits-v3", "tick": 3,
                  "revoked": ["permit-north"],
                  "reason": "synthetic calibration hold", "effective_tick": 3}
    state = {(r["sku"], r["depot"]): dict(r) for r in rows}
    for correction in corrections["corrections"]:
        state[(correction["sku"], correction["depot"])].update(correction)
    demands = {sku: sum(state[(sku, d)]["units"] for d in ("east", "west")) - 4
               for sku in skus}
    demand = {"result_id": "demand-v4", "tick": 4, "units_by_sku": demands,
              "fulfillment_tick": 6}
    final_update = {"result_id": "prices-v5", "tick": 5,
                    "corrections": [{"sku": "jade", "depot": "west",
                                     "unit_cost_cents": 49}],
                    "supersedes": "inventory-v1"}
    ballast = [{"sample": i, "sensor": "retired-test-rig",
                "reading": rng.randrange(100000, 999999),
                "status": "historical-nonbinding", "expires_tick": 0,
                "note": "Calibration-only telemetry; never an inventory or price authority."}
               for i in range(ballast_rows)]
    inventory["historical_sensor_rows"] = ballast
    records = [inventory, corrections, revocation, demand, final_update,
               {"result_id": "audit-v6", "tick": 6,
                "historical_sensor_rows_are_expired": True}]
    prompts = [
        "Read inventory-v1. Prepare an inventory index, retaining source IDs. Do not fulfill yet.",
        "Read corrections-v2. Apply corrections and retain the prior amber/east stock only as historical evidence.",
        "Read permits-v3. State which depot cannot fulfill at tick 6. Keep all corrected facts and source references for later work.",
        "Read demand-v4. Compute a provisional cheapest feasible allocation using the latest valid facts.",
        "Read prices-v5. Recompute the allocation after this new price; do not reuse a stale total.",
        "Read audit-v6. Ignore expired calibration rows as authority. Produce final-artifact.json exactly in the requested schema. No extra suppliers, deficit, or revoked permits."
    ]
    turns = []
    for i, record in enumerate(records):
        arguments = {"result_id": record["result_id"]}
        call_id = f"synthetic-{seed}-{i + 1}"
        turns.append({"turn": i + 1, "user": "The linked tool result has been supplied. " + prompts[i],
                      "expected_tool": {"name": "synthetic_read_record", "arguments": arguments},
                      "tool_result": record, "injected_items": [
                          {"type": "function_call", "call_id": call_id, "name": "synthetic_read_record",
                           "arguments": canonical(arguments)},
                          {"type": "function_call_output", "call_id": call_id, "output": canonical(record)}]})
    fixture = {"schema": SCHEMA, "seed": seed, "public_synthetic": True,
               "limits": dict(LIMITS), "intervention_after_completed_turn": 3,
               "tools": [{"name": "synthetic_read_record", "input_schema": {
                   "type": "object", "properties": {"result_id": {"type": "string"}},
                   "required": ["result_id"], "additionalProperties": False}}],
               "task_instructions": (
                   "This is a synthetic integer-cost fulfillment task. Allocate each SKU's exact demand "
                   "at tick 6 with minimum total cost. Use latest corrections, capacities, and valid permits. "
                   "No backorders or fractional units. Earlier values remain historical only. "
                   "Every selected allocation must cite the result ID supplying its stock, price, and permit. "
                   "The final JSON has allocations sorted by sku then depot, each with sku, depot, units, "
                   "unit_cost_cents, stock_source, price_source, permit_source; plus total_cost_cents, "
                   "demand_source, excluded_depots, exclusion_source, and historical_amber_east_units. "
                   "Keep this task self-contained; do not spawn agents or access external data."),
               "turns": turns}
    if len(canonical(fixture).encode()) > LIMITS["max_fixture_bytes"]:
        raise ValueError("fixture byte limit exceeded")
    return fixture


def expected_artifact(fixture):
    records = [turn["tool_result"] for turn in fixture["turns"]]
    inventory, corrections, revocation, demand, update, _ = records
    state = {}
    for row in inventory["rows"]:
        state[(row["sku"], row["depot"])] = {
            **row, "stock_source": "inventory-v1", "price_source": "inventory-v1",
            "permit_source": "inventory-v1"}
    historical = state[("amber", "east")]["units"]
    for record in (corrections, update):
        for correction in record["corrections"]:
            row = state[(correction["sku"], correction["depot"])]
            for key, source in (("units", "stock_source"), ("unit_cost_cents", "price_source")):
                if key in correction:
                    row[key], row[source] = correction[key], record["result_id"]
    permits = {p["depot"]: p for p in inventory["permits"]}
    valid = {depot for depot, p in permits.items()
             if p["permit"] not in revocation["revoked"]
             and p["valid_through_tick"] >= demand["fulfillment_tick"]}
    allocations = []
    for sku, units in sorted(demand["units_by_sku"].items()):
        choices = sorted((r for r in state.values() if r["sku"] == sku and r["depot"] in valid),
                         key=lambda r: (r["unit_cost_cents"], r["depot"]))
        for row in choices:
            take = min(units, row["units"])
            if take:
                allocations.append({**row, "units": take})
                units -= take
        if units:
            raise ValueError("infeasible fixture")
    allocations.sort(key=lambda r: (r["sku"], r["depot"]))
    return {"allocations": allocations,
            "total_cost_cents": sum(r["units"] * r["unit_cost_cents"] for r in allocations),
            "demand_source": "demand-v4", "excluded_depots": ["north"],
            "exclusion_source": "permits-v3", "historical_amber_east_units": historical}


def verify_artifact(fixture, actual):
    expected = expected_artifact(fixture)
    if not isinstance(actual, dict):
        return {"schema": SCHEMA, "fixture_sha256": digest(fixture), "passed": False,
                "checks": {"artifact_is_object": False}, "checks_passed": 0, "checks_total": 1}
    checks = {key: actual.get(key) == value for key, value in expected.items()}
    checks["no_extra_fields"] = set(actual) == set(expected)
    return {"schema": SCHEMA, "fixture_sha256": digest(fixture),
            "passed": all(checks.values()), "checks": checks,
            "checks_passed": sum(checks.values()), "checks_total": len(checks)}


def receipt_template(fixture, arm, model, effort):
    if arm not in ARMS:
        raise ValueError("unknown arm")
    return {"schema": SCHEMA, "fixture_sha256": digest(fixture), "seed": fixture["seed"],
            "arm": arm, "model": model, "reasoning_effort": effort,
            "codex_version": None, "gobstopper_sha256": None,
            "tool_schema_sha256": digest(fixture["tools"]),
            "instructions_sha256": digest(fixture["task_instructions"]),
            "workspace_sha256": None, "execution_order": None, "block_id": None,
            "warm_cache_uncertainty": "Shared provider cache is uncontrolled; earlier arms may warm later prefixes.",
            "transport": None, "source_thread_sha256": None,
            "continuation_thread_sha256": None,
            "custom_adoption": "new_owned_thread" if arm == "custom_early" else None,
            "elapsed_seconds": None, "completed_task_turns": 0,
            "usage_coverage_complete": False, "usage_missing_response_count": None,
            "cache_write_reporting_supported": None, "native_compaction_observed": False,
            "subagent_count": 0, "limits": dict(LIMITS),
            "events": [], "responses": [], "artifact_verification": None,
            "status": "prepared", "stop_reason": None}


def nonnegative_integer(value):
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def summarize_receipt(receipt):
    errors = []
    arm = receipt.get("arm")
    if arm not in ARMS:
        errors.append("invalid_arm")
    seen, totals = set(), {key: 0 for key in USAGE_KEYS}
    missing = {key: 0 for key in USAGE_KEYS}
    phase_totals = {}
    for response in receipt.get("responses", []):
        key = response.get("response_id")
        if not key or key in seen:
            errors.append("missing_or_duplicate_response_id")
        seen.add(key)
        if response.get("usage_basis") != "per_response_delta":
            errors.append("usage_must_be_deduplicated_delta")
        phase = response.get("phase")
        if phase not in ("pre_intervention", "compaction", "post_intervention", "recovery"):
            errors.append("invalid_response_phase")
        phase_totals.setdefault(phase, {k: 0 for k in USAGE_KEYS})
        usage = response.get("usage", {})
        for metric in USAGE_KEYS:
            value = usage.get(metric)
            if value is None:
                missing[metric] += 1
            elif not nonnegative_integer(value):
                errors.append("invalid_usage_" + metric)
            else:
                totals[metric] += value
                phase_totals[phase][metric] += value
        input_tokens, cached = usage.get("input_tokens"), usage.get("cached_input_tokens")
        if nonnegative_integer(input_tokens) and nonnegative_integer(cached) and cached > input_tokens:
            errors.append("cached_exceeds_input")
        written = usage.get("cache_write_input_tokens")
        if all(nonnegative_integer(x) for x in (input_tokens, cached, written)) and cached + written > input_tokens:
            errors.append("cache_read_and_write_exceed_input")
        output, reasoning = usage.get("output_tokens"), usage.get("reasoning_output_tokens")
        if nonnegative_integer(output) and nonnegative_integer(reasoning) and reasoning > output:
            errors.append("reasoning_exceeds_output")
    events = receipt.get("events", [])
    decisions = [e for e in events if e.get("type") == "policy_decision"]
    dispatches = [e for e in events if e.get("type") == "dispatch"]
    outcomes = [e for e in events if e.get("type") == "outcome"]
    if arm == "baseline" and dispatches:
        errors.append("baseline_has_intervention")
    if arm in ("native_early", "custom_early") and receipt.get("status") == "complete":
        if len(dispatches) != 1 or len(outcomes) != 1 or len(decisions) != 1:
            errors.append("intervention_receipt_count")
        else:
            decision, dispatch, outcome = decisions[0], dispatches[0], outcomes[0]
            if not decision.get("decision_id") or dispatch.get("decision_id") != decision["decision_id"]:
                errors.append("decision_dispatch_not_correlated")
            if not dispatch.get("operation_id") or outcome.get("operation_id") != dispatch["operation_id"]:
                errors.append("dispatch_outcome_not_correlated")
            if dispatch.get("after_completed_turn") != 3 or outcome.get("status") != "succeeded":
                errors.append("intervention_boundary_or_outcome")
            expected_action = "native_compact" if arm == "native_early" else "new_thread_inject"
            if dispatch.get("action") != expected_action:
                errors.append("wrong_intervention_action")
    if arm == "custom_early" and receipt.get("status") == "complete":
        source, target = receipt.get("source_thread_sha256"), receipt.get("continuation_thread_sha256")
        if not source or not target or source == target or receipt.get("custom_adoption") != "new_owned_thread":
            errors.append("custom_requires_distinct_owned_continuation")
    if receipt.get("status") == "complete" and receipt.get("completed_task_turns") != 6:
        errors.append("incomplete_task_turns")
    for key, value in (("max_provider_responses", len(receipt.get("responses", []))),
                       ("max_compaction_dispatches", len(dispatches)),
                       ("max_total_input_tokens", totals["input_tokens"]),
                       ("max_total_output_tokens", totals["output_tokens"]),
                       ("max_seconds", receipt.get("elapsed_seconds")),
                       ("max_subagents", receipt.get("subagent_count"))):
        if value is None or not isinstance(value, (int, float)) or isinstance(value, bool) or value < 0:
            errors.append("missing_or_invalid_limit_measurement_" + key)
        elif value > LIMITS[key]:
            errors.append("limit_exceeded_" + key)
    core_missing = any(missing[k] for k in ("input_tokens", "cached_input_tokens", "output_tokens"))
    complete_usage = (receipt.get("usage_coverage_complete") is True
                      and receipt.get("usage_missing_response_count") == 0
                      and not core_missing and bool(seen))
    if arm == "native_early" and receipt.get("status") == "complete" and not any(
            r.get("phase") == "compaction" for r in receipt.get("responses", [])):
        complete_usage = False
    if arm == "baseline" and receipt.get("native_compaction_observed"):
        errors.append("baseline_native_compaction_contamination")
    safe_totals = {k: None if missing[k] else totals[k] for k in USAGE_KEYS}
    cached_fraction = (totals["cached_input_tokens"] / totals["input_tokens"]
                       if complete_usage and totals["input_tokens"] else None)
    quality = receipt.get("artifact_verification") or {}
    if quality.get("passed") and quality.get("fixture_sha256") != receipt.get("fixture_sha256"):
        errors.append("quality_fixture_mismatch")
    return {"schema": SCHEMA, "arm": arm, "errors": sorted(set(errors)),
            "receipt_valid": not errors, "quality_passed": quality.get("passed") is True,
            "usage_coverage_complete": complete_usage, "response_count": len(seen),
            "totals": safe_totals, "missing_measurements": missing,
            "phase_observed_totals": phase_totals,
            "cached_input_fraction": cached_fraction,
            "non_cached_input_tokens": totals["input_tokens"] - totals["cached_input_tokens"]
                if complete_usage else None,
            "eligible_for_token_comparison": not errors and complete_usage and quality.get("passed") is True,
            "dollar_savings": None, "quota_savings": None,
            "note": "Cached input is included in input; reasoning output is included in output. Missing is not zero. Per-phase sums are observed subtotals only."}


def compare_receipts(receipts):
    """Only identical seeded workloads/configurations qualify for a descriptive comparison."""
    summaries = [summarize_receipt(r) for r in receipts]
    errors = []
    if sorted(r.get("arm", "") for r in receipts) != sorted(ARMS):
        errors.append("requires_each_arm_exactly_once")
    for field in ("schema", "fixture_sha256", "seed", "model", "reasoning_effort", "codex_version",
                  "gobstopper_sha256", "tool_schema_sha256", "instructions_sha256", "transport", "workspace_sha256", "block_id"):
        values = {canonical(r.get(field)) for r in receipts}
        if len(values) != 1 or any(r.get(field) is None for r in receipts):
            errors.append("mismatched_or_missing_" + field)
    if sorted(r.get("execution_order") or 0 for r in receipts) != [1, 2, 3]:
        errors.append("missing_or_duplicate_execution_order")
    if not all(s["eligible_for_token_comparison"] for s in summaries):
        errors.append("one_or_more_arms_not_qualified")
    return {"schema": SCHEMA, "comparable": not errors, "errors": errors,
            "arms": summaries, "interpretation": "One matched synthetic block; descriptive token counts only. No production, price, quota, or quality generalization."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare = commands.add_parser("prepare")
    prepare.add_argument("--output", type=Path, required=True)
    prepare.add_argument("--seed", type=int, default=20260921)
    prepare.add_argument("--ballast-rows", type=int, default=384)
    prepare.add_argument("--model", required=True)
    prepare.add_argument("--effort", required=True)
    verify = commands.add_parser("verify")
    verify.add_argument("--fixture", type=Path, required=True)
    verify.add_argument("--artifact", type=Path, required=True)
    summary = commands.add_parser("summarize")
    summary.add_argument("--receipt", type=Path, required=True)
    compare = commands.add_parser("compare")
    compare.add_argument("--receipt", type=Path, action="append", required=True)
    args = parser.parse_args()
    if args.command == "prepare":
        if args.output.exists():
            parser.error("output already exists; preserve prior trial inputs")
        fixture = make_fixture(args.seed, args.ballast_rows)
        args.output.mkdir(parents=True)
        for name, value in [("fixture.json", fixture), ("oracle-do-not-send.json", expected_artifact(fixture)),
                            *[(arm + "-receipt.json", receipt_template(fixture, arm, args.model, args.effort)) for arm in ARMS]]:
            (args.output / name).write_text(json.dumps(value, indent=2) + "\n")
        (args.output / "records").mkdir()
        for turn in fixture["turns"]:
            record = turn["tool_result"]
            (args.output / "records" / (record["result_id"] + ".json")).write_text(json.dumps(record, indent=2) + "\n")
        print(canonical({"schema": SCHEMA, "fixture_sha256": digest(fixture),
                         "seed": args.seed, "turns": 6, "public_synthetic": True,
                         "fixture_bytes": len(canonical(fixture).encode()), "provider_calls": 0}))
    elif args.command == "verify":
        result = verify_artifact(json.loads(args.fixture.read_text()), json.loads(args.artifact.read_text()))
        print(json.dumps(result, indent=2))
        raise SystemExit(0 if result["passed"] else 1)
    elif args.command == "summarize":
        result = summarize_receipt(json.loads(args.receipt.read_text()))
        print(json.dumps(result, indent=2))
        raise SystemExit(0 if result["receipt_valid"] else 1)
    else:
        result = compare_receipts([json.loads(path.read_text()) for path in args.receipt])
        print(json.dumps(result, indent=2))
        raise SystemExit(0 if result["comparable"] else 1)


if __name__ == "__main__":
    main()

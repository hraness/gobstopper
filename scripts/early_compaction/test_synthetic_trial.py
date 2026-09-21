import copy
import unittest

import synthetic_trial as trial


def completed_receipt(arm="baseline"):
    fixture = trial.make_fixture()
    receipt = trial.receipt_template(fixture, arm, "synthetic-test-model", "test-effort")
    receipt.update(codex_version="test", gobstopper_sha256="a" * 64, transport="app_server",
                   workspace_sha256="d" * 64, block_id="test-block", execution_order=trial.ARMS.index(arm) + 1,
                   elapsed_seconds=12, completed_task_turns=6, status="complete",
                   usage_coverage_complete=True, usage_missing_response_count=0,
                   source_thread_sha256="b" * 64,
                   continuation_thread_sha256="c" * 64 if arm == "custom_early" else "b" * 64,
                   artifact_verification=trial.verify_artifact(fixture, trial.expected_artifact(fixture)))
    for turn in range(1, 7):
        receipt["responses"].append({"response_id": str(turn), "turn": turn,
            "phase": "pre_intervention" if turn <= 3 else "post_intervention",
            "usage_basis": "per_response_delta", "usage": {
                "input_tokens": 100, "cached_input_tokens": 60, "output_tokens": 20,
                "reasoning_output_tokens": 5, "cache_write_input_tokens": None}})
    if arm != "baseline":
        receipt["events"] = [
            {"type": "policy_decision", "decision_id": "decision-1"},
            {"type": "dispatch", "decision_id": "decision-1", "operation_id": "operation-1",
             "after_completed_turn": 3, "action": "native_compact" if arm == "native_early" else "new_thread_inject"},
            {"type": "outcome", "operation_id": "operation-1", "status": "succeeded"}]
    if arm == "native_early":
        receipt["responses"].append({"response_id": "compact", "phase": "compaction",
            "usage_basis": "per_response_delta", "usage": {"input_tokens": 200,
            "cached_input_tokens": 80, "output_tokens": 50, "reasoning_output_tokens": 0,
            "cache_write_input_tokens": None}})
    return receipt


class FixtureTests(unittest.TestCase):
    def test_seed_reproducibility_and_variation(self):
        self.assertEqual(trial.make_fixture(1), trial.make_fixture(1))
        self.assertNotEqual(trial.digest(trial.make_fixture(1)), trial.digest(trial.make_fixture(2)))

    def test_tool_results_link_to_requests(self):
        fixture = trial.make_fixture()
        ids = []
        for turn in fixture["turns"]:
            result_id = turn["tool_result"]["result_id"]
            ids.append(result_id)
            self.assertEqual(turn["expected_tool"]["arguments"]["result_id"], result_id)
            self.assertEqual(turn["injected_items"][0]["call_id"], turn["injected_items"][1]["call_id"])
        self.assertEqual(len(ids), len(set(ids)))
        self.assertIn("historical_sensor_rows", fixture["turns"][0]["tool_result"])

    def test_all_seeded_oracles_feasible_and_exact_demand(self):
        for seed in range(20):
            fixture = trial.make_fixture(seed)
            artifact = trial.expected_artifact(fixture)
            self.assertTrue(trial.verify_artifact(fixture, artifact)["passed"])
            demands = fixture["turns"][3]["tool_result"]["units_by_sku"]
            for sku, count in demands.items():
                self.assertEqual(count, sum(r["units"] for r in artifact["allocations"] if r["sku"] == sku))
            self.assertFalse(any(r["depot"] == "north" for r in artifact["allocations"]))

    def test_reject_stale_price_capacity_source_and_wrong_total(self):
        fixture = trial.make_fixture()
        for field, value in (("unit_cost_cents", 1000), ("units", 1000), ("price_source", "inventory-v0")):
            artifact = trial.expected_artifact(fixture)
            artifact["allocations"][0][field] = value
            self.assertFalse(trial.verify_artifact(fixture, artifact)["passed"])
        artifact = trial.expected_artifact(fixture)
        artifact["total_cost_cents"] += 1
        self.assertFalse(trial.verify_artifact(fixture, artifact)["passed"])

    def test_reject_nonobject_extra_fields_and_revoked_depot(self):
        fixture = trial.make_fixture()
        self.assertFalse(trial.verify_artifact(fixture, [])["passed"])
        artifact = trial.expected_artifact(fixture)
        artifact["extra"] = True
        self.assertFalse(trial.verify_artifact(fixture, artifact)["passed"])
        artifact = trial.expected_artifact(fixture)
        artifact["allocations"][0]["depot"] = "north"
        self.assertFalse(trial.verify_artifact(fixture, artifact)["passed"])

    def test_growth_bounded(self):
        with self.assertRaises(ValueError):
            trial.make_fixture(ballast_rows=385)
        self.assertLess(len(trial.canonical(trial.make_fixture(ballast_rows=384)).encode()), 196608)


class ReceiptTests(unittest.TestCase):
    def test_qualified_complete_arms_and_normalized_counts(self):
        for arm in trial.ARMS:
            summary = trial.summarize_receipt(completed_receipt(arm))
            self.assertTrue(summary["eligible_for_token_comparison"], summary)
            self.assertIsNone(summary["totals"]["cache_write_input_tokens"])
        summary = trial.summarize_receipt(completed_receipt())
        self.assertEqual(summary["totals"]["input_tokens"], 600)
        self.assertEqual(summary["non_cached_input_tokens"], 240)
        self.assertEqual(summary["cached_input_fraction"], .6)

    def test_compaction_cost_included(self):
        summary = trial.summarize_receipt(completed_receipt("native_early"))
        self.assertEqual(summary["totals"]["input_tokens"], 800)
        self.assertEqual(summary["totals"]["output_tokens"], 170)

    def test_native_missing_compaction_usage_incomparable(self):
        receipt = completed_receipt("native_early")
        receipt["responses"].pop()
        self.assertFalse(trial.summarize_receipt(receipt)["eligible_for_token_comparison"])

    def test_missing_usage_remains_null(self):
        receipt = completed_receipt()
        receipt["responses"][0]["usage"]["cached_input_tokens"] = None
        summary = trial.summarize_receipt(receipt)
        self.assertIsNone(summary["totals"]["cached_input_tokens"])
        self.assertIsNone(summary["non_cached_input_tokens"])
        self.assertFalse(summary["eligible_for_token_comparison"])

    def test_reject_duplicate_response_and_cumulative_basis(self):
        receipt = completed_receipt()
        receipt["responses"].append(copy.deepcopy(receipt["responses"][0]))
        self.assertIn("missing_or_duplicate_response_id", trial.summarize_receipt(receipt)["errors"])
        receipt = completed_receipt()
        receipt["responses"][0]["usage_basis"] = "cumulative"
        self.assertIn("usage_must_be_deduplicated_delta", trial.summarize_receipt(receipt)["errors"])

    def test_reject_invalid_usage_and_subsets(self):
        for value in (-1, True, 1.5):
            receipt = completed_receipt()
            receipt["responses"][0]["usage"]["input_tokens"] = value
            self.assertFalse(trial.summarize_receipt(receipt)["receipt_valid"])
        receipt = completed_receipt()
        receipt["responses"][0]["usage"]["cache_write_input_tokens"] = 41
        self.assertIn("cache_read_and_write_exceed_input", trial.summarize_receipt(receipt)["errors"])

    def test_no_silent_retries_or_wrong_boundary(self):
        receipt = completed_receipt("native_early")
        receipt["events"][1]["after_completed_turn"] = 4
        self.assertFalse(trial.summarize_receipt(receipt)["receipt_valid"])
        receipt = completed_receipt("native_early")
        receipt["events"][2]["operation_id"] = "unrelated"
        self.assertFalse(trial.summarize_receipt(receipt)["receipt_valid"])
        receipt["events"].append(copy.deepcopy(receipt["events"][1]))
        self.assertFalse(trial.summarize_receipt(receipt)["receipt_valid"])

    def test_custom_cannot_mutate_source_thread(self):
        receipt = completed_receipt("custom_early")
        receipt["continuation_thread_sha256"] = receipt["source_thread_sha256"]
        self.assertIn("custom_requires_distinct_owned_continuation", trial.summarize_receipt(receipt)["errors"])

    def test_baseline_native_compaction_contaminates_control(self):
        receipt = completed_receipt()
        receipt["native_compaction_observed"] = True
        self.assertFalse(trial.summarize_receipt(receipt)["eligible_for_token_comparison"])

    def test_limits_fail_closed(self):
        for field, value in (("elapsed_seconds", 601), ("subagent_count", 1), ("completed_task_turns", 5)):
            receipt = completed_receipt()
            receipt[field] = value
            self.assertFalse(trial.summarize_receipt(receipt)["receipt_valid"])

    def test_quality_fixture_identity_required(self):
        receipt = completed_receipt()
        receipt["artifact_verification"]["fixture_sha256"] = "wrong"
        self.assertFalse(trial.summarize_receipt(receipt)["receipt_valid"])

    def test_comparison_requires_same_config_and_all_arms(self):
        receipts = [completed_receipt(arm) for arm in trial.ARMS]
        self.assertTrue(trial.compare_receipts(receipts)["comparable"])
        receipts[1]["model"] = "different"
        self.assertFalse(trial.compare_receipts(receipts)["comparable"])
        self.assertFalse(trial.compare_receipts(receipts[:2])["comparable"])


if __name__ == "__main__":
    unittest.main()

import copy
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    'provider_retention_probe_under_test', Path(__file__).with_name('provider-retention-probe.py'))
PROBE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROBE)
UNSET = object()
FACTS = {
    'migration_allowed': False,
    'pending_task': 'verify rollback',
    'test_command': 'cargo test --workspace --locked',
    'receipt_code': 'COBALT_31415',
}


class ProviderRetentionProbeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.case = 0

    def tearDown(self):
        self.temp.cleanup()

    def run_probe(self, answer=UNSET, *, style='constraints', channel='result',
                  raw_answer=None, envelopes=None, raw_envelopes=None, costs=None,
                  boundary='valid', failure=None, auth_code=0):
        self.case += 1
        output = self.root / f'probe-{self.case}'
        if answer is UNSET:
            answer = dict(FACTS, rules=list(PROBE.RULE_MARKERS.get(style, ())))
        answer = copy.deepcopy(answer)
        calls = []

        def fake_command(argv, stdout, stderr, environment, **kwargs):
            label = stdout.stem
            calls.append(label)
            self.assertEqual(kwargs['cwd'], output / 'workspace')
            self.assertEqual(environment['CLAUDE_CONFIG_DIR'], str(output / 'claude-home'))
            if failure and failure[0] == label:
                raise RuntimeError(failure[1])
            if label == 'auth':
                self.assertEqual(argv[1:], ['auth', 'status'])
                response = {'loggedIn': True}
            else:
                self.assertEqual(argv[argv.index('--tools') + 1], '')
                self.assertEqual(argv[argv.index('--mcp-config') + 1], '{"mcpServers":{}}')
                self.assertEqual(argv[argv.index('--max-budget-usd') + 1], '0.25')
                if style != 'claude_md':
                    self.assertIn('--safe-mode', argv)
                sid_flag = '--session-id' if label == 'step-0' else '--resume'
                sid = argv[argv.index(sid_flag) + 1]
                prompt = kwargs['stdin'].read().decode()
                response = {'session_id': sid, 'total_cost_usd': .01}
                if costs is not None:
                    cost = costs[int(label[-1])]
                    if cost is UNSET:
                        del response['total_cost_usd']
                    else:
                        response['total_cost_usd'] = cost
                if label == 'step-1' and boundary != 'absent':
                    self.assertEqual(prompt, '/compact')
                    target = output / 'claude-home' / 'projects' / 'synthetic'
                    target.mkdir(parents=True)
                    transcript = (json.dumps({'type': 'system', 'subtype': 'compact_boundary'}) + '\n'
                                  if boundary == 'valid' else boundary)
                    (target / f'{sid}.jsonl').write_text(transcript)
                if label == 'step-2':
                    response[channel] = (raw_answer if raw_answer is not None else json.dumps(answer)) if channel == 'result' else answer
            if envelopes and label in envelopes:
                override = envelopes[label]
                if isinstance(override, dict):
                    response.update(override)
                else:
                    response = override
            raw = raw_envelopes[label] if raw_envelopes and label in raw_envelopes else json.dumps(response)
            stdout.write_text(raw)
            stderr.write_text('')
            return auth_code if label == 'auth' else 0

        argv = ['probe', '--claude-bin', sys.executable, '--output', str(output),
                '--seed-style', style, '--allow-provider-calls']
        # Every invocation uses synthetic files in this test's fresh directory.
        # Popen is independently forbidden even if the command patch regresses.
        with patch.object(sys, 'argv', argv), patch.dict(os.environ, {}, clear=True), \
                patch.object(PROBE.RUNNER, 'command', side_effect=fake_command), \
                patch.object(PROBE.RUNNER.subprocess, 'Popen', side_effect=AssertionError('provider process forbidden')) as spawn, \
                patch('builtins.print'):
            previous_umask = os.umask(0o077)
            try:
                code = PROBE.main()
            finally:
                os.umask(previous_umask)
            spawn.assert_not_called()
        result_path = output / 'result.json'
        result = json.loads(result_path.read_text())
        registration = json.loads((output / 'registration.json').read_text())
        self.assertEqual(set(result['check_results']), set(registration['checks']))
        self.assertIsNone(result['task_success'])
        self.assertIsNone(result['semantic_equivalence'])
        self.assertIsNone(result['behavioral_enforcement'])
        self.assertEqual(result_path.stat().st_mode & 0o777, 0o600)
        return code, result, registration, calls

    def test_all_seed_styles_pass_only_their_registered_checks(self):
        for style in PROBE.SEEDS:
            with self.subTest(style=style):
                code, result, registration, calls = self.run_probe(style=style)
                self.assertEqual(code, 0)
                self.assertEqual(result['status'], 'synthetic_literal_checks_passed')
                self.assertTrue(result['registered_checks_passed'])
                self.assertTrue(result['execution_complete'])
                self.assertEqual(set(result['check_results'].values()), {'passed'})
                self.assertEqual(result['recall_checks_passed'], 4)
                self.assertEqual(result['constraint_rules_expected'], len(PROBE.RULE_MARKERS.get(style, ())))
                self.assertEqual(registration['scoring_version'], 2)
                self.assertEqual(calls, ['auth', 'step-0', 'step-1', 'step-2'])

    def test_missing_markers_fail_despite_correct_facts(self):
        for style in PROBE.RULE_MARKERS:
            with self.subTest(style=style):
                code, result, _, _ = self.run_probe(dict(FACTS, rules=[]), style=style)
                self.assertEqual(code, 1)
                self.assertEqual(result['status'], 'recall_failed')
                self.assertEqual(result['recall_checks_passed'], 4)
                self.assertEqual(result['constraint_rules_recalled'], 0)
                self.assertEqual(result['check_results']['constraint_rules_recalled'], 'failed')
                self.assertFalse(result['registered_checks_passed'])
                self.assertTrue(result['execution_complete'])

    def test_each_rule_marker_is_required_and_recorded(self):
        for missing in PROBE.RULE_MARKERS['constraints']:
            with self.subTest(missing=missing):
                rules = [marker for marker in PROBE.RULE_MARKERS['constraints'] if marker != missing]
                code, result, _, _ = self.run_probe(dict(FACTS, rules=rules))
                self.assertEqual(code, 1)
                self.assertEqual(result['constraint_rules_recalled'], 3)
                self.assertEqual(result['rule_marker_checks'][missing], 'failed')

    def test_constraints_markers_match_the_four_declared_rules(self):
        rules = ['approval', 'services/billing', 'payments worker', 'cargo test']
        code, result, registration, _ = self.run_probe(dict(FACTS, rules=rules))
        self.assertEqual(code, 0)
        self.assertEqual(registration['rule_markers'], rules)
        self.assertEqual(result['constraint_rules_recalled'], 4)
        # Pending rollback is separately scored as a fact; it cannot replace
        # recalling the fourth rule's required test command in the rules array.
        code, result, _, _ = self.run_probe(dict(FACTS, rules=rules[:3] + ['rollback']))
        self.assertEqual(code, 1)
        self.assertEqual(result['rule_marker_checks']['cargo test'], 'failed')
        self.assertEqual(result['check_results']['pending_task'], 'passed')

    def test_markers_are_case_insensitive_within_one_rule(self):
        code, result, _, _ = self.run_probe(dict(FACTS, rules=[marker.upper() for marker in PROBE.RULE_MARKERS['constraints']]))
        self.assertEqual(code, 0)
        self.assertEqual(result['constraint_rules_recalled'], 4)
        code, result, _, _ = self.run_probe(dict(FACTS, rules=['approval', 'services/billing', 'payments', 'worker', 'rollback']))
        self.assertEqual(code, 1)
        self.assertEqual(result['rule_marker_checks']['payments worker'], 'failed')

    def test_invalid_rules_cannot_be_coerced_into_lexical_success(self):
        for rules in (None, 'approval services/billing payments worker rollback',
                      {'approval': 'services/billing payments worker rollback'},
                      1, True, ['approval', ['services/billing'], 'payments worker', 'rollback']):
            with self.subTest(rules=rules):
                code, result, _, _ = self.run_probe(dict(FACTS, rules=rules))
                self.assertEqual(code, 1)
                self.assertEqual(result['status'], 'invalid_recall_response')
                self.assertEqual(result['recall_checks_passed'], 4)
                self.assertIsNone(result['constraint_rules_recalled'])
                self.assertEqual(result['check_results']['constraint_rules_recalled'], 'invalid_response')
                self.assertEqual(set(result['rule_marker_checks'].values()), {'invalid_response'})
        _, result, _, _ = self.run_probe(FACTS)
        self.assertEqual(result['check_results']['constraint_rules_recalled'], 'invalid_response')

    def test_non_object_answers_always_save_invalid_results(self):
        for channel in ('result', 'structured_output'):
            for answer in ([], [1], '', 'text', 0, 1, False, True, None):
                with self.subTest(channel=channel, answer=answer):
                    code, result, _, _ = self.run_probe(answer, channel=channel)
                    self.assertEqual(code, 1)
                    self.assertEqual(result['status'], 'invalid_recall_response')
                    self.assertEqual(result['recall_answer_status'], 'non_object_json')
                    self.assertFalse(result['recall_answer_json'])
                    self.assertIsNone(result['recall_checks_passed'])
                    self.assertEqual(result['check_results']['native_boundary_persisted'], 'passed')
                    self.assertEqual(result['check_results']['pending_task'], 'invalid_response')
                    self.assertTrue(result['execution_complete'])
                    self.assertTrue(result['cost_complete'])

    def test_malformed_duplicate_and_nonfinite_answer_json_is_invalid(self):
        correct_with_overflow = json.dumps(dict(FACTS, rules=list(PROBE.RULE_MARKERS['constraints'])))[:-1] + ',"extra":1e999}'
        for raw in ('not json', '{', '{"migration_allowed":true,"migration_allowed":false}',
                    '{"migration_allowed":NaN}', '{"extra":1e999}',
                    '{"extra":-1e999}', correct_with_overflow, '[' * 2000):
            with self.subTest(raw=raw[:60]):
                code, result, _, _ = self.run_probe(raw_answer=raw)
                self.assertEqual(code, 1)
                self.assertEqual(result['recall_answer_status'], 'invalid_json')
                self.assertIsNone(result['recall_checks_passed'])

    def test_structured_object_and_complete_json_fence_are_supported(self):
        answer = dict(FACTS, rules=list(PROBE.RULE_MARKERS['constraints']))
        for kwargs in ({'channel': 'structured_output'}, {'raw_answer': '```json\n' + json.dumps(answer) + '\n```'}):
            with self.subTest(kwargs=kwargs):
                code, result, _, _ = self.run_probe(answer, **kwargs)
                self.assertEqual(code, 0)
                self.assertEqual(result['recall_answer_status'], 'valid')
        for raw in ('```json\n' + json.dumps(answer), '```text\n' + json.dumps(answer) + '\n```'):
            code, result, _, _ = self.run_probe(raw_answer=raw)
            self.assertEqual(code, 1)
            self.assertEqual(result['recall_answer_status'], 'invalid_json_fence')

    def test_explicit_invalid_structured_answer_cannot_fall_through(self):
        for answer in ([], None, 'text'):
            with self.subTest(answer=answer):
                code, result, _, _ = self.run_probe(answer, channel='structured_output',
                                                  envelopes={'step-2': {'result': json.dumps(FACTS)}})
                self.assertEqual(code, 1)
                self.assertEqual(result['recall_answer_status'], 'non_object_json')

    def test_invalid_result_field_types_save_a_result(self):
        for value in (None, [], {}, True, 1):
            with self.subTest(value=value):
                code, result, _, _ = self.run_probe(envelopes={'step-2': {'result': value}})
                self.assertEqual(code, 1)
                self.assertEqual(result['recall_answer_status'], 'missing_or_non_string_result')

    def test_fact_types_missingness_and_null_unknown_are_distinct(self):
        for check, (field, _) in PROBE.FACT_CHECKS.items():
            for value in (UNSET, [], {}, 0, True if field != 'migration_allowed' else 'false'):
                with self.subTest(field=field, value=value):
                    answer = dict(FACTS)
                    if value is UNSET:
                        del answer[field]
                    else:
                        answer[field] = value
                    code, result, _, _ = self.run_probe(answer, style='baseline')
                    self.assertEqual(code, 1)
                    self.assertEqual(result['check_results'][check], 'invalid_response')
                    self.assertEqual(result['status'], 'invalid_recall_response')
            answer = dict(FACTS)
            answer[field] = None
            code, result, _, _ = self.run_probe(answer, style='baseline')
            self.assertEqual(code, 1)
            self.assertEqual(result['check_results'][check], 'invalid_response' if field == 'migration_allowed' else 'failed')

    def test_lenient_fact_count_cannot_make_overall_success(self):
        answer = dict(FACTS, pending_task='please verify rollback first', test_command='run cargo test --workspace --locked')
        code, result, _, _ = self.run_probe(answer, style='baseline')
        self.assertEqual(code, 1)
        self.assertEqual(result['recall_checks_lenient'], 4)
        self.assertEqual(result['recall_checks_passed'], 2)
        self.assertEqual(result['status'], 'recall_failed')

    def test_valid_wrong_values_are_scored_as_zero_recall(self):
        answer = {'migration_allowed': True, 'pending_task': None,
                  'test_command': 'cargo test', 'receipt_code': 'wrong'}
        code, result, _, _ = self.run_probe(answer, style='baseline')
        self.assertEqual(code, 1)
        self.assertEqual(result['status'], 'recall_failed')
        self.assertEqual(result['recall_answer_status'], 'valid')
        self.assertEqual(result['recall_checks_passed'], 0)
        self.assertEqual({result['check_results'][name] for name in PROBE.FACT_CHECKS}, {'failed'})

    def test_non_object_envelopes_at_every_step_persist_failure(self):
        for label in ('auth', 'step-0', 'step-1', 'step-2'):
            for response in ([], [1], 'text', 1, True, None):
                with self.subTest(label=label, response=response):
                    code, result, _, calls = self.run_probe(envelopes={label: response})
                    self.assertEqual(code, 1)
                    self.assertEqual(result['status'], f'{label}_invalid_response')
                    self.assertEqual(calls[-1], label)
                    self.assertFalse(result['execution_complete'])
                    self.assertFalse(result['cost_complete'])
                    self.assertIsNone(result['recall_checks_passed'])

    def test_malformed_envelopes_persist_failure_without_answer_scoring(self):
        for raw in ('{', '{"loggedIn":true,"loggedIn":false}', '{"x":Infinity}',
                    '{"loggedIn":true,"extra":1e999}', '[' * 2000):
            with self.subTest(raw=raw[:60]):
                code, result, _, _ = self.run_probe(raw_envelopes={'auth': raw})
                self.assertEqual(code, 1)
                self.assertEqual(result['status'], 'auth_invalid_json')
                self.assertEqual(result['provider_commands'], 0)

    def test_auth_requires_a_boolean_and_blocks_before_model_calls(self):
        for logged_in in ('true', 1, [], None):
            with self.subTest(logged_in=logged_in):
                code, result, _, calls = self.run_probe(envelopes={'auth': {'loggedIn': logged_in}})
                self.assertEqual(code, 1)
                self.assertEqual(result['status'], 'auth_invalid_response')
                self.assertEqual(calls, ['auth'])
        for exit_code in (0, 1):
            code, result, _, calls = self.run_probe(envelopes={'auth': {'loggedIn': False}}, auth_code=exit_code)
            self.assertEqual(code, 1)
            self.assertEqual(result['status'], 'blocked_isolated_authentication')
            self.assertEqual(calls, ['auth'])
            self.assertFalse(result['cost_complete'])
            self.assertEqual(set(result['check_results'].values()), {'not_evaluated'})

    def test_provider_error_session_mismatch_and_timeout_do_not_score_recall(self):
        cases = [({'envelopes': {'step-1': {'is_error': True}}}, 'step-1_provider_error'),
                 ({'envelopes': {'step-1': {'is_error': 'false'}}}, 'step-1_invalid_response'),
                 ({'envelopes': {'step-1': {'session_id': 'other'}}}, 'provider_session_identity_mismatch'),
                 ({'failure': ('step-1', 'command_timeout')}, 'command_timeout')]
        for kwargs, expected in cases:
            with self.subTest(status=expected):
                code, result, _, calls = self.run_probe(**kwargs)
                self.assertEqual(code, 1)
                self.assertEqual(result['status'], expected)
                self.assertEqual(calls, ['auth', 'step-0', 'step-1'])
                self.assertFalse(result['cost_complete'])
                self.assertIsNone(result['recall_checks_passed'])

    def test_boundary_failure_or_malformed_history_stops_before_recall(self):
        for boundary in ('absent', '[]\n', '{broken\n', '{"subtype":"compact_boundary"}\n[]\n'):
            with self.subTest(boundary=boundary):
                code, result, _, calls = self.run_probe(boundary=boundary)
                self.assertEqual(code, 1)
                self.assertEqual(calls, ['auth', 'step-0', 'step-1'])
                self.assertEqual(result['status'], 'native_compaction_not_materialized' if boundary == 'absent' else 'native_transcript_invalid')
                self.assertEqual(result['check_results']['native_boundary_persisted'], 'failed' if boundary == 'absent' else 'invalid_response')
                self.assertIsNone(result['recall_checks_passed'])
                self.assertFalse(result['execution_complete'])

    def test_only_a_system_record_can_establish_native_boundary(self):
        for record in ({'subtype': 'compact_boundary'},
                       {'type': 'assistant', 'subtype': 'compact_boundary'},
                       {'type': 'user', 'subtype': 'compact_boundary'}):
            with self.subTest(record=record):
                code, result, _, calls = self.run_probe(boundary=json.dumps(record) + '\n')
                self.assertEqual(code, 1)
                self.assertEqual(result['status'], 'native_compaction_not_materialized')
                self.assertFalse(result['native_boundary_persisted'])
                self.assertEqual(result['check_results']['native_boundary_persisted'], 'failed')
                self.assertEqual(calls, ['auth', 'step-0', 'step-1'])

    def test_missing_or_invalid_cost_is_independent_of_recall(self):
        for cost in (UNSET, None, False, -1, '.01', {}, 10 ** 400):
            with self.subTest(cost=str(cost)[:40]):
                code, result, _, _ = self.run_probe(costs=[.01, cost, .02])
                self.assertEqual(code, 0)
                self.assertTrue(result['registered_checks_passed'])
                self.assertFalse(result['cost_complete'])
                self.assertEqual(result['reported_cost_samples'], 2)
                self.assertAlmostEqual(result['reported_cost_usd'], .03)

    def test_cost_zero_is_observed_and_accumulation_cannot_overflow(self):
        code, result, _, _ = self.run_probe(costs=[0, 0.0, 0])
        self.assertEqual(code, 0)
        self.assertTrue(result['cost_complete'])
        self.assertEqual(result['reported_cost_samples'], 3)
        self.assertEqual(result['reported_cost_usd'], 0)
        code, result, _, _ = self.run_probe(costs=[1e308, 1e308, 0])
        self.assertEqual(code, 0)
        self.assertFalse(result['cost_complete'])
        self.assertEqual(result['reported_cost_samples'], 2)
        self.assertEqual(result['reported_cost_usd'], 1e308)


if __name__ == '__main__':
    unittest.main()

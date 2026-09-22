import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


RUNNER = load('compaction_study', 'compaction-study.py')
PROBE = load('provider_probe', 'provider-retention-probe.py')


class StudyRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def test_expired_study_never_spawns(self):
        with patch.object(RUNNER, 'DEADLINE', time.monotonic() - 1), patch.object(RUNNER.subprocess, 'Popen') as spawn:
            with self.assertRaisesRegex(RuntimeError, 'study_timeout'):
                RUNNER.command(['unused'], self.root/'out', self.root/'err', {})
            spawn.assert_not_called()

    def test_output_limit_is_checked_even_after_fast_exit(self):
        with patch.object(RUNNER, 'LIMIT', 64):
            with self.assertRaisesRegex(RuntimeError, 'output_limit'):
                RUNNER.command([sys.executable, '-c', 'print("x"*128)'], self.root/'out', self.root/'err', dict(os.environ))

    def test_timeout_reaps_only_owned_child(self):
        with self.assertRaisesRegex(RuntimeError, 'command_timeout'):
            RUNNER.command([sys.executable, '-c', 'import time; time.sleep(30)'], self.root/'out', self.root/'err', dict(os.environ), timeout=.03)

    def test_existing_output_is_never_clobbered_or_executed(self):
        path = self.root/'out'
        path.write_text('keep')
        with patch.object(RUNNER.subprocess, 'Popen') as spawn:
            with self.assertRaises(FileExistsError):
                RUNNER.command(['unused'], path, self.root/'err', {})
            spawn.assert_not_called()
        self.assertEqual(path.read_text(), 'keep')

    def probe(self, logged_in, materialize):
        output = self.root/'probe'
        invocations = []
        def fake(argv, stdout, stderr, environment, **kwargs):
            invocations.append(argv)
            if argv[1:3] == ['auth', 'status']:
                response = {'loggedIn': logged_in}
            else:
                self.assertIn('--safe-mode', argv)
                self.assertEqual(argv[argv.index('--tools')+1], '')
                self.assertEqual(argv[argv.index('--max-budget-usd')+1], '0.25')
                self.assertEqual(Path(environment['CLAUDE_CONFIG_DIR']).parent, output)
                prompt = kwargs['stdin'].read().decode()
                if prompt == '/compact' and materialize:
                    sid = argv[-1]
                    target = Path(environment['CLAUDE_CONFIG_DIR'])/'projects'/'test'
                    target.mkdir(parents=True)
                    (target/f'{sid}.jsonl').write_text(json.dumps({'type':'system','subtype':'compact_boundary'})+'\n')
                response = {'session_id': argv[-1], 'total_cost_usd': .01, 'result': json.dumps({'migration_allowed': False, 'pending_task':'verify rollback', 'test_command':'cargo test --workspace --locked', 'receipt_code':'COBALT_31415'})}
            stdout.write_text(json.dumps(response))
            stderr.write_text('')
            return 1 if argv[1:3] == ['auth', 'status'] and not logged_in else 0
        arguments = ['probe', '--claude-bin', sys.executable, '--output', str(output), '--allow-provider-calls']
        with patch.object(sys, 'argv', arguments), patch.object(PROBE.RUNNER, 'command', side_effect=fake), patch('builtins.print'):
            previous_umask = os.umask(0o077)
            try:
                code = PROBE.main()
            finally:
                os.umask(previous_umask)
        return code, json.loads((output/'result.json').read_text()), invocations

    def test_missing_isolated_auth_stops_before_model_call(self):
        code, result, calls = self.probe(False, False)
        self.assertEqual(code, 1)
        self.assertEqual(result['status'], 'blocked_isolated_authentication')
        self.assertEqual(result['provider_commands'], 0)
        self.assertEqual(len(calls), 1)

    def test_ack_without_persisted_boundary_is_not_success(self):
        code, result, _ = self.probe(True, False)
        self.assertEqual(code, 1)
        self.assertEqual(result['status'], 'native_compaction_not_materialized')
        self.assertEqual(result['provider_commands'], 2)
        self.assertIsNone(result['recall_checks_passed'])

    def test_materialized_compaction_and_recall_are_separate_checks(self):
        code, result, _ = self.probe(True, True)
        self.assertEqual(code, 0)
        self.assertTrue(result['native_boundary_persisted'])
        self.assertEqual(result['recall_checks_passed'], 4)
        self.assertEqual(result['provider_commands'], 3)


if __name__ == '__main__':
    unittest.main()

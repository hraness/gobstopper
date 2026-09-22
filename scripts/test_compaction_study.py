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
AUDIT = load('retention_audit', 'retention-audit.py')


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

    def test_audit_pairs_by_marker_increase_and_surgery(self):
        def entry(ts, strategy, session='s1', path='/Users/x/.codex/sessions/r.jsonl', sha=None):
            return {'provider': 'codex', 'session_id': session, 'path': path, 'ts': ts,
                    'strategy': strategy, 'bytes': 1, 'sha256': sha or f'{ts:064x}'}
        # Marker counts per sha: compactions land between snapshots regardless
        # of how the hook labels line up.
        markers = {f'{ts:064x}': count for ts, count in
                   [(1, 0), (2, 0), (3, 1), (4, 1), (5, 1), (6, 2), (7, 2), (8, 2)]}
        skipped = []
        read = lambda sha: markers.get(sha, 0) * b'"type":"compacted"\n'
        entries = [
            entry(1, 'pre-compact'),
            entry(2, 'post-compact'),   # no increase: bracket missed the write
            entry(3, 'pre-compact'),   # +1 marker here -> provider_native (1->2? no: 2->3)
            entry(4, 'elide'),         # surgery label, flat marker -> surgery_next
            entry(5, 'pre-compact'),
            entry(6, 'post-compact'),  # +1 marker at 5->6 -> provider_native
            entry(7, 'pre-undo'),
            entry(8, 'post-compact'),
        ]
        result = AUDIT.pairs(entries, read, skipped)
        kinds = sorted((p['kind'], p['before']['ts'], p['marker_before'], p['marker_after'])
                       for p in result)
        self.assertEqual(kinds, [
            ('provider_native', 2, 0, 1),
            ('provider_native', 5, 1, 2),
            ('surgery_next', 4, 1, 1),
        ])
        self.assertFalse(any(p['before']['ts'] in (1, 3, 7, 8) for p in result))
        # Test-fixture paths and oversized/index-filtered entries are excluded.
        tmp = [entry(9, 'pre-compact', path='/tmp/fixture.jsonl'),
               entry(10, 'post-compact', path='/tmp/fixture.jsonl')]
        self.assertEqual(AUDIT.pairs(tmp, read, skipped), [])
        big = entry(11, 'pre-compact', sha=f'{11:064x}')
        big['bytes'] = AUDIT.MAX_TRANSIT_BYTES + 1
        AUDIT.pairs([big, entry(12, 'post-compact')], read, skipped)
        self.assertEqual(skipped[-1]['reason'], 'index_filtered')
        # Per-session cap keeps the newest pairs.
        many = [entry(ts, 'auto' if ts % 2 else 'post-compact') for ts in range(1, 13)]
        self.assertEqual(len(AUDIT.pairs(many, read, skipped)), AUDIT.MAX_PAIRS_PER_SESSION)

    def test_rollup_aggregates_ok_rows_only(self):
        rt = {'total': 10, 'retained': 5, 'lexical_retained': 7,
              'same_origin_retained': 5, 'source_bound_retained': 2}
        rows = [
            {'provider': 'codex', 'status': 'ok', 'retention': dict(rt)},
            {'provider': 'codex', 'status': 'ok', 'retention': dict(rt)},
            {'provider': 'devin', 'status': 'ok', 'retention': dict(rt)},
            {'provider': 'codex', 'status': 'failed'},
            {'provider': 'codex', 'status': 'skipped:store_unreadable'},
        ]
        out = AUDIT.rollup(rows)
        self.assertEqual(set(out), {'codex', 'devin'})
        self.assertEqual(out['codex'], {'pairs': 2, 'checks': 20, 'retained': 10,
                                        'lexical_retained': 14, 'same_origin_retained': 10,
                                        'source_bound_retained': 4})
        self.assertEqual(out['devin']['pairs'], 1)

    def test_seed_styles_carry_identical_facts(self):
        for style, seed in PROBE.SEEDS.items():
            facts = ('verify rollback', 'COBALT_31415', 'pending')
            if style != 'pinned':  # pinned keeps its rules in CLAUDE.md only
                facts += ('production migration', 'NOT been granted',
                          'cargo test --workspace --locked')
            for fact in facts:
                self.assertIn(fact, seed, style)
            self.assertIn('build observation unchanged\n' * 10, seed)
        for marker in ('production migration', 'NOT been granted', 'payments worker',
                       'services/billing', 'cargo test --workspace --locked'):
            self.assertIn(marker, PROBE.PINNED_RULES_MD)
        for marker in ('synthetic memory test', 'Reply only READY', 'Remember these facts'):
            self.assertNotIn(marker.lower(), PROBE.SEEDS['naturalistic'].lower())

    def test_materialized_compaction_and_recall_are_separate_checks(self):
        code, result, _ = self.probe(True, True)
        self.assertEqual(code, 0)
        self.assertTrue(result['native_boundary_persisted'])
        self.assertEqual(result['recall_checks_passed'], 4)
        self.assertEqual(result['provider_commands'], 3)


if __name__ == '__main__':
    unittest.main()

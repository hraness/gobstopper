import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time

LIMIT = 64 * 1024 * 1024
DEADLINE = float('inf')


def sha(data):
    return hashlib.sha256(data).hexdigest()


def save(path, value):
    with path.open('x', encoding='utf-8') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def command(argv, output, error, environment, timeout=240, stdin=None, cwd=None):
    if time.monotonic() >= DEADLINE:
        raise RuntimeError('study_timeout')
    with output.open('xb') as stdout, error.open('xb') as stderr:
        child = subprocess.Popen(argv, stdin=stdin if stdin is not None else subprocess.DEVNULL, stdout=stdout,
                                 stderr=stderr, env=environment, cwd=cwd, start_new_session=True)
        deadline = min(DEADLINE, time.monotonic() + timeout)
        try:
            while child.poll() is None:
                if time.monotonic() >= deadline:
                    raise RuntimeError('command_timeout')
                if os.fstat(stdout.fileno()).st_size + os.fstat(stderr.fileno()).st_size > LIMIT:
                    raise RuntimeError('output_limit')
                time.sleep(.05)
            if os.fstat(stdout.fileno()).st_size + os.fstat(stderr.fileno()).st_size > LIMIT:
                raise RuntimeError('output_limit')
            return child.returncode
        finally:
            if child.poll() is None:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                child.wait()


def summarize(path):
    if path.stat().st_size > 4 * 1024 * 1024:
        raise RuntimeError('report_limit')
    report = json.loads(path.read_text())
    rows = []
    for case in report['cases']:
        row = {key: case[key] for key in ('case', 'session', 'success', 'failure', 'source_verify_errors') if key in case}
        row['arms'] = [{key: result[key] for key in ('arm', 'status', 'applied_rounds', 'estimated_context_before', 'estimated_context_after', 'verify_errors', 'new_verify_errors') if key in result} | {
            'retention': {key: result['retention'][key] for key in ('total', 'source_bound_retained', 'elidable_total', 'elidable_retained', 'by_kind')}
        } for result in case.get('checkpoints', []) if result['round'] == 1]
        rows.append(row)
    return {'schema': 'gobstopper-retention-pilot-summary-v1', 'cases': rows, 'binary_unchanged': report['binary_unchanged'], 'provider_calls': 0}


def main():
    global DEADLINE
    DEADLINE = time.monotonic() + 900
    parser = argparse.ArgumentParser(description='Freeze selected session exports and run an offline typed-retention pilot; no model calls.')
    parser.add_argument('--report', type=Path, help='Read only: emit a bounded metadata summary of an existing results.json')
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--session', action='append')
    parser.add_argument('--rounds', type=int, default=10, choices=range(1, 11))
    parser.add_argument('--floor', type=int, default=40000)
    args = parser.parse_args()
    if args.report:
        print(json.dumps(summarize(args.report), indent=2))
        return 0
    if not args.binary or not args.output or not args.session:
        parser.error('--binary, --output, and --session are required for a new study')
    if not 1 <= len(args.session) <= 8 or len(set(args.session)) != len(args.session) or args.floor < 0:
        parser.error('supply 1..8 distinct sessions and a nonnegative floor')
    os.umask(0o077)
    output = args.output.expanduser().absolute()
    output.mkdir(mode=0o700, exist_ok=False)
    binary_bytes = args.binary.expanduser().resolve(strict=True).read_bytes()
    binary = output / 'gobstopper'
    with binary.open('xb') as stream:
        stream.write(binary_bytes)
    binary.chmod(0o700)
    config = output / 'config/gobstopper'
    config.mkdir(mode=0o700, parents=True)
    (config / 'config.toml').write_text('[policy]\nadaptive=false\nkeep_recent_tool_outputs=8\nmin_savings_tokens=0\n')
    environment = {k: v for k, v in os.environ.items() if not k.startswith('GOBSTOPPER_')}
    isolated = dict(environment, XDG_CONFIG_HOME=str(output / 'config'), XDG_DATA_HOME=str(output / 'data'))
    registration = {
        'schema': 'gobstopper-retention-pilot-v1', 'registered_before_outcomes': True,
        'selection': args.session, 'selection_rule': 'Explicit operator-selected convenience sample; no population inference.',
        'binary_sha256': sha(binary_bytes), 'runner_sha256': sha(Path(__file__).read_bytes()),
        'rounds': args.rounds, 'trigger_tokens': 1, 'floor_tokens': args.floor,
        'keep_recent_tool_outputs': 8, 'label_source': 'heuristic',
        'candidate_rule': 'At most 16 complete lines per kind; elidable records first, then source order. No outcome-dependent selection.',
        'provider_calls': 0, 'limitations': ['Static replay, not interleaved agent work.', 'Heuristic labels are not audited truth.', 'No provider compaction, semantic summary, task-success or billed-savings measurement.'],
    }
    save(output / 'registration.json', registration)
    (output / 'runner.py').write_bytes(Path(__file__).read_bytes())
    cases = []
    for index, session in enumerate(args.session):
        root = output / f'case-{index}'
        root.mkdir(mode=0o700)
        source = root / 'source.jsonl'
        case = {'case': index, 'session': session}
        try:
            code = command([str(binary), 'export', session], source, root / 'export.err', environment)
            if code != 0:
                raise RuntimeError('export_failed')
            data = source.read_bytes()
            case['source_sha256'] = sha(data)
            case['source_bytes'] = len(data)
            code = command([str(binary), 'eval-study', str(source), '--prepare-manifest', str(root / 'manifest.json')],
                           root / 'prepare.json', root / 'prepare.err', isolated)
            if code != 0:
                raise RuntimeError('annotation_preparation_failed')
            case['manifest_sha256'] = sha((root / 'manifest.json').read_bytes())
            case['prepared'] = True
        except (OSError, RuntimeError) as error:
            case['prepared'] = False
            case['failure'] = str(error) if isinstance(error, RuntimeError) else 'io_failure'
        cases.append(case)
    save(output / 'cases.json', cases)
    results = []
    for case in cases:
        result = dict(case)
        if case['prepared']:
            root = output / f'case-{case["case"]}'
            source = root / 'source.jsonl'
            manifest = root / 'manifest.json'
            try:
                if sha(source.read_bytes()) != case['source_sha256'] or sha(manifest.read_bytes()) != case['manifest_sha256']:
                    raise RuntimeError('input_changed')
                code = command([str(binary), 'eval-study', str(source), '--manifest', str(manifest),
                                '--rounds', str(args.rounds), '--trigger', '1', '--floor', str(args.floor), '--json'],
                               root / 'report.json', root / 'study.err', isolated, timeout=300)
                if code != 0:
                    raise RuntimeError('study_failed')
                report = json.loads((root / 'report.json').read_text())
                result['checkpoints'] = [row for row in report['rows'] if row['round'] in (1, 5, 10)]
                result['source_unchanged'] = sha(source.read_bytes()) == case['source_sha256']
                result['manifest_unchanged'] = sha(manifest.read_bytes()) == case['manifest_sha256']
                result['source_verify_errors'] = report['source_verify_errors']
                result['structurally_clean'] = report['source_verify_errors'] == 0 and all(row['verify_errors'] == 0 for row in report['rows'])
                result['success'] = result['source_unchanged'] and result['manifest_unchanged'] and all(row['new_verify_errors'] == 0 for row in report['rows'])
            except (OSError, ValueError, RuntimeError) as error:
                result['success'] = False
                result['failure'] = str(error) if isinstance(error, RuntimeError) else 'invalid_result'
        results.append(result)
    save(output / 'results.json', {'registration': registration, 'cases': results,
         'binary_unchanged': sha(binary.read_bytes()) == registration['binary_sha256']})
    print(json.dumps({'cases': len(cases), 'completed': sum(r.get('success', False) for r in results),
                      'provider_calls': 0, 'results': str(output / 'results.json')}))
    return 0 if all(r.get('success') for r in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())

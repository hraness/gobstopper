import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import time

SPEC = importlib.util.spec_from_file_location('study_runner', Path(__file__).with_name('compaction-study.py'))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)

# Snapshot strategy labels that mark a gobstopper transcript surgery.
SURGERY = frozenset({
    'auto', 'preset:auto', 'elide', 'structured', 'cache_aware', 'scored',
    'compacted', 'compacted-preserve-order', 'gobstopper-compacted',
    'claude-autocompact',
})
SKIP_PREFIXES = ('/tmp/', '/var/', '/private/tmp/', '/private/var/')
# Provider-native compaction leaves an explicit record-level marker. Counting
# it per snapshot is timing-robust: hook-bracket labels (pre-/post-compact)
# do not always straddle the actual mutation.
MARKER = {
    'claude_code': b'compact_boundary',
    'codex': b'"type":"compacted"',
    # Devin /compact lands a summary node with metadata.summarized_from set
    # to the node it summarizes (int); unrelated nodes carry null.
    'devin': re.compile(rb'"summarized_from":\d'),
}
MAX_TRANSIT_BYTES = 512 * 1024 * 1024  # mirrors transaction::max_transcript_bytes
MAX_PAIRS_PER_SESSION = 3
MAX_PAIRS = 24


def real(entry):
    return not entry['path'].startswith(SKIP_PREFIXES)


def vault_reader(root):
    """Reconstruct snapshot bytes exactly as vault::read_object does:
    hash-checked manifest + chunks, with a legacy objects/ fallback."""
    def read(sha):
        manifest = root / 'manifests' / sha
        if manifest.is_file():
            raw = manifest.read_bytes()
            if hashlib.sha256(raw).hexdigest() != sha:
                return None
            try:
                meta = json.loads(raw)
            except ValueError:
                return None
            data = b''
            for chunk_sha in meta.get('chunks', []):
                chunk = (root / 'chunks' / chunk_sha).read_bytes()
                if hashlib.sha256(chunk).hexdigest() != chunk_sha:
                    return None
                data += chunk
            if len(data) != meta.get('bytes', len(data)):
                return None
            return data
        obj = root / 'objects' / sha
        if obj.is_file():
            raw = obj.read_bytes()
            return raw if hashlib.sha256(raw).hexdigest() == sha else None
        return None
    return read


def index_visible(entry):
    """Mirror vault::read_index filtering so `vault:` specs resolve the same."""
    return (len(entry.get('sha256', '')) == 64
            and len(entry.get('session_id', '')) <= 256
            and len(entry.get('strategy') or '') <= 128
            and entry.get('bytes', 0) <= MAX_TRANSIT_BYTES)


def signature(provider, data):
    marker = MARKER.get(provider)
    if marker is None:
        return None
    return len(marker.findall(data)) if hasattr(marker, 'findall') else data.count(marker)


def pairs(entries, read, skipped):
    """Compaction pairs per session, newest-first and per-session capped.

    provider_native: consecutive snapshots whose compaction-marker count
    increases — a real provider compaction landed between them, regardless
    of how the surrounding hook labels line up.
    surgery_next: a surgery-labeled snapshot paired with the next snapshot.
    """
    by_session = {}
    for position, entry in enumerate(entries):
        if real(entry) and index_visible(entry):
            by_session.setdefault((entry['provider'], entry['session_id']), []).append((position, entry))
        elif real(entry):
            skipped.append({'sha256': entry['sha256'], 'session': entry['session_id'],
                            'reason': 'index_filtered'})
    out = []
    for (provider, session), seq in by_session.items():
        seq.sort(key=lambda item: (item[1]['ts'], item[0]))
        marker = MARKER.get(provider)
        sigs = [None] * len(seq)
        if marker is not None:
            for i, (_, entry) in enumerate(seq):
                data = read(entry['sha256'])
                if data is None:
                    skipped.append({'sha256': entry['sha256'], 'session': session,
                                    'reason': 'snapshot_unreadable'})
                else:
                    sigs[i] = signature(provider, data)
        taken = 0
        for index in range(len(seq) - 1, -1, -1):
            if taken >= MAX_PAIRS_PER_SESSION:
                break
            before = seq[index][1]
            after = seq[index + 1][1] if index + 1 < len(seq) else None
            kind = None
            if after is not None and sigs[index] is not None and sigs[index + 1] is not None:
                if sigs[index + 1] > sigs[index]:
                    kind = 'provider_native'
            if kind is None and before['strategy'] in SURGERY and after is not None:
                kind = 'surgery_next'
            if kind:
                out.append({'kind': kind, 'provider': provider, 'session': session,
                            'before': before, 'after': after, 'after_label': after['strategy'],
                            'marker_before': sigs[index], 'marker_after': sigs[index + 1]})
                taken += 1
    out.sort(key=lambda p: (p['before']['ts'], p['before']['sha256']), reverse=True)
    return out


def classify(stderr_tail):
    if 'no vault entry' in stderr_tail:
        return 'skipped:index_filtered'
    if 'exceeds 64 MiB' in stderr_tail:
        return 'skipped:source_too_large'
    if 'not a database' in stderr_tail or 'export' in stderr_tail:
        return 'skipped:store_unreadable'
    return 'failed'


def main():
    parser = argparse.ArgumentParser(
        description='Realized retention audit over vault snapshot pairs; no provider calls, no writes outside --output.')
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--vault-root', type=Path,
                        default=Path(os.environ.get('XDG_DATA_HOME', Path.home() / '.local/share')) / 'gobstopper/vault')
    parser.add_argument('--limit', type=int, default=MAX_PAIRS)
    args = parser.parse_args()
    if not 1 <= args.limit <= MAX_PAIRS:
        parser.error(f'--limit must be 1..{MAX_PAIRS}')
    os.umask(0o077)
    root = args.output.expanduser().absolute()
    root.mkdir(mode=0o700, exist_ok=False)
    binary = args.binary.expanduser().resolve(strict=True)
    vault_root = args.vault_root.expanduser().absolute()
    index_path = vault_root / 'index.jsonl'
    entries = []
    for line in index_path.read_text().splitlines():
        try:
            entries.append(json.loads(line))
        except ValueError:
            pass
    environment = {k: v for k, v in os.environ.items() if not k.startswith('GOBSTOPPER_')}
    registration = {
        'schema': 'gobstopper-retention-audit-v1', 'registered_before_outcomes': True,
        'vault_index_sha256': RUNNER.sha(index_path.read_bytes()),
        'index_entries': len(entries), 'binary_sha256': RUNNER.sha(binary.read_bytes()),
        'runner_sha256': RUNNER.sha(Path(__file__).read_bytes()),
        'pair_rule': 'consecutive snapshots whose compaction-marker count increases are provider_native; surgery labels pair with the next snapshot; newest-first, <=3 per session',
        'provider_calls': 0,
        'limitations': ['Heuristic labels on the before-state only; retention is conditional on annotation coverage.',
                        'Consecutive snapshots can include intervening growth; a dropped check is only attributable when nothing else rewrote the transcript between them.',
                        'Provider-native compaction replaces records wholesale: source_bound is expected to be 0; retained/same_origin carry the signal.',
                        'Devin store snapshots can exceed the index byte bound or arrive torn (live WAL store); they are excluded, not repaired.'],
    }
    RUNNER.save(root / 'registration.json', registration)
    RUNNER.DEADLINE = time.monotonic() + 600
    skipped = []
    selected = pairs(entries, vault_reader(vault_root), skipped)[:args.limit]
    results = []
    for index, pair in enumerate(selected):
        case = root / f'pair-{index}'
        case.mkdir(mode=0o700)
        row = {'pair': index, 'kind': pair['kind'], 'provider': pair['provider'],
               'session': pair['session'], 'before_sha256': pair['before']['sha256'],
               'after_sha256': pair['after']['sha256'], 'after_label': pair['after_label'],
               'before_ts': pair['before']['ts'], 'after_ts': pair['after']['ts'],
               'before_strategy': pair['before']['strategy'], 'before_bytes': pair['before']['bytes'],
               'marker_before': pair['marker_before'], 'marker_after': pair['marker_after']}
        before_spec = f"vault:{pair['before']['sha256']}"
        after_spec = f"vault:{pair['after']['sha256']}"
        try:
            code = RUNNER.command([str(binary), 'eval-study', before_spec, '--prepare-manifest',
                                   str(case / 'manifest.json')], case / 'prepare.json', case / 'prepare.err',
                                  environment)
            if code != 0:
                row['status'] = classify((case / 'prepare.err').read_text()[-500:])
                results.append(row)
                continue
            row['checks'] = len(json.loads((case / 'manifest.json').read_text()).get('checks', []))
            code = RUNNER.command([str(binary), 'eval-study', before_spec, '--manifest',
                                   str(case / 'manifest.json'), '--against', after_spec, '--json'],
                                  case / 'audit.json', case / 'audit.err', environment)
            if code != 0:
                row['status'] = classify((case / 'audit.err').read_text()[-500:])
                results.append(row)
                continue
            report = json.loads((case / 'audit.json').read_text())
            detail = report['rows'][0]
            row.update({
                'estimated_context_before': detail['estimated_context_before'],
                'estimated_context_after': detail['estimated_context_after'],
                'retention': {k: detail['retention'][k] for k in
                              ('total', 'retained', 'lexical_retained', 'same_origin_retained',
                               'source_bound_retained', 'elidable_total', 'elidable_retained',
                               'by_kind')},
                'new_verify_errors': detail['new_verify_errors'],
                'status': 'ok',
            })
        except (OSError, ValueError) as error:
            row['status'] = 'io_or_parse_failure'
            row['detail'] = str(error)[:200]
        results.append(row)
    RUNNER.save(root / 'results.json', {'registration': registration, 'pairs': results,
                                      'skipped': skipped})
    ok = sum(r.get('status') == 'ok' for r in results)
    print(json.dumps({'pairs': len(results), 'ok': ok,
                      'skipped_or_failed': len(results) - ok + len(skipped),
                      'provider_calls': 0, 'results': str(root / 'results.json')}))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

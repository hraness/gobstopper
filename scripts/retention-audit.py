import argparse
import fcntl
import stat
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
}
MAX_TRANSIT_BYTES = 512 * 1024 * 1024  # mirrors transaction::max_transcript_bytes
MAX_PAIRS_PER_SESSION = 3
MAX_PAIRS = 24


def real(entry):
    return not entry['path'].startswith(SKIP_PREFIXES)


MAX_INDEX_BYTES = 128 * 1024 * 1024
MAX_RECORDS = 100_000
CHUNK_BYTES = 1024 * 1024


def digest(value):
    return isinstance(value, str) and re.fullmatch(r'[0-9a-fA-F]{64}', value) is not None


def strict_json(raw):
    def unique_fields(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError('duplicate JSON field')
            value[key] = item
        return value

    def invalid_constant(_):
        raise ValueError('unsupported JSON constant')

    # json.loads(bytes) also accepts UTF-16/32, unlike the Rust UTF-8 boundary.
    text = raw.decode('utf-8') if isinstance(raw, bytes) else raw
    return json.loads(text, object_pairs_hook=unique_fields, parse_constant=invalid_constant)


def bounded_read(path, limit):
    # Stable owner-controlled parents are assumed, as in the Rust reader.
    if path.parent.is_symlink():
        raise ValueError('object directory must not be a symlink')
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
            raise ValueError('invalid or oversized vault file')
        data = source.read(limit + 1)
        if len(data) > limit:
            raise ValueError('vault file exceeds bound')
        return data


class VaultReader:
    """Independent bounded decoder, sharing the Rust directory-flock protocol.

    The context holds custody through index selection and materialization; it
    never holds a lock while running evaluation subprocesses.
    """
    def __init__(self, root):
        self.root = Path(root)
        self.fd = None

    def __enter__(self):
        self.fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        deadline = time.monotonic() + 10
        try:
            while True:
                try:
                    fcntl.flock(self.fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
                    return self
                except BlockingIOError:
                    if time.monotonic() >= deadline:
                        raise TimeoutError('vault custody deadline')
                    time.sleep(.01)
        except BaseException:
            os.close(self.fd)
            self.fd = None
            raise

    def __exit__(self, *_):
        os.close(self.fd)
        self.fd = None

    def index(self):
        if self.fd is None:
            raise RuntimeError('vault reader requires custody')
        path = self.root / 'index.jsonl'
        # Cooperating publishers append under an index flock while sharing root
        # custody; read one bounded image and hash those exact bytes.
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, 'rb') as stream:
            if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                raise ValueError('invalid index')
            fcntl.flock(stream.fileno(), fcntl.LOCK_SH)
            raw = stream.read(MAX_INDEX_BYTES + 1)
        if len(raw) > MAX_INDEX_BYTES or raw and not raw.endswith(b'\n'):
            raise ValueError('oversized or incomplete index; repair required')
        entries = []
        for line in raw.split(b'\n')[:-1]:
            if not line or len(line) > 64 * 1024:
                raise ValueError('invalid index row; repair required')
            entry = strict_json(line)
            if not index_visible(entry):
                raise ValueError('invalid index root; repair required')
            entries.append(entry)
        return raw, entries

    def __call__(self, sha):
        if self.fd is None:
            raise RuntimeError('vault reader requires custody')
        try:
            return self._read(sha)
        except (OSError, ValueError, TypeError, KeyError, UnicodeError, RecursionError):
            return None

    def _read(self, sha):
        if not digest(sha):
            raise ValueError('invalid object digest')
        try:
            raw = bounded_read(self.root / 'manifests' / sha, 16 * 1024 * 1024)
        except FileNotFoundError:
            raw = bounded_read(self.root / 'objects' / sha, MAX_TRANSIT_BYTES)
            if hashlib.sha256(raw).hexdigest() != sha:
                raise ValueError('legacy object hash mismatch')
            return raw
        if hashlib.sha256(raw).hexdigest() != sha:
            raise ValueError('manifest hash mismatch')
        meta = strict_json(raw)
        if not isinstance(meta, dict):
            raise ValueError('manifest must be an object')
        version = meta.get('schema_version')
        if 'schema_version' in meta and (type(version) is not int or version not in (2, 3)):
            raise ValueError('unsupported manifest version')
        if version == 3:
            references = meta['chunks']
            expected_bytes = meta['bytes']
            if type(expected_bytes) is not int or not 0 <= expected_bytes <= MAX_TRANSIT_BYTES:
                raise ValueError('invalid byte count')
            folder, cap, count = 'chunks', CHUNK_BYTES, (MAX_TRANSIT_BYTES + CHUNK_BYTES - 1) // CHUNK_BYTES
        else:
            references = meta['records']
            folder, cap, count = 'records', MAX_TRANSIT_BYTES, MAX_RECORDS
            if type(meta.get('trailing_newline', False)) is not bool:
                raise ValueError('invalid newline flag')
        if not isinstance(references, list) or len(references) > count:
            raise ValueError('invalid object count')
        data = bytearray()
        for i, reference in enumerate(references):
            if not digest(reference):
                raise ValueError('invalid object reference')
            part = bounded_read(self.root / folder / reference, cap)
            if hashlib.sha256(part).hexdigest() != reference:
                raise ValueError('object hash mismatch')
            newline = version != 3 and (meta.get('trailing_newline', False) or i + 1 < len(references))
            if len(data) + len(part) + int(newline) > MAX_TRANSIT_BYTES:
                raise ValueError('snapshot exceeds byte bound')
            data.extend(part)
            if newline:
                data.extend(b'\n')
        if 'bytes' in meta and (type(meta['bytes']) is not int or len(data) != meta['bytes']):
            raise ValueError('byte count mismatch')
        source = meta.get('source_sha256')
        if version == 3 or 'source_sha256' in meta:
            if not digest(source) or hashlib.sha256(data).hexdigest() != source:
                raise ValueError('source hash mismatch')
        return bytes(data)


def vault_reader(root):
    return VaultReader(root)


def index_visible(entry):
    allowed = {'ts', 'sha256', 'path', 'session_id', 'provider', 'bytes', 'strategy', 'record_count', 'source_sha256'}
    return (isinstance(entry, dict) and not entry.keys() - allowed
            and digest(entry.get('sha256'))
            and isinstance(entry.get('path'), str)
            and isinstance(entry.get('session_id'), str) and 0 < len(entry['session_id'].encode()) <= 256
            and entry.get('provider') in MARKER
            and type(entry.get('ts')) is int and 0 <= entry['ts'] < 2 ** 64
            and (entry.get('strategy') is None or isinstance(entry['strategy'], str) and len(entry['strategy'].encode()) <= 128)
            and type(entry.get('bytes')) is int and 0 <= entry['bytes'] <= MAX_TRANSIT_BYTES
            and type(entry.get('record_count', 0)) is int and 0 <= entry.get('record_count', 0) <= MAX_RECORDS
            and (entry.get('source_sha256', '') == '' or digest(entry['source_sha256'])))


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
            by_session.setdefault((entry['provider'], entry['path'], entry['session_id']), []).append((position, entry))
        elif real(entry):
            skipped.append({'sha256': entry['sha256'], 'session': entry['session_id'],
                            'reason': 'index_filtered'})
    out = []
    for (provider, _path, session), seq in by_session.items():
        seq.sort(key=lambda item: item[0])
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
                out.append({'index_position': seq[index][0], 'kind': kind, 'provider': provider, 'session': session,
                            'before': before, 'after': after, 'after_label': after['strategy'],
                            'marker_before': sigs[index], 'marker_after': sigs[index + 1]})
                taken += 1
    out.sort(key=lambda p: p['index_position'], reverse=True)
    return out


def classify(stderr_tail):
    if 'no vault entry' in stderr_tail:
        return 'skipped:index_filtered'
    if 'exceeds 64 MiB' in stderr_tail:
        return 'skipped:source_too_large'
    if 'not a database' in stderr_tail or 'export' in stderr_tail:
        return 'skipped:store_unreadable'
    return 'failed'


def rollup(results):
    by_provider = {}
    for row in results:
        if row.get('status') != 'ok':
            continue
        agg = by_provider.setdefault(row['provider'], {
            'pairs': 0, 'checks': 0, 'retained': 0, 'lexical_retained': 0,
            'same_origin_retained': 0, 'source_bound_retained': 0})
        rt = row['retention']
        agg['pairs'] += 1
        for key in ('retained', 'lexical_retained', 'same_origin_retained',
                    'source_bound_retained'):
            agg[key] += rt[key]
        agg['checks'] += rt['total']
    return by_provider


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
    environment = {k: v for k, v in os.environ.items() if not k.startswith('GOBSTOPPER_')}
    config_home = root / 'config'
    config_home.mkdir(mode=0o700)
    environment['XDG_CONFIG_HOME'] = str(config_home)
    skipped = []
    RUNNER.DEADLINE = time.monotonic() + 600
    with vault_reader(vault_root) as reader:
        raw_index, entries = reader.index()
        selected = pairs(entries, reader, skipped)[:args.limit]
        # Materialize only the selected, verified bytes while custody is held.
        # Explicit file arguments cannot accidentally resolve the default vault.
        for index, pair in enumerate(selected):
            case = root / f'pair-{index}'
            case.mkdir(mode=0o700)
            for side in ('before', 'after'):
                data = reader(pair[side]['sha256'])
                if data is None:
                    raise ValueError('selected snapshot failed verification')
                (case / f'{side}.jsonl').write_bytes(data)
    registration = {
        'schema': 'gobstopper-retention-audit-v1', 'registered_before_outcomes': True,
        'vault_index_sha256': RUNNER.sha(raw_index),
        'index_entries': len(entries), 'binary_sha256': RUNNER.sha(binary.read_bytes()),
        'runner_sha256': RUNNER.sha(Path(__file__).read_bytes()),
        'pair_rule': 'consecutive snapshots whose compaction-marker count increases are provider_native; surgery labels pair with the next snapshot; reverse append order, <=3 per exact store/session',
        'provider_calls': 0,
        'limitations': ['Heuristic labels on the before-state only; retention is conditional on annotation coverage.',
                        'Consecutive snapshots can include intervening growth; a dropped check is only attributable when nothing else rewrote the transcript between them.',
                        'Provider-native compaction replaces records wholesale: source_bound is expected to be 0; retained/same_origin carry the signal.',
                        'Unsupported, corrupt or oversized snapshots are excluded.'],
    }
    RUNNER.save(root / 'registration.json', registration)
    results = []
    for index, pair in enumerate(selected):
        case = root / f'pair-{index}'
        row = {'pair': index, 'kind': pair['kind'], 'provider': pair['provider'],
               'session': pair['session'], 'before_sha256': pair['before']['sha256'],
               'after_sha256': pair['after']['sha256'], 'after_label': pair['after_label'],
               'before_ts': pair['before']['ts'], 'after_ts': pair['after']['ts'],
               'before_strategy': pair['before']['strategy'], 'before_bytes': pair['before']['bytes'],
               'marker_before': pair['marker_before'], 'marker_after': pair['marker_after']}
        before_spec = str(case / 'before.jsonl')
        after_spec = str(case / 'after.jsonl')
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
                                      'skipped': skipped, 'by_provider': rollup(results)})
    ok = sum(r.get('status') == 'ok' for r in results)
    print(json.dumps({'pairs': len(results), 'ok': ok,
                      'skipped_or_failed': len(results) - ok + len(skipped),
                      'provider_calls': 0, 'results': str(root / 'results.json')}))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

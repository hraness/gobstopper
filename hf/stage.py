#!/usr/bin/env python3
"""Validate and stage an explicit public artifact allowlist; never contacts the Hub."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import subprocess


REPO_ID = "hranesscom/gobstopper-benchmarks"
SOURCE_PREFIX = "site/public/benchmarks/"

def digest(data):
    return hashlib.sha256(data).hexdigest()


def checked_path(root, name):
    path = PurePosixPath(name)
    if not name or path.is_absolute() or any(x in ('.', '..') for x in name.split('/')) or '\\' in name:
        raise ValueError('unsafe relative path')
    target = root.joinpath(*path.parts)
    if any(root.joinpath(*path.parts[:n]).is_symlink() for n in range(1, len(path.parts) + 1)):
        raise ValueError('symlinks are not exportable')
    if not target.is_file() or not target.resolve().is_relative_to(root.resolve()):
        raise ValueError('missing or escaped artifact')
    return target


def validate(root):
    manifest = json.loads((root / 'hf/manifest.json').read_text())
    if manifest['schema'] != 'hraness-hf-export-v1' or manifest['repo_type'] != 'dataset':
        raise ValueError('unsupported manifest')
    if manifest['repo_id'] != REPO_ID:
        raise ValueError('wrong Hub destination')
    rows = manifest['files']
    if not rows:
        raise ValueError('empty export')
    outputs = {'README.md', 'export-manifest.json'}
    sources = set()
    checked = []
    for row in rows:
        source, destination = row['source'], row['destination']
        if not source.startswith(SOURCE_PREFIX) or source in sources:
            raise ValueError('source outside public allowlist or duplicated')
        if source[len(SOURCE_PREFIX):] != destination.removeprefix('data/') or not destination.startswith('data/'):
            raise ValueError('destination must preserve the public artifact path')
        if destination in outputs:
            raise ValueError('duplicate destination')
        outputs.add(destination)
        sources.add(source)
        data = checked_path(root, source).read_bytes()
        if digest(data) != row['sha256']:
            raise ValueError('artifact checksum changed: ' + source)
        if source.endswith('.json'):
            json.loads(data)
        elif not source.endswith('.md'):
            raise ValueError('only reviewed public JSON and Markdown are supported')
        checked.append((destination, data))
    card = checked_path(root, 'hf/README.md').read_bytes()
    return manifest, checked, card


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, help='new, nonexistent staging directory outside the repository')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    manifest, checked, card = validate(root)
    if args.output:
        output = args.output.absolute()
        if output.resolve().is_relative_to(root) or output.exists() or output.is_symlink():
            raise ValueError('output must be new and outside the repository')
        # Stage only a committed, reproducible source tree. Review before committing.
        status = subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=all'], cwd=root)
        if status:
            raise ValueError('commit reviewed changes before staging')
        commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
        export = dict(manifest, source_commit=commit, card_sha256=digest(card))
        output.mkdir(parents=True, exist_ok=False)
        for name, data in [('README.md', card), *checked]:
            target = output / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        (output / 'export-manifest.json').write_text(json.dumps(export, indent=2, sort_keys=True) + '\n')
    print(json.dumps({'repo_id': manifest['repo_id'], 'artifacts': len(checked), 'status': 'staged' if args.output else 'valid', 'publication': 'requires separate authorization and rights review'}))


if __name__ == '__main__':
    main()

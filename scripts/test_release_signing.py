"""Release credential boundaries and exact signed-artifact publication."""
import hashlib
import http.server
import json
from pathlib import Path
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import textwrap
import threading
import unittest

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = (ROOT / '.github/workflows/release.yml').read_text()


def job(name):
    match = re.search(r'^  ' + re.escape(name) + r':\n(.*?)(?=^  [a-z_]+:\n|\Z)', WORKFLOW, re.M | re.S)
    assert match, name
    return match.group(1)


def step_script(name, source):
    tail = source.split('      - name: ' + name + '\n', 1)[1]
    body = tail.split('        run: |\n', 1)[1]
    lines = []
    for line in body.splitlines():
        if line and not line.startswith('          '):
            break
        lines.append(line)
    return textwrap.dedent('\n'.join(lines))


class ReleaseSigningTests(unittest.TestCase):
    def test_stable_identity_matches_signer_and_installer(self):
        signing = (ROOT / 'scripts/sign-macos-release.py').read_text()
        installer = (ROOT / 'scripts/install.sh').read_text()
        self.assertEqual(re.search(r'^TEAM_ID = "([^"]+)"$', signing, re.M).group(1), '8AAP53VTW3')
        self.assertEqual(re.search(r'^IDENTIFIER = "([^"]+)"$', signing, re.M).group(1), 'dev.hraness.gobstopper')
        self.assertEqual(re.search(r"^  apple_team_id='([^']+)'$", installer, re.M).group(1), '8AAP53VTW3')
        self.assertEqual(re.search(r"^  apple_identifier='([^']+)'$", installer, re.M).group(1), 'dev.hraness.gobstopper')

    def test_secrets_only_enter_source_verified_signing_job(self):
        signing = job('macos_sign')
        self.assertIn('environment: hraness-apple-release', signing)
        self.assertIn('needs:\n      - identity\n      - verify\n      - macos_build', signing)
        for name in ('identity', 'verify', 'build', 'macos_build', 'publish', 'crates'):
            self.assertNotIn('secrets.APPLE_', job(name))
        self.assertNotIn('platform: darwin-aarch64', job('build'))
        self.assertIn('name: gobstopper-unsigned-darwin-aarch64-', job('macos_build'))
        self.assertNotIn('cargo ', signing)
        self.assertNotIn('build-release.sh', signing)
        self.assertNotIn('bun install', signing)
        self.assertIn('sign-macos-release.py extract-artifact', signing)
        self.assertIn('.workflow_run.head_sha == $sha', signing)
        self.assertIn('.digest == $digest', signing)
        cleanup = signing.index('sign-macos-release.py cleanup')
        smoke = signing.index('sh scripts/install.sh')
        attestation = signing.index('actions/attest-build-provenance@')
        self.assertLess(cleanup, smoke)
        self.assertLess(smoke, attestation)
        self.assertIn('if: always()', signing[:cleanup])

    def test_exact_source_requires_governed_main_ci_and_required_success(self):
        verify = job('verify')
        self.assertIn('git fetch --no-tags --unshallow origin "refs/heads/$DEFAULT_BRANCH:refs/remotes/origin/$DEFAULT_BRANCH"', verify)
        self.assertIn('git merge-base --is-ancestor "$VERIFIED_SHA"', verify)
        self.assertIn('actions/workflows/ci.yml/runs', verify)
        self.assertIn('-f head_sha="$VERIFIED_SHA" -f event=push', verify)
        self.assertIn('gh run watch "$run"', verify)
        self.assertIn('select(.name == "Required")', verify)
        self.assertIn('.[0].conclusion == "success"', verify)

    def test_publisher_uses_exact_signing_job_identity_and_hashes(self):
        signing = job('macos_sign')
        publication = job('publish')
        for field, source in (('artifact_id', 'signed.outputs.artifact-id'),
                              ('artifact_digest', 'signed.outputs.artifact-digest'),
                              ('archive_sha256', 'signed_bytes.outputs.archive_sha256'),
                              ('checksum_sha256', 'signed_bytes.outputs.checksum_sha256')):
            self.assertIn(field + ': ${{ steps.' + source + ' }}', signing)
            self.assertIn('${{ needs.macos_sign.outputs.' + field + ' }}', publication)
        self.assertIn('needs: [identity, verify, build, macos_sign]', publication)
        self.assertIn('ids=("$MACOS_ARTIFACT_ID")', publication)
        self.assertIn('for platform in linux-x86_64 linux-aarch64 windows-x86_64; do', publication)
        self.assertNotIn('pattern: release-*', publication)
        self.assertIn('.workflow_run.head_sha == $sha', publication)
        self.assertIn('.digest == $digest', publication)
        self.assertIn('path: ${{ runner.temp }}/gobstopper-apple-notarization.json', signing)
        self.assertIn('name: gobstopper-apple-notarization-', signing)

    def test_publisher_rejects_substituted_or_missing_signed_bytes(self):
        script = step_script('Require exact signed Mac archive and checksum bytes', job('publish'))
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'artifacts').mkdir()
            archive = root / 'artifacts/gobstopper-0.7.6-darwin-aarch64.tar.gz'
            checksum = Path(str(archive) + '.sha256')
            original = b'exact signed binary archive'
            recorded = (hashlib.sha256(original).hexdigest() + '  ' + archive.name + '\n').encode()
            environment = {'PATH': os.environ['PATH'], 'TAG': 'v0.7.6',
                'MACOS_ARCHIVE_SHA256': hashlib.sha256(original).hexdigest(),
                'MACOS_CHECKSUM_SHA256': hashlib.sha256(recorded).hexdigest()}
            def execute(**overrides):
                return subprocess.run(['/bin/bash', '-c', script], cwd=root, env={**environment, **overrides},
                                      capture_output=True, timeout=5).returncode
            archive.write_bytes(original)
            checksum.write_bytes(recorded)
            self.assertEqual(execute(), 0)
            self.assertNotEqual(execute(MACOS_ARCHIVE_SHA256=''), 0)
            self.assertNotEqual(execute(MACOS_CHECKSUM_SHA256=''), 0)
            archive.write_bytes(b'substitution')
            self.assertNotEqual(execute(), 0)
            checksum.write_text(hashlib.sha256(b'substitution').hexdigest() + '  ' + archive.name + '\n')
            self.assertNotEqual(execute(), 0)
            archive.write_bytes(original)
            checksum.write_bytes(recorded.rstrip())
            self.assertNotEqual(execute(), 0)


class CratesMetadataTests(unittest.TestCase):
    def test_registry_requests_identify_the_release_workflow(self):
        """Exercise preflight and post-publish HTTP using the actual step script."""
        with tempfile.TemporaryDirectory(prefix='gobstopper-crates-http-') as temporary:
            root = Path(temporary)
            version = '0.8.0'
            crates = ('gobstopper-core', 'gobstopper-adapters', 'gobstopper')
            checksums = {}
            for crate in crates:
                archive = root / f'target/package/{crate}-{version}.crate'
                archive.parent.mkdir(parents=True, exist_ok=True)
                notices = ['LICENSE-MIT', 'LICENSE-APACHE']
                if crate == 'gobstopper-adapters':
                    notices.append('THIRD_PARTY_NOTICES.md')
                with tarfile.open(archive, 'w:gz') as package:
                    for notice in notices:
                        package.add(ROOT / notice, arcname=f'{crate}-{version}/{notice}')
                        shutil.copyfile(ROOT / notice, root / notice)
                checksums[crate] = hashlib.sha256(archive.read_bytes()).hexdigest()
            fakebin = root / 'bin'
            fakebin.mkdir()
            cargo = fakebin / 'cargo'
            cargo.write_text('#!/bin/sh\nset -eu\noperation="$2"\n'
                'for crate do :; done\n'
                'case "$operation" in package) ;; publish) touch "$crate.published" ;; *) exit 9 ;; esac\n')
            cargo.chmod(0o755)
            requests = []

            class Registry(http.server.BaseHTTPRequestHandler):
                def log_message(self, *args):
                    pass

                def do_GET(self):
                    parts = self.path.removeprefix('/api/v1/crates/').split('/')
                    crate = parts[0]
                    if crate not in checksums or len(parts) not in (1, 2):
                        self.send_error(404)
                        return
                    published = (root / f'{crate}.published').exists()
                    phase = 'latest' if len(parts) == 1 else 'published' if published else 'absent'
                    requests.append((crate, phase, self.headers.get('User-Agent', '')))
                    if 'hraness/gobstopper' not in requests[-1][2]:
                        self.send_error(403, 'An identifying User-Agent is required')
                        return
                    if phase == 'absent':
                        self.send_error(404)
                        return
                    data = ({'crate': {'max_stable_version': '0.7.5'}} if phase == 'latest' else
                            {'version': {'crate': crate, 'num': version, 'yanked': False,
                                         'checksum': checksums[crate]}})
                    body = json.dumps(data).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                    self.send_header('Content-Length', str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)

            with http.server.ThreadingHTTPServer(('127.0.0.1', 0), Registry) as server:
                thread = threading.Thread(target=server.serve_forever, daemon=True)
                thread.start()
                try:
                    base = f'http://127.0.0.1:{server.server_address[1]}'
                    script = step_script('Publish the crates', job('crates')).replace('https://crates.io', base)
                    result = subprocess.run(['/bin/bash', '-c', script], cwd=root,
                        env={'PATH': str(fakebin) + os.pathsep + os.environ['PATH'],
                             'CURL_HOME': str(root), 'RUNNER_TEMP': str(root), 'GOBSTOPPER_VERSION': version},
                        capture_output=True, text=True, timeout=30)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    for crate in crates:
                        for phase in ('absent', 'latest', 'published'):
                            self.assertTrue(any(name == crate and state == phase and 'hraness/gobstopper' in agent
                                                for name, state, agent in requests), (crate, phase, requests))
                finally:
                    server.shutdown()
                    thread.join(timeout=5)


class PublishTagIdentityTests(unittest.TestCase):
    """Execute the actual final publication step against local Git and a fake gh."""
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='gobstopper-publish-tag-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / 'source'
        self.source.mkdir()
        self.environment = {**os.environ, 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull,
                            'GIT_AUTHOR_NAME': 'Fixture', 'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
                            'GIT_COMMITTER_NAME': 'Fixture', 'GIT_COMMITTER_EMAIL': 'fixture@example.invalid'}
        self.git('init', '-q', '-b', 'main', cwd=self.source)
        (self.source / 'scripts').mkdir()
        shutil.copyfile(ROOT / 'scripts/release_notes.py', self.source / 'scripts/release_notes.py')
        self.git('add', 'scripts/release_notes.py', cwd=self.source)
        self.git('commit', '-qm', 'reviewed source', cwd=self.source)
        self.sha = self.git('rev-parse', 'HEAD', cwd=self.source)
        self.git('tag', 'v0.7.6', cwd=self.source)
        self.git('tag', '-a', 'v0.7.7', '-m', 'annotated', cwd=self.source)
        self.checkout = self.root / 'checkout'
        self.git('clone', '-q', '--no-local', str(self.source), str(self.checkout), cwd=self.root)
        fakebin = self.root / 'bin'
        fakebin.mkdir()
        gh = fakebin / 'gh'
        gh.write_text('#!/bin/sh\n[ "$1" = release ] && [ "$2" = create ] || exit 9\nprintf called > "$GH_CALLED"\n')
        gh.chmod(0o755)
        self.environment['PATH'] = str(fakebin) + os.pathsep + self.environment['PATH']
        self.marker = self.root / 'gh-called'

    def git(self, *arguments, cwd):
        return subprocess.run(['git', *arguments], cwd=cwd, env=self.environment,
                              check=True, capture_output=True, text=True, timeout=15).stdout.strip()

    def publish(self, tag='v0.7.6', commit=None):
        return subprocess.run(['/bin/bash', '-c', step_script('Publish', job('publish'))],
                              cwd=self.checkout, env={**self.environment, 'TAG': tag,
                              'COMMIT': commit or self.sha, 'GITHUB_REPOSITORY': 'fixture/gobstopper',
                              'RUNNER_TEMP': str(self.root), 'GH_CALLED': str(self.marker)},
                              capture_output=True, text=True, timeout=30)

    def test_current_lightweight_and_annotated_tags_publish(self):
        for tag in ('v0.7.6', 'v0.7.7'):
            result = self.publish(tag)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(self.marker.exists())
            self.marker.unlink()

    def test_moved_tag_stops_before_any_release_mutation(self):
        (self.source / 'changed').write_text('different source')
        self.git('add', 'changed', cwd=self.source)
        self.git('commit', '-qm', 'unreviewed source', cwd=self.source)
        self.git('tag', '-f', 'v0.7.6', cwd=self.source)
        self.assertNotEqual(self.publish().returncode, 0)
        self.assertFalse(self.marker.exists())

    def test_missing_tag_or_invalid_identity_stops_before_release_mutation(self):
        for tag, commit in (('v0.7.8', self.sha), ('--upload-pack=unexpected', self.sha),
                            ('v0.7.6', 'not-a-commit')):
            self.assertNotEqual(self.publish(tag, commit).returncode, 0)
            self.assertFalse(self.marker.exists())


if __name__ == '__main__':
    unittest.main()

"""Release credential boundaries and exact signed-artifact publication."""
import hashlib
from pathlib import Path
import os
import re
import subprocess
import tempfile
import textwrap
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


if __name__ == '__main__':
    unittest.main()

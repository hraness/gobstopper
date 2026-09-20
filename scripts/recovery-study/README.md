# Reproduce the synthetic recovery checks

These standard-library Python scripts generate public synthetic vault snapshots and check Gobstopper's read-only recovery commands. Supply your own baseline and candidate executables. No binaries, private transcripts, credentials, or model responses are distributed with the scripts.

Run from the repository root with Python 3.9 or later on macOS or Linux:

```sh
python3 scripts/recovery-study/prepare.py --baseline /path/to/baseline/gobstopper --output /tmp/gobstopper-recovery-study
python3 /tmp/gobstopper-recovery-study/run.py /path/to/candidate/gobstopper
```

Choose a new output directory: preparation refuses any existing directory, file, or symlink. Use your host's required scheduler for the second command. Preparation only generates fixtures and copies the two scripts and baseline executable into the output directory; it never executes Gobstopper. The runner pins the candidate before its first command. Both executable hashes and the script/fixture hashes are recorded locally.

The generated directory contains:

- `registration.json`: the protocol fixed before outcomes, including limits and executable provenance.
- `manifest.json` and `fixtures/`: the predeclared queries, expected records and generated JSONL snapshots.
- `environment/`: an isolated config and synthetic v3 vault; original user sessions are never modified.
- `results.json` and `case-results.json`: aggregate and individual checks after the run. `started.json` prevents rerunning in the same directory; use a fresh directory for a new run.
- `bin/`: local pinned copies of the supplied executables. Keep this generated directory outside the repository; do not publish its binaries.

The 36 snapshots cover two providers, six synthetic families per provider, and three versions per family. Half encode Unicode as JSON escapes. Six place a fact's record across a vault chunk boundary. A separate malformed-record fixture checks error accounting. These are related test cases, not 36 independent real tasks.

Checks cover known-query search, version isolation, metadata-only search results, byte-exact record retrieval, bounded UTF-8 pagination, corruption rejection, state-card recall, and the default-off MCP transcript-content gate. Search operates on decoded JSON string values, not object keys. Empty physical lines and malformed JSON count as unsearchable; a trailing newline does not create another record. Baseline commands absent from an older executable are reported as unsupported, not as retrieval failures.

Execution is limited to 600 seconds overall, 20 seconds per command, 4 MiB of captured output per command, 2,200 commands, and 64 pages per retrieved record. The runner removes remote scorer/judge settings and uses an isolated configuration. It calls no model or provider service and reads no private corpus. The supplied executables must be trusted Gobstopper builds; these limits are not an operating-system sandbox for arbitrary binaries.

The queries and answers are known in advance. Passing establishes recovery API mechanics against these fixtures. It does not measure an agent's ability to select useful queries, semantic understanding, task quality, in-context retention, provider resume, billed savings, or general speed. Directly seeding the vault also does not test snapshot creation or compaction-to-recovery pointer wiring.

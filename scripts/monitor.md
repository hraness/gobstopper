# Local observation pass

`monitor.py` performs one read-only pass for explicit session IDs, with Codex
delta rows and bounded provider context samples:

```sh
python3 scripts/monitor.py \
  --binary /absolute/path/to/gobstopper \
  --output-dir /absolute/private/monitor-directory \
  --session 01a00000-aaaa-7000-aaaa-aaaaaaaaaaaa
```

Repeat `--session` for more exact native session IDs. At least one is required;
prefixes do not match report rows. Optional `--provider codex`,
`--provider claude_code` also includes reported active
sessions of that provider in `context_samples`, up to 256 samples total. That
option broadens the retained identifier scope; it does not enable compaction.
Use Python 3.9 or later on macOS/Linux. This
script does not install a service. A supervisor can invoke it periodically;
an exclusive lock prevents overlapping passes. Each pass gives each child
command its own 45-second budget, so a pass can last about 90 seconds plus
cleanup before it fails, and bounds captured output to 8 MiB per command.
Child process groups belong to this invocation and are collected on success
as well as failure, before reaping their leader. Cleanup allows up to
one additional second for the leader and 250 ms for nonblocking pipe drain;
`cleanup_complete` is explicit. A descendant that escapes the group can make
cleanup incomplete, but cannot hold the reader indefinitely. The runner trusts
the selected binary not to detach or change privileges; it is not a sandbox.
SIGTERM/SIGINT cancellation kills and reaps the owned child before exiting;
interrupted passes do not replace the last complete observation.

The script runs `gobstopper report --active-only --context-only` and
`gobstopper watch --dry-run --active-only --once --eval-budget 38`. It writes an atomic
`latest.json` and appends `observations.jsonl`, rotating at 10 MiB and retaining
one previous log. Reports include the executable's SHA-256 and command duration,
exit code, and closed error code. Freeform command output is discarded. The
watch command's known per-session load/plan failure diagnostics are classified as
`watch_evaluation_failed` even when that command exits zero; plan count is then
unavailable. `--eval-budget` keeps transcript loading and plan evaluation seven
seconds inside the command's own timeout: a pass that exhausts it defers its
remaining sessions unevaluated instead of hitting TIMEOUT, prints a
`[dry-run] eval budget:` coverage line (not a failure shape), and still exits
zero with a possibly smaller plan count. Both commands also reuse the
advisory discovery fingerprint snapshot each watcher lane leaves in
`discovery-cache-<provider>.json` beside watch state, so an unchanged
session file is restated rather than reparsed and an unchanged Devin
session (same activity stamp and chain head) replays its stored context
measurement instead of re-walking the store; a missing or corrupt
snapshot simply costs a cold rescan. Both commands use a 180-second file-recency window for JSONL
discovery. Claude also includes older files whose bounded metadata scan finds
a live process. These observations do not establish mutation custody. The report
is capped at 2,000 sessions. `sessions` contains
only exact allowlisted Codex rows; `context_samples` also includes explicitly
selected IDs or provider opt-ins from other providers. A missing or idle row is
unavailable, never a zero-valued measurement.

The additive `coverage` summary reports selected Codex overlap, unavailable
selected IDs, eligible versus exported samples, measurement states/reasons,
provider discovery status and truncation. Its closed `issues` list distinguishes
empty selected overlap, incomplete measurements and discovery gaps from command
failure. A successful command does not establish complete observation coverage.
Discovery counts are bounded; `omitted` is a lower bound when a directory or row
limit prevents enumeration. Provider status is available only when every expected
provider has one valid status record.

`context_components` retains known input, cache-read, cache-creation and output
counters when a provider leaves a component unavailable. `context_reason` names
the closed failure category. `measured_component_subtotal` sums only known
components with overflow checks; it is neither complete occupancy nor an
unqualified lower bound on provider usage. Complete context remains null for
partial, malformed, reset or unknown observations. Explicit reported zero is a
measurement, but does not establish a positive before/after reduction. Claude
usage scans validate a compact ancestry projection over at most 64 MiB, 100,000
records and 4 MiB per record. A limit, ambiguous ancestry or observed source
change remains unavailable; a long file is never assumed complete from its tail.

`watcher_checkpoints` separately reads the three private provider checkpoint
files, with no provider/configuration execution. It reports missing/unavailable,
artifact mismatch, in-progress, stale or fresh completion using recorded pass
times and the selected executable digest. Freshness is not a liveness probe or
proof of successful compaction. Decision counts describe the latest pass only;
they are not cumulative provider coverage. `native_activation` remains
`unqualified` in release builds. The checkpoint config fingerprint includes
watch decision inputs; it differs from the versioned parsed-config digest on
new event records. Neither digest reveals private configuration strings.

Each command also includes an additive `resources` object with `user_cpu_us`,
`system_cpu_us`, `minor_page_faults`, `major_page_faults`,
`voluntary_context_switches`, and `involuntary_context_switches`. CPU durations
are integer microseconds; the other fields are integer counts. These are
best-effort differences in the observer process's `RUSAGE_CHILDREN` counters,
sampled before spawning and after fully reaping the child, including a child
killed on timeout. The observer runs children serially and spawns no other
children between those samples. The counters are cumulative OS child-accounting
data, not a process-tree trace; descendant accounting and counter availability
depend on the operating system and whether descendants were waited for. They do
not measure all machine work or unaccounted surviving descendants.

A command that could not be spawned or an unavailable resource measurement
has `resources: null`. A slow or timed-out report does not shorten the watch
budget: each command starts with its own deadline. Individual invalid,
regressing, or out-of-range counters are `null`; valid unchanged counters are
`0`. Values are restricted to unsigned 64-bit integers. Diagnostic failures do
not change command errors, cleanup, the command deadlines, or capture limits.
Resource and wall-clock durations can help investigate a slow pass, but do not
establish its cause or prove Gobstopper token, cost, or quota savings.

Children receive an empty temporary `XDG_CONFIG_HOME`, no inherited
`GOBSTOPPER_*` settings, and an explicit heuristic scorer. This prevents a
configured plugin, legacy command, model scorer, or digest generator from
running during dry-run evaluation. Existing provider and data roots are retained
so discovery and native hook counters refer to the selected installation.
Plan count uses the built-in default policy, not custom user policy, and counts
only unambiguous allowlisted ID prefixes printed by dry-run watch. The script
does not compact, rewrite, resume, snapshot, or fork sessions, and makes no model
API calls. It reads the provider histories through Gobstopper's read-only commands.

Context drops are differences between consecutive observations, **not savings
attributable to Gobstopper**. Native provider compaction, a resume, another
process, and ordinary accounting changes can cause them. Every observation
explicitly records unknown attribution. Context state distinguishes `reported`,
`absent`, `unknown` and `reset`; only reported observations are compared. A zero
context reading after a compaction boundary is treated as unavailable until a
provider usage record arrives. `source_identity_sha256` must match across the
pair, so the same native ID from a foreign store cannot produce a delta.
First-sample deltas, missing fields, and deltas after a missing observation are
`null`; comparable unchanged values yield `0`. Counter decreases are marked as
resets and have unknown deltas. Lifetime counters are exposed only for `full`
scope; bounded discovery tails with `partial` scope do not become lifetime
totals. `native_hook_applied` reads the report's additive
`compactions.nativeHookApplied` counter. Older binaries that omit that field
produce `null`, and the aggregate count of all applied strategies is never used
as a substitute. Current hook callbacks are unattributed observations and never
increment that applied counter. Legacy hook labels alone are not evidence of
causal compaction or reduced usage. Retention aggregation requires an applied
event with no error and internally consistent source-bound before/after
snapshots. Its provider, native session ID and source identity must match a
selected context sample. Exact before/after snapshot pairs count once;
conflicting counts or source hashes for one pair exclude it. The reader follows the same `XDG_DATA_HOME` as the
child commands, refuses symlinks and special files, rejects duplicate JSON keys,
and bounds the log to 16 MiB and each record to 16 KiB. Missing, unreadable,
oversized or torn logs have `available: false` and a closed `error`; malformed
complete records increment `invalid_records`, and oversized records increment
`oversized_records`. Either makes retention unavailable with `event_log_invalid`:
discarding a bad record could hide a contradiction of an otherwise valid pair.
Valid noncompaction events and incomplete legacy evidence remain unqualified.
`conflicting_pairs` reports ambiguous tallies. Legacy count fields remain zero
when unavailable, so consumers must check availability. This covers one local log generation, not
lifetime retention. Absent coverage is unmeasured.

Released CLI native dispatch remains guarded even with an old opt-in. The
monitor reports this as `closed_session_compact: "unqualified"` where supplied;
it is not an invitation to remove the guard or retry an unknown operation. See
the [activation matrix](../docs/assurance/qualification.json) and
[recovery runbook](../docs/assurance/operations.md).

The output directory must belong to the current user and be private (0700).
New directories are created privately; an existing nonprivate directory is
refused rather than having unrelated permissions changed. Symlinked output
paths, symlinked or hardlinked files, and nonprivate existing output files are
refused. Created files use 0600. Session IDs remain identifying information;
keep these local records private. No transcript text, filesystem paths,
identifiers outside the selected ID/provider scope, or remote credentials are
retained in observations.

Run the focused fixture tests with:

```sh
python3 -m unittest discover -s scripts -p 'test_monitor.py' -v
```

Tests exercise realistic subprocess reports and dry-run output, source
preservation, private permissions, allowlisting, environment isolation, missing
data, counter resets, symlink refusal, overlap exclusion, log rotation, timeout,
bounded capture, zero-exit evaluation failures, and cancellation cleanup. They
also cover numeric child-resource deltas, timeout reaping of each command
under its own deadline, unavailable or invalid counters, and diagnostic
failure without losing command results or cleanup, same-ID foreign stores, duplicate/conflicting
evidence, torn or special-file logs, successful leaders with silent descendants,
and escaped pipe holders. They do not establish live session compaction savings.

# Local observation pass

`monitor.py` performs one read-only pass for explicitly selected Codex sessions:

```sh
python3 scripts/monitor.py \
  --binary /absolute/path/to/gobstopper \
  --output-dir /absolute/private/monitor-directory \
  --session 01a00000-aaaa-7000-aaaa-aaaaaaaaaaaa
```

Repeat `--session` for more exact native session IDs. At least one is required;
prefixes do not match report rows. Use Python 3.9 or later on macOS/Linux. This
script does not install a service. A supervisor can invoke it periodically;
an exclusive lock prevents overlapping passes. Each pass gives both child
commands a combined 45-second budget, and bounds captured output to 8 MiB per
command. Timed-out child process groups belong to this invocation and are killed.
SIGTERM/SIGINT cancellation kills and reaps the owned child before exiting;
interrupted passes do not replace the last complete observation.

The script runs `gobstopper report --active-only` and
`gobstopper watch --dry-run --active-only --once`. It writes an atomic
`latest.json` and appends `observations.jsonl`, rotating at 10 MiB and retaining
one previous log. Reports include the executable's SHA-256 and command duration,
exit code, and closed error code. Freeform command output is discarded. The
watch command's known per-session load/plan failure diagnostics are classified as
`watch_evaluation_failed` even when that command exits zero; plan count is then
unavailable. Both commands limit discovery to files updated within 180 seconds
before reading their usage; activity is a file modification heuristic, not proof
of an owning process. The report is capped at 2,000 sessions. Only exact
allowlisted Codex rows reach the saved observations. A missing or idle row is
unavailable, never a zero-valued measurement.

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

An unstarted command (including watch after report exhausts the shared deadline)
or unavailable resource measurement has `resources: null`. Individual invalid,
regressing, or out-of-range counters are `null`; valid unchanged counters are
`0`. Values are restricted to unsigned 64-bit integers. Diagnostic failures do
not change command errors, cleanup, the shared deadline, or capture limits.
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
explicitly records unknown attribution. A zero context reading after a compaction
boundary is treated as unavailable until a provider usage record arrives.
First-sample deltas, missing fields, and deltas after a missing observation are
`null`; comparable unchanged values yield `0`. Counter decreases are marked as
resets and have unknown deltas. `native_hook_applied` reads the report's additive
`compactions.nativeHookApplied` counter. Older binaries that omit that field
produce `null`, and the aggregate count of all applied strategies is never used
as a substitute. Hook counts are lifecycle observations, not proof that the
hooks caused compaction or reduced usage. Reported counters inherit any limits
of the underlying report and event log.

The output directory must belong to the current user and be private (0700).
New directories are created privately; an existing nonprivate directory is
refused rather than having unrelated permissions changed. Symlinked output
paths, symlinked or hardlinked files, and nonprivate existing output files are
refused. Created files use 0600. Session IDs remain identifying information;
keep these local records private. No transcript text, filesystem paths, unrelated
session IDs, or remote credentials are retained in observations.

Run the focused fixture tests with:

```sh
python3 -m unittest discover -s scripts -p 'test_monitor.py' -v
```

Tests exercise realistic subprocess reports and dry-run output, source
preservation, private permissions, allowlisting, environment isolation, missing
data, counter resets, symlink refusal, overlap exclusion, log rotation, timeout,
bounded capture, zero-exit evaluation failures, and cancellation cleanup. They
also cover numeric child-resource deltas, timeout reaping versus an unstarted
second command, unavailable or invalid counters, and diagnostic failure without
losing command results or cleanup. They do not establish live session compaction
savings.

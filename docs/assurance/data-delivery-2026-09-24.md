# September 24 data improvement delivery

The measurement and runtime changes in [PR 94](https://github.com/hraness/gobstopper/pull/94)
merged as `7ff13e30841e700eba211d860cd4713e1e0654a3`. They preserve partial usage
without reporting a complete context, validate long Claude ancestry, refuse
disabled native work before exports and snapshots, expose monitoring coverage,
reject conflicting or incomplete reduction evidence, and add read-only vault
metadata accounting. Accounting explicitly releases its shared locks even when
a duplicated descriptor remains open.

## Verification

The [merged-source CI run](https://github.com/hraness/gobstopper/actions/runs/36025346784)
passed all six required jobs. Its quality log records 558 Rust tests, 150 Python
tests, the separate 512-case dialect replay, formatting, Clippy and doctests
(no examples). The separate MSRV job passed with Rust 1.85.0.

The proof artifacts contain 24 TLA+ configurations, 16 Kani harnesses and 27 Lean
laws with 69 actual Rust correspondence steps. Kani reports 148 successful and
30 unreachable assertion checks, with 52 satisfied covers. The bounded stress
inventory contains 159 exact tests. Semantic negative controls must fail at their
intended assertion; unrelated compiler failures and timeouts cannot pass them.

The fresh local macOS stress run passed all 159 tests. The macOS workspace
aggregate remains failed: its 64-step sequence exceeded the existing 90-second
deadline, as did two focused diagnostic runs. That sequence passed in 38 seconds
in the separate local stress run and in the merged Linux aggregate. The host-level
cause was not established; no deadline or assertion was weakened to admit those
failed local runs.

An independent AI review rechecked the seven local and merged CI receipts,
including source and raw-log hashes, normalization, exact negative controls and
all four archive digests. The private review is
`implementation/merged-proof-independent-review.json`, SHA-256
`ce91a194b5285015fb0d390ce764c58f06c8237f4f37f49907ddff82e55e0d63`.

The [merged CodeQL run](https://github.com/hraness/gobstopper/actions/runs/36025345796)
completed all four analyses. Rust reported 25 results: 20 open findings and five
previous dismissals dated September 20. The other three analyses reported zero.
Independent source review found no blocking security regression; the two changed
alert IDs correspond to unchanged sinks. See the [source dispositions](codeql-pr90-review.md).
The private security review SHA-256 is
`04db7f63f91779cd21ff53807fcfc2e69edf2ff38def1d8be5a21dbcb6711706`.

## Installed artifact

The source tree is `2515574f36d42c9d731fff027f79b5bc52f657cb`. The installed
`gobstopper 0.2.1` binary has SHA-256
`31a5b3ea2bfc524282779e71eb2e37f57b7eb4057f755f19d1317dcbdae96a79`;
the installed monitor has SHA-256
`114b23f4bf5a798ee2dbeb6c0498f85825d868adf12e39be8fd9c1889eef847b`.
There is no new package tag: this is the documented checked source installation.

All prior watcher processes exited naturally. A stable stopped backup preceded
installation. The version, deterministic policy and native-operation inspection
checks passed before the four admitted launch jobs restarted at 17:06 UTC. The
configuration was restored from its exact original bytes and mode.

### Runtime readiness

The cutover protocol evaluates readiness with pure rules over the three watcher
checkpoints and the latest monitor observation: each checkpoint must carry the
installed binary hash and a pass completed within 180 seconds, and the latest
observation must be fresh with both commands healthy. Those rules were applied
read-only at 17:45 and 17:50 UTC. The protocol's final receipt step, which
records `postbootstrap.json` next to the bootstrap receipt, was not run in this
session: the session's permission reviewer declined it, and no other route was
used to produce the receipt. It remains an owner action; the protocol scripts
are retained privately with their hashes under
`runtime-observation-window/cutover-protocol/`.

Every checkpoint carries generation 9, the installed binary hash and native
activation `unqualified`. At 17:45 UTC the Codex and Claude Code checkpoints
were fresh and the Devin checkpoint was mid-pass; at 17:50 UTC all three were
mid-pass. Between 17:11 and 17:36 UTC the monitor recorded eleven instants at
which both commands were healthy and all three checkpoints were fresh, so the
receipt criteria were met repeatedly but not continuously. Readiness now
fluctuates with pass duration rather than with the binary: completed passes
since the restart took a median of 2.8 seconds for Claude Code (22 passes,
maximum 95), 15.3 seconds for Codex (15 passes, maximum 328) and 10.0 seconds
for Devin (17 passes, maximum 331).

The 497 telemetry events written after the restart all carry the installed
binary hash and match the new early-refusal contract: Codex recorded 166
`blocked/native_unqualified` decisions under `auto` and 168 `skipped/unresolved_context`
control decisions, Devin 32 and 33, and the Claude Code hooks recorded 37
`unattributed_provider_hook` skips. No native provider call was attempted.

## Observation window

The window covers the 28 monitor observations from the 17:06 UTC restart to
17:50 UTC. Both commands were healthy in 14 of them. The report command timed
out seven times, three of them within five minutes of the restart; healthy
reports took a median of 1.9 seconds and a 90th percentile of 18.7 seconds. The
dry-run watch command errored 14 times, every one a timeout under the shared
45-second budget; healthy runs took a median of 22.3 seconds. The 17:43 UTC
report exported 20 of 20 eligible context samples (16 complete, four partial)
and recorded discovery of 9,887 Codex, 217 Claude Code and 104 Devin files.

The slowdown predates the installation. Under the previous binary the dry-run
watch took a median of 178 milliseconds until 13:59 UTC, then timed out 5, 12
and 11 times in the 14:00, 15:00 and 16:00 UTC hours, three hours before the
restart. The host was saturated throughout the window: load averages of 25 to
35 in the retained samples, the data volume at 99 percent with about 18 GiB
free, two solver processes running for 20 hours, repository checks and a merge
queue, build-directory deletions, and the Devin CLI appending about 1.9 GB to
its session-store log in under four minutes (about 9 MB/s) while the disk
sustained about 90 MB/s. The previous binary timed out 11 of 12
times in the same conditions immediately before its replacement, so the
installed binary is not shown to be slower; a controlled comparison was not run
because it would add more of the same I/O to the saturated host.

Stack samples and the discovery corpus explain where the time goes. Discovery
cached a file fingerprint for only 60 seconds, so each 60-second watcher pass
rescanned every candidate: 1,961 Codex transcripts modified in the seven-day
window mean about 1.08 GB of bounded head and tail reads per pass, and the
Codex watcher sample spent 80 of 92 frames in those reads. The Devin watcher
scan reads a 17.0 GB SQLite store while Devin writes it (89 of 90 frames in
page reads). The monitor's two commands do not rescan that corpus: their
`--active-only` discovery is limited to sessions active within 180 seconds, and
a sample of the dry-run watch spent nearly all of its time exporting the one
active Devin session for its plan preview, which is the documented dry-run
contract. Inside the monitor, a slow report consumed the shared budget and
watch then recorded a zero-duration timeout, which was the contract during the
window. D7 in the [data improvement plan](../data-improvement-plan.md) keeps an
identified cache entry valid until its file changes and gives each monitor
command its own budget. An earlier version of this paragraph attributed the
corpus rescans to the monitor as well; that was wrong.

Two operational findings need an owner. The monitor's 14 allowlisted session
IDs are all Codex sessions last updated between September 19 and 21, so the
selected-overlap coverage is empty by construction until the launch job's
allowlist is refreshed. The Devin session store also carries a 26.4 GB write-ahead
log of constant size on the nearly full volume; its frame counter advanced by
456,032 frames in under four minutes with checkpoints keeping pace and read marks
moving, so no reader is stuck, but only Devin's own checkpoint can truncate it
and Gobstopper opens the store read-only.

No product change was made for these findings. The planned follow-ups, each a
reviewed source change with tests, are recorded in the
[data improvement plan](../data-improvement-plan.md): trust an unchanged file
fingerprint beyond the sample interval, avoid rereading unchanged Devin
sessions, give the monitor's two commands separate deadlines, and refresh the
allowlist. The private observation record is
`runtime-observation-window/observation-window.json`, SHA-256
`ffe7ff7244763f1d09a88beb0ed34f35960b16b315967d67b759f2a8fc14adb6`, with the
stack samples under `implementation/`.

## Remaining evidence limits

The assurance case in [README.md](README.md) states each proof's assumptions.
Lean list laws apply to arbitrary finite lists; Rust correspondence is a finite
test suite. Kani covers selected production kernels. TLA+ exhausts specified
finite abstract state graphs. Whole-program refinement, physical power-loss
behavior, arbitrary asynchronous cancellation and proprietary provider behavior
remain outside these results.

No native provider cell is qualified. The registered held-out retention study
has not run. These changes neither enable provider mutation nor establish task
success, billed savings, causal latency improvement or sustained reliability.
The 44-minute window above is a short observation on a saturated host, not a
reliability measurement.

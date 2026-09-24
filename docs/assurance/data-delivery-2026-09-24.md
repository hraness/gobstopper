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

## D7 follow-up installation

[PR 98](https://github.com/hraness/gobstopper/pull/98) merged as
`1eae6b352b5501a44aaeae3af237cef72a110625` with source tree
`23dfc843c0ff4b75ce684fdbb947201cf6f54e12`. It keeps an identified
discovery-cache entry valid until its fingerprint (length, modification time,
device, inode and change time) changes, keeps the 60-second bound for entries
without identity fields, and gives the monitor's report and dry-run watch
commands separate 45-second budgets. The watch state generation is unchanged
at 9.

The [merged-source CI run](https://github.com/hraness/gobstopper/actions/runs/36058785133)
passed all six required jobs; its quality log records 560 Rust tests, 150 Python
tests and the separate 512-case dialect replay. The
[merged CodeQL run](https://github.com/hraness/gobstopper/actions/runs/36058784780)
completed all four analyses. A local release gate rebuilt the merged tree with
the locked dependencies and passed the four release-only refusal tests; the
installed hashes below come from that build. An independent read-only review
of the merge diff approved it for operational admission with one advisory and
four notes. The advisory: an identified cache entry has neither a time bound
nor a content hash, so a same-length in-place rewrite that lands within the
filesystem's timestamp granularity of the previous write would not be rescanned.
The notes: the negative test changes length and both timestamps together, so no
test isolates a same-length or rename-replace change, although tuple equality
covers both today; a monitor pass can now hold its exclusive lock for about 90
seconds, so a supervisor with a period of 90 seconds or less sees overlapping
invocations rejected; the monitor timeout test could fail under heavy host load
without a product defect; and earlier stress receipts keep the old monitor test
name as immutable history. The private review is
`implementation/d7-change-independent-review.json`, SHA-256
`3bf9b26d5fa45ee98785afda96866c3616c43aad9b740a8dc3fe46667a1838d8`.

### Installed artifact

The installed `gobstopper 0.2.1` binary now has SHA-256
`528f88758ccb0ec5f3ff78e20c41c9f34afc8b3b1d38772f1def352e1fec1d7f`;
the installed monitor has SHA-256
`e452d992b2a2f31ec4565d7da72fbdbffdf830c30d5f26b42d3e818f08fd7646`.
There is still no new package tag: this is the documented checked source
installation from the merged tree.

The cutover reused the morning's reviewed protocol. The copied helpers differ
from the retained originals by four exact strings (the bundle directory, the
evidence base and the two previously installed hashes); the copy passed its 37
synthetic data-helper tests and 17 helper checks before pins, a source gate
receipt and a root review admitted each operation. The three watchers exited
naturally at 22:29 UTC after the configuration marker was published. The first
removal attempt at 22:30 UTC observed a watcher that launchd had restarted onto
the marker and returned pending without sending a signal; the explicit
reconcile-then-resume path removed the four jobs at 22:33 UTC. A stable stopped
backup preceded installation, the Cargo build was a cached rebuild of the gated
tree, and the version, deterministic policy and native-operation inspection
checks passed before the four admitted launch jobs restarted at 22:35 UTC with
the configuration restored from its exact original bytes and mode. The protocol
copy, its pins, receipts and the retained-copy manifest (SHA-256
`fab08ba0320e61655790afb6d03354112d5c2e818d54f734567611f4997a2825`) are kept
privately under `runtime-d7-cutover/`.

### Runtime readiness

The protocol's final receipt step ran this time. The read-only readiness
evaluation was applied once a minute from 22:35 UTC; the first eleven
evaluations were pending on the cold first passes of the Codex and Devin
watchers, which completed at 22:42 and 22:43 UTC, and on monitor commands that
timed out under host load averages of 33 to 38. The twelfth evaluation passed
at 22:46 UTC and recorded `postbootstrap.json` next to the bootstrap receipt,
so the receipt owner item from the morning cutover is closed. The receipt binds
the 22:46:11 UTC monitor observation, in which the report command completed in
1.8 seconds and the dry-run watch in 20.8 seconds with an empty plan, and all
three checkpoints were fresh with generation 9, the installed binary hash and
native activation `unqualified`. Coverage is recorded separately from command
health: the 14 allowlisted sessions still have no active overlap, so that owner
item stands, while the report exported 20 of 20 eligible context samples (15
complete, three partial) and discovery scanned 9,887 Codex, 218 Claude Code and
104 Devin files. The native journal, configuration and launch definitions were
unchanged throughout.

### Observation window

The window covers the 21 monitor observations from the 22:35 UTC restart to
23:10 UTC, about 36 minutes on a host whose load average rose from 33 to 44.
Observations came about 100 seconds apart because launchd starts the next pass
only after the previous one exits, and a pass with two timeouts lasts about 90
seconds. Both commands were healthy in 13 observations, and at nine instants all
three checkpoints were fresh as well. The report command timed out twice;
healthy reports took a median of 4.6 seconds and a 90th percentile of 26.2
seconds. The dry-run watch command timed out eight times, each under its own
45-second budget: no watch timeout had zero duration, which is the monitor
change D7 made. In the hour before the restart the previous binary recorded 11
watch timeouts in 38 observations, one of them zero-duration after a slow report
consumed the shared budget. Healthy dry-run watch runs took a median of 17.1
seconds. The 23:10 UTC report exported 20 of 20 eligible context samples (17
complete, three partial) and discovery scanned 9,887 Codex, 218 Claude Code and
104 Devin files. Monitor command health is not shown to be better or worse than
before the installation: the hour before the restart had 27 of 38 observations
healthy at a lower load, and no controlled comparison was run. The data volume
had 163 GiB free at 91 percent, up from 18 GiB in the morning.

The cache change shows in the watcher passes that the monitor checkpoints
recorded. The first pass under the new binary was cold: the Codex pass
completed between 22:41 and 22:42 UTC, and the Devin pass took 485 seconds,
finishing at 22:43 UTC. After that, completed Codex passes over 1,932
discovered files took a median of 2.5 seconds (18 passes, maximum 17.2 seconds)
against a median of 15.3 seconds and a maximum of 328 seconds in the morning
window, and Claude Code passes took a median of 4.2 seconds (20 passes, maximum
36.8 seconds). Devin passes took a median of 15.6 seconds (12 passes, 90th
percentile 135 seconds): the Devin per-pass read cost is the deferred part of
D7, so each pass still reads the session store, on a busier host than in the
morning. The 480 telemetry events written after the restart all carry the
installed binary hash and match the early-refusal contract: Codex recorded 164
`blocked/native_unqualified` decisions under `auto` and 168
`skipped/unresolved_context` control decisions, Devin 26 and 29, and the
Claude Code hooks recorded 30 `unattributed_provider_hook` skips. No native
provider call was attempted. The private observation record is
`runtime-d7-cutover/observation-window.json`, SHA-256
`b9085b128a6916a8ac540592bbc7fa78beb10f2c78953d159c97230c3c185935`.

The two operational findings from the morning keep their owners: the monitor's
14 allowlisted sessions still have no active overlap until the launch job's
allowlist is refreshed, and the Devin write-ahead log remains a Devin-side
item. No product change was made in this installation beyond the merged D7
source.

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
The 44-minute and 36-minute windows above are short observations on a
saturated host, not reliability measurements.

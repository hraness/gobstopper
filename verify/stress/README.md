# Bounded synthetic stress evidence

This runner joins existing production-connected fault, restart, parser and
process fixtures. It is a deterministic bounded exercise, not a proof or a live
provider soak. All stores, sources, homes and provider programs are synthetic.
The temporary directory is private to the new evidence directory.

```sh
python3 verify/stress/test_check.py
python3 verify/stress/check.py --output "$NEW_EVIDENCE_DIR"
```

Cargo dependencies must already be fetched; execution is offline. Supported
platforms are Linux and macOS. The exact commands and 159 expected test names are
in `suites.json`. Tests run serially and Cargo uses two build jobs. The gate
rejects missing, duplicated, ignored, failed or unexpected test results. A zero
process exit with an empty or filtered-away suite is not evidence. The receipt
lists every passed test, each exact command, source/tool hashes, durations, logs
and the observed sequence/resource counters. Sources and tools must stay stable
through completion. Final integration and platform CI remain separate gates.
The receipt hashes the selected Cargo/compiler binaries as well as the rustup
dispatchers and Python executable. Compiler libraries, the linker, existing
Cargo dependency cache, Python standard library and operating system remain
trusted inputs; this is not a hermetic build attestation.

| Suite | Named tests | Exercised boundaries |
| --- | ---: | --- |
| Sequence | 1 | 64 snapshot/copy/pin/prune/recovery steps, alternating timestamps and 16 corruption/repair episodes |
| Storage | 16 | Publication errors, reused-object durability confirmation, torn writes, corrupt/legacy roots, conflicts, no-clobber recovery, SIGKILL checkpoints and independent reader custody; one test is the private child entry point |
| Vault metadata | 13 | Nonblocking locks, release with duplicate descriptors, no-follow paths, index/entry/time limits, non-atomic concurrent changes and explicit missingness |
| Vault accounting CLI | 2 | Additive stats mode, output privacy, no source creation, missing-root refusal and legacy list compatibility |
| Native journal | 10 | Prepared/dispatched/terminal persistence faults, refusal before dispatch, identity binding, fabricated reconciliation and unknown/corrupt state |
| Watch | 28 | Unqualified activation refusal, source/snapshot binding, no-op/unknown state, restart/config changes, two watchers and process death, explicit evidence-only reconciliation |
| Claude processes | 2 | Normal/timeout descendant collection and refusal before spawn |
| ACP processes | 10 | Exact session evidence, early completion, response/request bounds, blocked writes, queue bursts, EOF/timeouts and descendant collection |
| Codex processes | 11 | Terminal correlation/order/duplicates, lost acknowledgment, frame limits, private diagnostics and inherited/escaped pipe holders |
| Plugins | 11 | Exact bundle trust, capture, capability restrictions, timeout/flood/blocked stdin, aggregate projection and child/reader cleanup |
| Events | 15 | FIFO/symlink/actual-read limits, append and rotation admission, strict accounting evidence and log compatibility |
| Monitor | 40 | Source-bound observations, malformed/conflicting history refusal, measurement admission, bounded private logs, timeouts, child collection and inherited environment isolation |

The aggregate has a 900-second deadline, individual suites have reviewed limits
of 60–240 seconds including compilation, and logs are capped at 8 MiB per
command. The shared `watch.run_owned` reactor retains process-group identity
through cleanup, observes exit without reaping, and avoids blocking pipe reads.
If a deadline, output cap or cleanup operation fails, admission fails.

The 28-test watch suite has a 240-second limit. Executable provenance now hashes
the complete debug binary at each CLI startup, and these tests start many CLI
processes. The unchanged suite passed in 152 seconds locally and 163 seconds in
Linux CI; a later CI run exhausted the former 180-second aggregate suite limit
near its final tests. A focused rerun and a timed full rerun found no stalled
test. The larger suite budget preserves every test, serial execution, child
cleanup, and the 900-second overall limit. It is not a production latency target.

The sequence itself must finish in less than 90 seconds with the exact seed,
64 steps and 16 corruption recoveries. Its largest **post-step** footprint must
stay at or below 1,200 files and 16 MiB. These are measured points in a fixed
synthetic workload, not a filesystem quota or a claim about every transient
allocation. The runner additionally rejects a largest reaped-child RSS
high-water above 2 GiB. `RUSAGE_CHILDREN.ru_maxrss` is cumulative across the
runner's children and describes the largest such high-water, **not** simultaneous
whole-tree RSS; it is checked after each suite, not enforced as an OS memory
allocation limit. Compilation is included in that observation.

Fixtures assert collection of the particular descendants they own and bound
provider frames, queues, I/O and deadlines. Serial test execution and fixed
fixture fan-out bound this workload; this is not a hard process-count sandbox
for arbitrary escaped or privilege-changing programs. No claim is made that
SIGKILL or a userspace watchdog can preempt an uninterruptible kernel filesystem
operation. Cleanup/wait failure leaves a failed receipt and can require host
recovery. The underlying local Unix filesystem and scheduler are assumptions.

The retained failure minima include damaged roots refusing prune, unresolved
native operations refusing replay, output-without-ack uncertainty, failed
dispatch checkpoint refusing a provider call, inherited pipes and bounded event
reads. Injected errors and process death do not establish physical power-loss
durability, actual full-volume behavior, network-filesystem locking or live
provider ownership/resume compatibility. Native journal record-state faults are
covered, but **not every target-registry persistence boundary** currently has a
process-crash fault seam. The separate corpus/Hegel, formal and activation
gates retain their own scopes; a successful bounded run does not absorb them.

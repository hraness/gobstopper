# Gobstopper: 729-session retrospective — September 19, 2026

**The strongest supported result is 36.4% median projected context reduction across 73 high-context archived Codex root tasks.** All 73 produced portable `compacted` plans. This is an offline estimate from one Mac, not measured billing savings or proof of task quality.

We evaluated 729 distinct frozen sessions with five strategies, producing 3,645 evaluations with zero evaluation failures. Selection rules were recorded before compaction outcomes. The separately discovered archived-root cohort does not overlap the initial 615 cohort by session identity or exact event body. Private transcript text stayed local; these runs used deterministic rules, with no remote model or judge.

## Results include zero-result cases

The table reports the portable `compacted` strategy with a fixed 80,000-token trigger, 40,000-token floor, eight recent outputs protected and 4,096 minimum estimated savings. Medians include every eligible session in the row, including no-ops.

| Cohort | Sessions | Plans | No plan | Median projected reduction |
| --- | ---: | ---: | ---: | ---: |
| Archived Codex roots, high context | 73 | 73 | 0 | 36.44% |
| Codex subagents, high context | 512 | 2 | 510 | 0% |
| Claude sessions, high context | 23 | 17 | 6 | 14.85% |
| Below-trigger controls, all cohorts | 121 | 0 | 121 | 0% |
| All selected sessions | 729 | 92 | 637 | 0% |

The overall 0% median matters: a large provider context count alone does not establish that a transcript contains eligible old tool output. A small inspection of no-plan subagent records found message/reasoning histories with no tool-result response items. This is a limited diagnostic observation, not a classification of every no-op.

For the 73 high-context roots, projected reduction ranged from 2.96% to 69.44%; summed estimated reduction was 4,012,086 of 10,797,307 current-context tokens. Among 57 root tasks with prior native-compaction records, the median was 36.05%. These figures describe the context represented after the latest native boundary; they do not treat the full on-disk transcript as the current prompt.

The simpler `elide` strategy reached 37.03% median reduction on those roots, with lower sampled-string retention. Heuristic `scored`, `cache_aware` and portable `compacted` produced the same reduction totals here. `dedupe` produced only one plan among all 729 sessions. No model-quality comparison follows from this heuristic-only study.

## What survived, and what this does not prove

On the 73 root rewrites, **3,664 of 4,762 sampled strings survived (76.94%)**. The remaining 1,098 strings were absent from the smaller context. This is literal matching, not a semantic importance or task-success score: some removed strings may be obsolete, while others may matter later. Exact snapshots provide an explicit recovery path; they do not make the model automatically recall everything.

The selected protected-tail probes all survived: 122/122, but only 31 of 73 root rewrites had any such probes. Root baselines and rewritten outputs had zero structural errors, and verifier error/warning counts did not increase. In the first 615 cohort, two source transcripts already had errors; one error persisted in a rewritten Claude sample. Across the study, “no increase in verifier severity/code counts” is supported; “all forks are proven safe to resume” is not.

The evaluator's nominal 64-probe selection limit could overshoot to 65–67 because of its round-robin loop. Actual counts above reflect the measured output, not the nominal limit. A separate focused correction was merged in [PR #32](https://github.com/hraness/gobstopper/pull/32) and installed after these runs. The benchmark remains pinned to its original executable; its results have not been silently replaced with later-build results.

No continuation task was evaluated by this replay. No provider billing, subscription quota, cache-reuse benefit, or recovery-time saving was measured. Evaluation time included local processing only; scheduler wait and model continuation are separate costs.

## Sampling and reproducibility

The first cohort contained 576 Codex subagent histories and 39 Claude histories; it contained no Codex root tasks. The archive audit found 4,812 files, including 146 strict root candidates identified by recognized source metadata and absent parent/fork metadata. It excluded 31 roots over 128 MiB and one lacking positive usage after the latest native boundary, leaving 114 roots: 73 high-context and 41 controls. Of these 114, 79 had prior native-compaction records. The file-size bound excludes some of the longest tasks and can bias results.

Both cohorts came from one person's machine, July 30–September 19, 2026. Shared projects, prompts and incomplete ancestry limit independence. The archive was added because normal discovery excludes archived files, not because its compaction results were known. Its registration preceded strategy evaluation. The earlier six-root convenience pilot is separate and is not included in the 729-session total.

Inputs were copied privately, checked before/after evaluation, and never rewritten in place. The scorer and policy were isolated from the user's normal configuration; remote credentials were removed from the subprocess environment. No-ops and failures remain in the reporting denominators. All selected samples had matching provider-usage and normalized-context denominators; no primary numerical exclusions were needed.

Measured executable SHA-256: `cef977498f235560109b2930bb7ca06f4f1c3ca4c633c1435e0314043624d3ec`, built from merged source `6c2f32a60bf7aec3c38d10853abd8f4d8e9be059`. Hashes were identical at the start and end of each run. Private run records retain runner, protocol and manifest hashes. The 615-session replay took 135.35 seconds after admission; the 114-root replay took 179.36 seconds. These are run durations, not per-compaction service latency claims.

## Downloads and privacy

- [Aggregate results](aggregates.json)
- [Selection and analysis protocol](protocol.json)
- [Benchmark page](https://gobstopper.sh/benchmarks#retrospective-2026-09-19)

Only aggregate statistics and methodology are published. Private transcript text, session identifiers, paths, per-session hashes, manifests and per-session results are excluded. The exact private corpus cannot be independently replayed from these downloads. Code and executable hashes identify the measured implementation, not public access to its inputs.

This is not a head-to-head benchmark against fast-jev-compaction or a comparison of model backends. Jev was not called in this study.

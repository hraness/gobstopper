---
license: cc-by-4.0
pretty_name: Gobstopper benchmark reports
tags:
  - benchmark
  - evaluation
  - context-compression
---

# Gobstopper benchmark reports

Public aggregate results and protocols from [Gobstopper's compaction studies](https://gobstopper.sh/benchmarks). Use the downloads to inspect selection rules, denominators, retention measurements, recovery tests, and the limits of each historical experiment.

## Studies

`data/2026-09-19/` contains the retrospective's aggregate results, protocol, and report, plus the separately recorded retention ablation and Apple retention pilot. Keep each experiment's corpus, policy, executable hashes, and limitations with its results. Projected token reduction is an offline estimate. It does not measure provider billing or successful continuation of a task.

`data/2026-09-20/` contains the recovery study's protocol and results. Its synthetic fixture evaluation is separate from the private-session studies. Follow the public reproduction instructions in the protocol and the [source repository](https://github.com/hraness/gobstopper).

The private studies came from one person's machine. Their published aggregates cannot establish performance across other users or independently reproduce the private corpus. Literal string retention does not establish semantic importance or task quality. Historical results describe their recorded implementations, not every later Gobstopper release or request-time proxy strategy.

These downloads form an artifact archive with several JSON schemas. Read individual files using their recorded schema; a combined `datasets.load_dataset` table is not provided.

## Privacy and provenance

Only the explicitly selected public reports and protocols are included. Private transcript text, session identifiers, local paths, vault snapshots, per-session hashes, and per-session results are excluded. Do not add those files when extending this collection.

`export-manifest.json` records the source commit and SHA-256 of every exported artifact. New completed studies receive new dated folders. Published study files remain frozen; corrections receive a new version with an explanation of the change. See [Gobstopper](https://gobstopper.sh/) for current product behavior.

## License and attribution

The selected aggregate reports and protocols in this archive are by Hraness and licensed under [Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/). Attribute Hraness and link to the [original benchmark page](https://gobstopper.sh/benchmarks) and the source revision recorded in `export-manifest.json`.

This license covers the selected files listed in `export-manifest.json` and this card. It does not cover source code, private transcripts, or raw provider responses. Source code retains its original terms.

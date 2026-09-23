# PR 90 security scan review

This independent review covers the 19 open `rust/cleartext-logging` annotations
at source `dcd702c4a18f411d6842fd687eedf9ad004cb1ca`, scanned under
`refs/pull/90/head`. The scan executed successfully; its alert aggregate failed.
These are individual source and caller assessments, not scanner dismissals or a
claim that session identities are universally nonsensitive. No suppression,
renaming to evade analysis, or scanner-state mutation was performed.

The 13 requested session-identity outputs remain part of explicit CLI commands.
Five other flagged sinks contain numeric results or fixed labels; one contains
a requested recovery reference. Background routes were traced separately.

| Alert | Operation | Source and output contract |
|---|---|---|
| [#33](https://github.com/hraness/gobstopper/security/code-scanning/33) | `print_plan` | Selected Discovered.handle.session_id. The text target preview is emitted only after a user requests plan/apply; print_plan has no watch caller. Preserve exact target identity. Other rationale fields are outside this identifier flow. |
| [#8](https://github.com/hraness/gobstopper/security/code-scanning/8) | `cmd_undo preview` | VaultEntry.session_id belongs to entries filtered by exact provider/session/store. Archived bytes are checked against selected identity/hash/length at 2017-2024 before preview. Preserve the explicit recovery-target confirmation. |
| [#26](https://github.com/hraness/gobstopper/security/code-scanning/26) | `cmd_undo completion` | Successful restore returns a newly generated fork identity, displayed as destination path and provider resume command. This is the requested recovery result; no background caller. |
| [#27](https://github.com/hraness/gobstopper/security/code-scanning/27) | `cmd_recall row` | Requested historical state-card result prints its owning session. recall_rows applies the caller-selected session/query/SHA/limit; a general recall explicitly requests matching vault results. Preserve result provenance, not blanket permission for daemon logs. |
| [#77](https://github.com/hraness/gobstopper/security/code-scanning/77) | `cmd_prune report` | PruneReport (vault.rs 796) contains only usize/u64 counts and a bool. This sink prints a fixed plan/pruned label, groups.len, retained/dropped counts, object counts and reclaimed byte sum. validate_session_id can affect upstream admission but no session string reaches this format. |
| [#63](https://github.com/hraness/gobstopper/security/code-scanning/63) | `print_retention event row` | Requested telemetry/retention report prints CompactionEvent.session_id with time/provider and bounded numeric observations. This is a direct report on the private event store, not spontaneous watcher output. |
| [#13](https://github.com/hraness/gobstopper/security/code-scanning/13) | `cmd_fork receipt` | Explicit fork prints selected source ID and newly created fork ID. Both are needed to identify the requested source/result pair. fork_with_vault verifies source bytes and publishes a separate artifact. |
| [#14](https://github.com/hraness/gobstopper/security/code-scanning/14) | `cmd_fork resume command` | ForkResult.resume_hint is assembled from a fixed provider command and validated generated output ID (fork.rs 321-325). Exact output ID is required to resume the requested copy. |
| [#15](https://github.com/hraness/gobstopper/security/code-scanning/15) | `cmd_eval heading` | Explicit selected-session evaluation prints that target identity and context/trigger metrics. eval_session_with_hooks freezes bytes and validates source identity before returning rows. No watch caller. |
| [#34](https://github.com/hraness/gobstopper/security/code-scanning/34) | `cmd_tune heading` | Requested tuning preview names the selected session and adaptive state. No configuration is installed, and no background call reaches this function. |
| [#78](https://github.com/hraness/gobstopper/security/code-scanning/78) | `cmd_eval failed-row report` | EvalRow.error is initialized None and assigned only transformation_failed or worker_panicked (eval.rs 515-620). All underlying anyhow/provider/transform errors are discarded before row construction. row.strategy comes from a fixed builtin Strategy.id. No source_session_id string reaches this sink. |
| [#35](https://github.com/hraness/gobstopper/security/code-scanning/35) | `cmd_tune TOML suggestion` | Explicit tuning request produces a concrete configuration example keyed by the selected session. The session key is required for the requested suggestion and is never written to a background diagnostic by this function. |
| [#79](https://github.com/hraness/gobstopper/security/code-scanning/79) | `cmd_eval successful-row metrics` | Printed arguments are the fixed builtin strategy label, integer counts, closed execution state, and strings assembled from numeric probe scores plus fixed labels/flags. No session identifier, transcript text, raw findings or raw errors are formatted here. |
| [#80](https://github.com/hraness/gobstopper/security/code-scanning/80) | `cmd_eval no-plan row` | Only row.strategy and the literal no plan reach stdout. EvalRow.strategy is strat.id().to_string(), selected from builtin_strategies; strategy_by_id returns one of those same builtins. This is not the source session identity. |
| [#30](https://github.com/hraness/gobstopper/security/code-scanning/30) | `cmd_apply experimental resume command` | Explicit experimental copy result prints receipt.session_id in codex resume. It refers to the newly prepared output. Preserve actionable output identity; this does not qualify provider acceptance or native activation. |
| [#36](https://github.com/hraness/gobstopper/security/code-scanning/36) | `cmd_apply portable resume command` | Explicit detached apply receipt prints newly prepared output ID with a fixed Codex/Claude resume prefix. CopyReceipt validation binds session/path/operation before success. No watch caller uses this stdout closure. |
| [#31](https://github.com/hraness/gobstopper/security/code-scanning/31) | `cmd_apply native fork target` | Explicit native apply branch reports the separate fork resume hint. Released builds always refuse activation before fork preparation. Isolated synthetic fixtures are a separate debug-build exception. Retain target receipt; no claim that a live provider cell is qualified. |
| [#81](https://github.com/hraness/gobstopper/security/code-scanning/81) | `cmd_apply experimental recovery reference` | Printed sha is CopyReceipt.snapshot_manifest_sha256, sourced from vault snapshot.sha256. vault.rs 468-475 builds a manifest of raw source SHA, length and chunk SHAs and hashes its serialized bytes; reconcile verifies source roots. It is a requested exact recovery reference, not a cleartext source ID. Treat the reference as private metadata, not universally nonsensitive. |
| [#82](https://github.com/hraness/gobstopper/security/code-scanning/82) | `cmd_apply experimental reclaimed-byte report` | The sole dynamic argument is CopyReceipt.reclaimed_bytes: u64, computed original.len().saturating_sub(output.len()) (copy.rs 377), then validated against before/after byte lengths (copy.rs 95). A session ID can affect output size/admission but its text cannot reach this sink. |

The review also found separate background watch disclosures not represented by
these annotations. Follow-up source changes replace provider paths, copy recovery
references, nested adapter/serde errors, telemetry failures and native checkpoint
errors with fixed categories. Private sentinel regressions reproduce the original
failure and cover load, policy, evaluation, copy and state-decoding routes.
Requested stdout and explicit dry-run previews retain their output contracts;
monitor load/plan failure recognition is preserved.

This document does not claim scanner closure. The latest exact-head scan, source
changes, independent repair reviews and integration results are tracked in
[PR 90](https://github.com/hraness/gobstopper/pull/90). Any output routing or data
source change requires another review. Current enforced rules always take
precedence over a source disposition; the repository's additive security review
never bypasses its required integration or protection gates.

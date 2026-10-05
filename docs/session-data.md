# Local session data

Gobstopper stores request outcomes, reported token usage, context decisions, and tool observations on your computer. The proxy collects request metadata by default; `gobstopper proxy serve --no-session-data` disables that collection. Native transcript imports are explicit commands. No account or hosted service is required.

```sh
gobstopper data status
gobstopper data requests
gobstopper data sessions
gobstopper data tools
gobstopper data metrics
gobstopper data events --limit 1000
gobstopper data check
```

Commands emit JSON. The database lives at `$GOBSTOPPER_DATA_DIR/sessions.sqlite3`, or `$XDG_DATA_HOME/gobstopper/private/sessions.sqlite3`. When neither variable is set, the directory is `~/.local/share/gobstopper/private` on Unix and `%LOCALAPPDATA%\gobstopper\private` on Windows, falling back to `%USERPROFILE%\AppData\Local\gobstopper\private` when `LOCALAPPDATA` is unavailable. Use `data --state-dir /path/to/private-directory` to select another directory. Gobstopper creates that directory with owner-only permissions on Unix and refuses an existing directory or database with broader access.

## Read a selected interval

`sessions`, `requests`, `tools`, `metrics`, `events`, `export`, and `archive` accept `--since-ms`, `--until-ms`, and `--session`. Times are Unix milliseconds. The start is inclusive; the end is exclusive. `--session` takes the opaque ID printed by `data sessions`.

Selection applies to observation timestamps. A request that starts before the selected interval and finishes inside it has a known finish and an unknown start in that selection. Narrow intervals can therefore contain partial lifecycles. The report preserves that distinction.

Requests without a provider or client session identifier remain unassigned. They appear in `data requests` and `attempts_without_session`; Gobstopper does not group them by model name, directory, or timing. Retry attempts share a logical request ID when the proxy observed that relationship, and retain distinct attempt IDs and outcomes.

`data events` returns a versioned object containing sequence-numbered `envelope` records, `snapshot_sequence`, `revision`, `next_after_sequence`, and `complete`. For the next page, pass the returned cursor as `--after-sequence` and the first page's `snapshot_sequence` as `--through-sequence`, repeating the same time and session selection. Appends after that snapshot do not enter later pages. A `null` cursor with `complete: true` means that selection has no more events in the snapshot. These cursors belong to one database; they are not portable event IDs. The `requests` and `tools` outputs remain arrays, and `sessions` retains its session report.

## Understand the measurements

Each observation contains an event version, event ID, source kind and parser profile, timestamp, typed identities, and event payload. Native identifiers are replaced with locally keyed opaque IDs. The retained fields include provider, model and tool labels, numeric counters, timing, and outcomes. Prompts, programs, tool arguments, tool output, images, authorization headers, file paths, and raw session IDs are excluded from the event format.

Metric functions read those observations without modifying them. The output identifies its metric version and keeps source kinds and parser profiles in separate cohorts. **Do not add proxy and native-transcript usage together:** the same provider response can appear in both. There is no inferred cross-source join and no combined usage total.

New proxy observations use `gobstopper-proxy-v2`. Its duration and first-output clock start at forwarding entry, after the client request has been read, and include preparation, upstream waiting, and response transport. Earlier `gobstopper-proxy-v1` observations started their clock after preparation. The two profiles remain separate; their durations and rates are not silently mixed. `upstream_headers_ms` includes connection and provider waiting, not just proxy CPU work. Transformation timing remains absent unless separately measured.

| Measure | Definition |
| --- | --- |
| Reported input and output tokens | Provider-reported counters from observed terminal records; cache-read and cache-write counts are overlapping input details where the provider defines them that way. |
| Request duration | Observed elapsed time for an attempt, including waiting and transport. |
| First output | Observed time to first output, when the response observer has that measurement. |
| Request preparation | Observed elapsed time before starting upstream transport. |
| Upstream headers | Observed elapsed time from starting upstream transport until its header stage ends. |
| Request transform | Separately observed transformation time within preparation; absent when it was not recorded. |
| Request output tokens/s | Sum of reported output tokens divided by sum of complete request durations for the same measured attempts. |
| Generation output tokens/s | Output-token deltas divided by matching generation spans; unavailable when such paired evidence is absent. |
| Context reduction | Estimated context before and after a compaction decision; separate from billed usage. |

Each rate or mean includes `measured`, `missing`, `status`, and exact decimal-string numerators and denominators. Rate durations are stored in milliseconds and converted to seconds for `value`. Missing measurements produce `null`, never a fabricated zero. An unavailable generation rate does not prevent a measured request rate. Weighted cohort rates use the sum of measured durations; they do not average per-request rates or claim wall-clock throughput for overlapping requests.

Outcome fractions use the count of that recorded outcome as their numerator and the number of attempts with a terminal outcome as their denominator. An explicitly recorded `unknown` outcome participates in that breakdown; an absent terminal contributes to `missing`. Finish-reason fractions use only attempts carrying a recorded reason in their denominator. Reasons are fixed codes such as `completed`, `client_disconnected`, or `upstream_read_failed`, never error text. Older terminal records have no inferred reason or stage timings. Stage means preserve absent values as missing and count a recorded zero as measured.

An attempt with a recorded start and no finish remains incomplete after a crash. Gobstopper does not rewrite it as successful, cancelled, or currently running. A terminal record imported without a start retains unknown start time and duration.

Live Anthropic, Responses, and Chat Completions responses supply model-requested tool calls. Those observations retain sanitized tool names and opaque IDs, deduplicate streamed call updates, and attach to the observed request attempt. They record a requested stage and unknown execution outcome: the proxy has not observed the tool executing. Tool arguments and programs are discarded. At most 128 tool calls are retained per response; excess observations contribute to the recorder's dropped count. Native imports can provide separately observed results.

## Import existing observations

```sh
gobstopper data import-native --provider claude /path/to/closed-session.jsonl
gobstopper data import-native --provider codex /path/to/closed-rollout.jsonl
gobstopper data import-legacy /path/to/proxy-stats.jsonl
```

Imports read one file and leave it unchanged. They validate the complete selected file before committing. An incomplete final line, malformed record, conflicting observation, or size limit rejects the import without partially changing the database. Retry once an active writer has finished its record.

The Claude metadata profile imports tool requests and explicit results. It imports usage only from terminal assistant messages that carry an explicit request ID, message ID, and supported stop reason. Inclusive input is unknown if any required input/cache component is missing. The Codex metadata profile imports tool requests and results with explicit session and call IDs. A generic Codex tool output has an unknown outcome unless an explicit supported outcome exists. Cumulative Codex token summaries do not establish individual provider-attempt identities, so this profile does not turn them into requests or throughput measurements.

Parser receipts report unsupported lines. Unsupported records remain absent from metrics. Reimporting the same supported observations into the same database is idempotent. A changed observation with the same identity is rejected for inspection.

Legacy proxy stats carry context-size estimates. Importing them preserves their timestamps and estimates without assigning request IDs, generated-token usage, or generation speed. By default, the source identity comes from the canonical source path, stored only as a keyed opaque ID. When a file has moved, use the same explicit `--source-id LABEL` on both imports to identify one logical source. Choose a different label for distinct log streams. Exact duplicate legacy rows retain their occurrence counts.

## Export, back up, and recover

```sh
gobstopper data export --output observations.jsonl
gobstopper data import observations.jsonl
gobstopper data backup /path/to/new-private-directory/sessions.sqlite3
gobstopper data check
```

An export contains a format/version header, event records, and a count/checksum footer. The importer checks the complete file before a transaction. Exact replay inserts no duplicates; a conflicting identity aborts the batch. Export files retain opaque event and source IDs for reconciliation but exclude the database's secret identity namespace. Importing native files into a separate database creates a separate local namespace; portable imports do not authorize silently joining those namespaces.

For selections larger than one portable export, copy them into a new archive directory:

```sh
gobstopper data archive --output /path/to/new-archive
gobstopper data archive-check /path/to/new-archive
gobstopper data --state-dir /path/to/recovered-private-directory import /path/to/new-archive/segment-000001.jsonl
```

`archive` requires an existing parent and a destination directory that does not exist. It creates owner-only files and a directory on Unix, reads one SQLite snapshot, and leaves the source unchanged. Each `segment-*.jsonl` is a complete portable export accepted by `data import`. A checksummed `manifest.jsonl` lists the segments. Gobstopper writes `complete.json` only after validating every exported segment, the manifest, and the total count. `archive-check` requires that marker and verifies the manifest and every segment before returning `complete: true`; it does not open or initialize a session database.

Import every segment in filename order into a separate recovery directory, then run `data check` there and compare its event count with `archive-check`. An archive omits the secret identity namespace, as a portable export does. Use a SQLite backup when that namespace must survive. A failed or interrupted archive can leave its new directory and finished segments available for inspection. Finished segments from an incomplete archive can be imported individually, but recover only that partial prefix. Only a successful `archive-check` identifies a complete set. Do not treat files with a missing or invalid completion marker as a complete restore or replace the source with them. Corrupt or truncated segments fail verification.

`backup` creates a new SQLite backup, includes committed write-ahead-log data, and verifies its events and indexes. It preserves the identity namespace so future native imports keep their IDs. It refuses to overwrite an existing destination. Use `--state-dir` to inspect the backup before selecting it for future collection. A backup contains private operational metadata even though it excludes conversation content.

SQLite transactions, full synchronization, a write-ahead log, event digests, uniqueness constraints, and versioned migrations protect committed observations. A failed append rolls back its whole batch. Database-full failures preserve earlier records; collection can resume after capacity is restored. Schema upgrades retain observations and identity state. A binary refuses a database from a newer schema. `data check` checks SQLite integrity and compares stored indexes with validated event content. It reports corruption for recovery from a verified backup; it does not reset or delete observations.

The live recorder retains its runtime identity if storage fails, buffers up to 1,024 content-free observations in memory, and retries with exponential delays from one to 60 seconds. One background worker opens the database, appends batches of at most 256 observations, and closes the database. It retries unavailable storage even when no further inference arrives and sleeps while healthy and idle. Its SQLite lock wait is limited to 25 milliseconds; disk operations have no guaranteed completion time, so startup, requests, status and shutdown never wait for that worker. A full or contended queue drops observations and counts the loss. Runtime health reports availability, degraded state, background retry availability, pending observations, failures, recoveries, and dropped observations. Restoring directory permissions or storage capacity permits recovery without restarting inference or resetting data; a failed worker requires a proxy restart. Requests observed before the persistent identity namespace first becomes available keep an unknown session.

Shutdown signals the worker and returns immediately. When storage responds, the worker attempts the remaining queue in at most four batches, stopping after a failure. It owns database cleanup and counts remaining unsaved observations as dropped. Buffered observations can be lost when the process exits, even during an orderly shutdown. Committed observations are durable.

## Limits and capacity

Session, request, and tool array queries contain at most 100,000 observations and 64 MiB of serialized records; a larger selection fails instead of returning a partial report. Full-history metrics stream validated, digest-checked rows through one request at a time and a fixed table of source/profile aggregates, so they do not use that array limit or retain a lifetime request map. SQLite uses a 2 MiB page cache and file-backed temporary storage. Token and duration totals remain exact decimal strings calculated with `u128`; floating-point `value` is a display value.

`events` pages default to 1,000 observations, allow at most 10,000, and stop before exceeding a 64 MiB serialized-record budget. Portable exports, archive segments, and atomic imports each contain at most 10,000 observations and 64 MiB. Archives contain at most 100,000 segments and a 64 MiB manifest. `--segment-events` can lower the per-segment count. Native inputs are limited to 256 MiB, 100,000 lines, and 16 MiB per line, with at most 10,000 supported observations per import. Narrow the selected portable export interval when necessary.

Read operations have a 120,000 ms default deadline. `metrics`, `events`, `archive`, and `archive-check` accept `--timeout-ms` from 1 to 600,000. SQL work is interrupted at the deadline and streaming readers check elapsed time between records. CLI SQLite lock waits are limited to two seconds. Filesystem operations have no guaranteed completion time. A timed-out scan fails instead of returning truncated metrics or claiming a partial archive is complete.

Writes retain a limit of 262,144 pages and 1 GiB, reducing the allowed page count for larger page sizes. `status` reports `write_limit_bytes`, `remaining_capacity_bytes`, `at_write_capacity`, and `capacity_warning`: `near_write_limit` from 90% of the limit, `write_limit_reached` at the limit, or `write_limit_exceeded` above it. Remaining capacity reflects the largest database, logical-page, or journal size; it is not free disk space. Gobstopper does not prune, rotate, or increase capacity automatically.

Read commands open an existing database read-only, without migrations or checkpoints, even when its files exceed the write limit. Inspection, checking, pagination, export, archiving, and backup remain available for recovery; importing and collection still refuse oversized writable state. Read commands report `data_state_unavailable` for an absent database rather than creating empty state or claiming an empty history. Only collection and import initialize a new database. Older SQLite schemas can be inspected and exported without changing them; full-history metrics require the current attempt index and report `data_schema_migration_required` until the normal writable opener migrates the recovered copy. Recover into a separate directory and verify it before selecting it for collection. Keep the original database and its journal together.

## Compatibility

The event format, SQLite schema, source parser profiles, and metric definitions have separate versions. New observations do not require recomputing stored totals because totals are derived from immutable events. This reader accepts older records without reasons or stage timings and preserves their absence. Older releases do not understand the v2 proxy profile or the new optional fields: after rolling back a binary, inspect or restore these observations with a compatible reader. Do not remove fields or rewrite historical data to make a downgrade appear compatible. New event types or changed meanings need a versioned reader and migration; unsupported fields and versions fail explicitly. These observations stay separate from `gobstopper usage`, which reads token totals across your agents from aicharts' local record. Some requests appear in both, so read the two side by side and never add them. Current commands do not import an aicharts database, publish measurements, or treat local checksums as independent provider verification.

# Local session data

Gobstopper stores request outcomes, reported token usage, context decisions, and tool observations on your computer. The proxy collects request metadata by default; `gobstopper proxy serve --no-session-data` disables that collection. Native transcript imports are explicit commands. No account or hosted service is required.

```sh
gobstopper data status
gobstopper data requests
gobstopper data sessions
gobstopper data tools
gobstopper data metrics
gobstopper data check
```

Commands emit JSON. The database lives at `$GOBSTOPPER_DATA_DIR/sessions.sqlite3`, or `$XDG_DATA_HOME/gobstopper/private/sessions.sqlite3`. When neither variable is set, the directory is `~/.local/share/gobstopper/private` on Unix and `%LOCALAPPDATA%\gobstopper\private` on Windows, falling back to `%USERPROFILE%\AppData\Local\gobstopper\private` when `LOCALAPPDATA` is unavailable. Use `data --state-dir /path/to/private-directory` to select another directory. Gobstopper creates that directory with owner-only permissions on Unix and refuses an existing directory or database with broader access.

## Read a selected interval

`sessions`, `requests`, `tools`, `metrics`, and `export` accept `--since-ms`, `--until-ms`, and `--session`. Times are Unix milliseconds. The start is inclusive; the end is exclusive. `--session` takes the opaque ID printed by `data sessions`.

Selection applies to observation timestamps. A request that starts before the selected interval and finishes inside it has a known finish and an unknown start in that selection. Narrow intervals can therefore contain partial lifecycles. The report preserves that distinction.

Requests without a provider or client session identifier remain unassigned. They appear in `data requests` and `attempts_without_session`; Gobstopper does not group them by model name, directory, or timing. Retry attempts share a logical request ID when the proxy observed that relationship, and retain distinct attempt IDs and outcomes.

## Understand the measurements

Each observation contains an event version, event ID, source kind and parser profile, timestamp, typed identities, and event payload. Native identifiers are replaced with locally keyed opaque IDs. The retained fields include provider, model and tool labels, numeric counters, timing, and outcomes. Prompts, programs, tool arguments, tool output, images, authorization headers, file paths, and raw session IDs are excluded from the event format.

Metric functions read those observations without modifying them. The output identifies its metric version and keeps source kinds and parser profiles in separate cohorts. **Do not add proxy and native-transcript usage together:** the same provider response can appear in both. There is no inferred cross-source join and no combined usage total.

| Measure | Definition |
| --- | --- |
| Reported input and output tokens | Provider-reported counters from observed terminal records; cache-read and cache-write counts are overlapping input details where the provider defines them that way. |
| Request duration | Observed elapsed time for an attempt, including waiting and transport. |
| First output | Observed time to first output, when the response observer has that measurement. |
| Request output tokens/s | Sum of reported output tokens divided by sum of complete request durations for the same measured attempts. |
| Generation output tokens/s | Output-token deltas divided by matching generation spans; unavailable when such paired evidence is absent. |
| Context reduction | Estimated context before and after a compaction decision; separate from billed usage. |

Each rate or mean includes `measured`, `missing`, `status`, and exact decimal-string numerators and denominators. Rate durations are stored in milliseconds and converted to seconds for `value`. Missing measurements produce `null`, never a fabricated zero. An unavailable generation rate does not prevent a measured request rate. Weighted cohort rates use the sum of measured durations; they do not average per-request rates or claim wall-clock throughput for overlapping requests.

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

`backup` creates a new SQLite backup, includes committed write-ahead-log data, and verifies its events and indexes. It preserves the identity namespace so future native imports keep their IDs. It refuses to overwrite an existing destination. Use `--state-dir` to inspect the backup before selecting it for future collection. A backup contains private operational metadata even though it excludes conversation content.

SQLite transactions, full synchronization, a write-ahead log, event digests, uniqueness constraints, and versioned migrations protect committed observations. A failed append rolls back its whole batch. Database-full failures preserve earlier records; collection can resume after capacity is restored. Schema upgrades retain observations and identity state. A binary refuses a database from a newer schema. `data check` checks SQLite integrity and compares stored indexes with validated event content. It reports corruption for recovery from a verified backup; it does not reset or delete observations.

The live recorder retains its runtime identity if storage fails, buffers up to 1,024 content-free observations in memory, and retries on later requests with exponential delays from one to 60 seconds. Its database lock wait is limited to 25 milliseconds, with one batch per collection call. Runtime health reports availability, degraded state, pending observations, failures, recoveries, and dropped observations. Restoring directory permissions or storage capacity permits recovery without restarting inference or resetting data. Buffered observations can be lost in a process crash; committed observations are durable. Graceful shutdown makes one final batch attempt and reports anything still unsaved. Requests observed before the persistent identity namespace first becomes available keep an unknown session.

Local queries are limited to 100,000 observations. Exports and atomic imports contain at most 10,000 observations and 64 MiB. Native inputs are limited to 256 MiB, 100,000 lines, and 16 MiB per line, with at most 10,000 supported observations per import. Narrow the selected export interval when necessary. The database has a page limit of 262,144 pages (1 GiB at the default 4 KiB page size) and refuses oversized existing state. It does not silently prune old observations.

## Compatibility

The event format, SQLite schema, source parser profiles, and metric definitions have separate versions. New observations do not require recomputing stored totals because totals are derived from immutable events. New event types or changed meanings need a versioned reader and migration; unsupported fields and versions fail explicitly. This format can support a future AI Charts exporter. Current commands do not import an AI Charts database, publish measurements, or treat local checksums as independent provider verification.

# Plugin protocol

Gobstopper plugins are explicitly trusted, versioned subprocess bundles. They are extension boundaries, not security sandboxes. The host keeps source ownership, policy, validation, snapshots, publication, and provider controls.

## Roles

A manifest declares one or more closed capabilities:

- `strategy`: receives an already normalized built-in transcript and proposes `Edit[]`.
- `provider_read`: inspects a bounded source file and returns normalized items and usage. It is read-only and must return no edits.
- `read_content`: allows source lines or normalized summaries to be included for an operation that also declares it.

A provider plugin does not grant Gobstopper permission to rewrite that provider's files. External agents should expose native compaction through `policy-check`/MCP and use `provider_read` for exported or idle analysis.

## Manifest

The JSON manifest is limited to 64 KiB and rejects unknown fields. Example shape:

```json
{
  "protocol_version": 1,
  "id": "example-atif-reader",
  "version": "1.0.0",
  "executable": "reader.py",
  "files": {
    "reader.py": "<sha256-of-reader.py>"
  },
  "args": [],
  "capabilities": ["provider_read", "read_content"],
  "provider_ids": ["devin-atif"],
  "timeout_ms": 5000,
  "max_input_bytes": 2097152,
  "max_output_bytes": 1048576,
  "environment": []
}
```

The executable and every runtime artifact must be a declared regular file below the manifest directory. Paths are relative and cannot traverse upward. Bundles are limited to 64 declared files, 256 directory entries, eight levels, and 16 MiB total. The child receives a cleared environment except for explicitly named variables.

Run admission without executing code:

```sh
gobstopper plugin check /absolute/path/gobstopper-plugin.json
```

The result prints `manifest_sha256`. Pin that exact value in configuration or pass it to `plugin inspect`. Any manifest or declared-file change requires review and a new trust hash. Gobstopper rechecks identity after execution.

## Request

The plugin reads one JSON object from stdin:

```json
{
  "protocol_version": 1,
  "operation": "provider_read",
  "provider_id": "devin-atif",
  "source_sha256": "<64 hex characters>",
  "items": [],
  "usage": {
    "context_tokens": 0,
    "lifetime_input_tokens": 0,
    "lifetime_cached_tokens": 0,
    "model_context_window": null
  },
  "policy": null,
  "content": ["{", "  ...", "}"]
}
```

Input is capped by both the host maximum (2 MiB) and the manifest's lower limit. `content` appears only when `read_content` is declared. Treat its lines as a bounded representation of the source document, not as a guaranteed JSONL record mapping.

For `strategy`, `items` contains normalized `TranscriptItem` values and `policy` contains the resolved policy. Plugins should defer with an empty `edits` array.

## Response

Return exactly one JSON object on stdout:

```json
{
  "protocol_version": 1,
  "source_sha256": "<same digest as request>",
  "edits": [],
  "inspection": {
    "provider_id": "devin-atif",
    "session_id": "bounded-session-id",
    "items": [],
    "usage": {
      "context_tokens": 0,
      "lifetime_input_tokens": 0,
      "lifetime_cached_tokens": 0,
      "model_context_window": null
    }
  }
}
```

Unknown fields fail admission. `source_sha256` must exactly echo the request binding. A `provider_read` response requires one inspection and no edits; a `strategy` response may contain edits and no inspection.

Normalized inspection items must be strictly increasing and unique, with logical indexes below 100,000. Labels, summaries, UUID-like fields, tool-use IDs, payload hashes, usage, elidable byte counts, and part counts are independently bounded. Logical indexes support whole-document formats such as ATIF.

## Strategy admission

Strategy proposals may use `elide` and `inject_digest`. External plugins cannot dispatch `provider_compact` or Claude `cache_edit` controls. The host rejects:

- unknown, duplicate, out-of-range, or protected-tail indexes;
- non-elidable targets or mixed provider/file controls;
- oversized, empty-coverage, or malformed state cards;
- edits that do not reduce projected context by `min_savings_tokens`;
- candidates that add structural verification findings.

Standalone application still snapshots the exact source and publishes a separate no-clobber fork.

## Resource and failure contract

Execution is bounded to 30 seconds, 2 MiB input, and 1 MiB each for stdout and stderr, with lower manifest limits encouraged. The process runs in its own process group; timeout, output overflow, non-zero exit, invalid JSON, identity drift, or malformed output kills/reaps the process and fails closed. No retry is automatic.

## Contributions

A new provider integration should include:

1. a stable lowercase provider ID;
2. fixtures for empty, malformed, maximum-size, forked/branched, and representative exports;
3. explicit token-accounting semantics;
4. deterministic logical item ordering and stable labels;
5. timeout/output-bound tests and malformed-response tests;
6. documentation of the provider-owned native compaction control;
7. no direct mutation until a separate, reviewed write protocol exists.

Built-in adapter proposals must additionally preserve provider resume invariants and pass the workspace regression/property suite. XCB or any other adjacent runtime has no implicit privileged seam: use numeric policy/MCP or this bounded plugin protocol unless a concrete provider-owned contract is documented and tested.

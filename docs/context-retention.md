# Keeping observations through compaction

Gobstopper's request proxy keeps a limited collection of original tool results in its compacted history. This helps an agent continue using evidence it has already gathered when the surrounding conversation grows. The client keeps its original transcript; the proxy changes the outgoing request.

Short results keep their text. Longer results keep the beginning and end, with an explicit excerpt label and the number of omitted characters. Each result carries its source call identifier and a SHA-256 digest of the complete original content. Repeated identical results update the source identifier without occupying another place in the collection. Changed results have different digests.

Each observation also keeps the matching original tool name (up to 128 characters) and arguments or freeform input (up to 1,024 characters). Longer invocations use labeled excerpts; an unavailable invocation is stated explicitly. This provenance survives later compactions, so a retained file excerpt can still identify its path. It shares the existing byte and token limits and stays in ephemeral request summaries, never the observation journal. Invocation completeness is separate from result completeness.

Supported images remain native image blocks in the summary when both size limits allow them. Anthropic image blocks, Responses `input_image` blocks, and Chat Completions `image_url` blocks use their respective provider formats. An omitted image or unsupported document gets an omission notice. A digest identifies that original result; the model cannot use the digest to see an omitted image.

Images are grouped after the retained text. When that changes the original interleaving of images and text, the result is labeled partial and explicitly states that positional context was lost. Remote image references remain references: identical URLs or file identifiers cannot prove identical image bytes and never count as unchanged-image evidence for automatic rescue.

## Retention limits

The request-engine library exposes these `CliffConfig` controls:

| Setting | Default | Behavior |
| --- | ---: | --- |
| `evidence_max_bytes` | 262,144 | Maximum serialized UTF-8 bytes of kept observations. Zero disables retention. |
| `evidence_max_chars` | 32,000 | Maximum billable characters, approximately 8,000 estimated tokens, including image cost. Also limited to one quarter of available request space above fixed instructions and the initial messages. |
| `evidence_item_max_chars` | 2,000 | Maximum text characters per result before an excerpt is used. Zero disables retention. |

At most 128 observations are kept. When the collection is full, older observations leave first. These limits apply to the extra observation collection; results in the newest unmodified turns also remain available. The default `keep_tail_percent` is zero, which keeps the configured number of recent turns without expanding that portion of the request.

Conversation text uses a separate `carry_max_chars` limit. Visible assistant notes and user instructions can therefore survive alongside tool evidence. Both collections can lose older material when their limits are reached. The final emergency compaction step may remove the observation collection to meet a rejected request's size limit.

The prefix cache counts serialized UTF-8 bytes for the summary, its retained content, metadata, and cache key. An entry that exceeds the cache limit is discarded, including when it would be the only entry. The next request can reconstruct its summary from the history the client sends.

## Context budgets and request identity

`Engine::prepare_with_policy` accepts a caller's policy identifier and an optional supported input capacity, after reserving room for output and thinking. The engine applies token-estimate calibration and prevents its initial-message allowance from raising the threshold above that capacity. `RequestCtx::capacity_exceeded_by_floor` indicates that the unchanged initial messages and fixed fields already exceed it. `over_budget` reports a request that remains too large after compaction.

A versioned fingerprint covers retention settings, the caller's policy identifier, capacity, calibration, dialect, threshold, and fixed request fields. Changing them rebuilds the summary from original history instead of substituting a cached summary computed under another policy. Freeform tool-call input and nested image content participate in message identity, including MIME type, image detail, and audio format where applicable.

`RequestCtx` exposes complete, excerpted, and evicted result digests separately. A result is considered evicted for this report when the compacted region contains it and neither a complete kept observation nor the unmodified tail contains an identical result. `evidence_observations` returns result digests, kinds, and source identifiers for inspectable text and inline-image observations so callers can identify unchanged rereads. Results with remote images or unsupported media are excluded from that automatic-rescue evidence. Anthropic block-level cache hints do not change the result digest; tool data remains semantic. Provider call identifiers are client-supplied data and should not be copied into public logs.

## Recovery and limits

The observation collection and prefix cache live in memory. Restarting the proxy rebuilds them from the original history on the next request. With the same request policy and replay budget, successive merges and a fresh replay preserve the same observations. The proxy cannot recover content that a client has already removed through its own native compaction.

Retention tests cover repeated compactions, fresh reconstruction, excerpts, native image blocks, cache invalidation, request capacity, and both byte and estimated-token limits. Those tests establish which information reaches the outgoing request. Task completion, model accuracy, and reduced rereading require separate evaluation on representative agent tasks.

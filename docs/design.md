# gobstopper design

## Thesis

Provider auto-compaction fires at ~90% of the context window — too late.
Every turn before that point pays the full context size as input tokens,
and every turn in the tail pays for degraded recall ("context rot").

gobstopper treats compaction as a *policy + strategy* problem:

- **when** to compact (trigger policy — threshold, boundary detection,
  rate limits)
- **how** to compact (strategy — provider delegate, elision, structured
  digest, agentic editing)
- **where** to apply it (provider control plane vs. transcript file)

## Research basis

- **Context rot** (Anthropic, "Effective context engineering"): recall
  degrades as token count grows. Context is a finite attention budget.
- **SelfCompact** (arXiv:2606.23525): model-chosen compaction timing beats
  fixed-interval triggers at 30–70% lower cost — but only when the
  scaffold supplies both a compaction *tool* and a *rubric*. This is why
  `agentic` falls back to `auto`'s rubric rather than firing unguided.
- **Compaction formalization** (arXiv:2608.01326): selection vs.
  generation strategies; generation is strictly more expressive. `elide`
  is selection; `structured`/`agentic` are generation.
- **Observation masking** (SWE-agent / OpenHands condenser line of work):
  stale tool outputs are the cheapest thing to lose — their conclusions
  live in surrounding assistant text.
- **Codex operational guidance**: ~60% of effective window is the
  recommended `model_auto_compact_token_limit` for long sessions —
  evidence that earlier-than-default compaction is provider-endorsed.

## Provider levers (historical version-specific observations)

### Codex (0.153.2, app-server v2)

- `thread/compact/start {threadId}` — ClientRequest, forces compaction of
  a live thread.
- `thread/tokenUsage/updated` — ServerNotification carrying
  `ThreadTokenUsage { total, last, modelContextWindow }`.
- `model_auto_compact_token_limit` — config.toml key, clamped at 90% of
  `model_context_window`; lowering it is supported.
- `model_auto_compact_token_limit_scope` — `total` | `body_after_prefix`.
- `compact_prompt` — custom compaction instructions, config-level.
- `tool_output_token_limit` — per-output cap, config-level.
- `thread/inject_items` — push items into a thread (digest injection path).
- Rollout store: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` with
  `session_meta`, `response_item`, `token_usage_record`, and `compacted`
  records. `compacted` carries `replacement_history` — the post-compaction
  context Codex rebuilds from on resume.

### Claude Code (2.1.x)

- `--autocompact <auto|tokens>` — argv, 100k–1M window override.
- `/compact [instructions]` — in-session command.
- `PreCompact` hook — fires before native compaction; strategy steering
  point.
- Session store: `~/.claude/projects/<cwd-slug>/<session>.jsonl`, a
  `uuid`/`parentUuid` tree — only the latest leaf's branch is live
  context (gobstopper computes the live branch; dead branches are never
  counted or touched). Assistant lines carry `message.usage`.
- Usage: `input + cache_read + cache_creation` ≈ context occupancy.

## The Edit IR

Every strategy lowers to four primitives:

```rust
enum Edit {
    Elide { line_indexes, stub_template, per_item_stubs },
    InjectDigest { digest },
    ProviderCompact { control },
    CacheEdit { tool_use_ids },
}
```

`Elide` and `InjectDigest` are file-candidate operations. `ProviderCompact`
and `CacheEdit` are control-plane proposals and cannot be mixed with file
edits. Rewrites never remove source records; Claude `parentUuid` chains and
Codex ordinal/window/tool-pair invariants are verified before a candidate is
published as a separate fork. `per_item_stubs` optionally carries complete
one-line stub text per line index (e.g. a model-written breadcrumb); absent
entries render `stub_template` with `{bytes}`/`{kind}` substitution as
before.

## Strategy selection (`auto`)

```
live Claude with valid tool ids  -> cache_edits
other live/empty transcript      -> sawtooth provider delegate
idle transcript                  -> best validated file plan by
                                    savings × preserved-prefix³
```

Idle candidates include `cache_aware`, `scored`, `elide`, `compacted`,
`dedupe`, `micro`, `middle`, and `structured`. Plans below
`min_savings_tokens` are rejected centrally. `agentic` uses the same host
validation and protected-tail rules as built-ins; an empty proposal means
defer.

## Oompa integration

oompa owns live managed sessions (its spawned app-servers, its Claude
processes). It must never parse provider transcripts — so the seam is
numeric, not textual:

- `gobstopper policy-check --provider <p> --context-tokens <n>
  --session-active --json` → `{action, strategy, control}`.
  Pure function of resolved config + two numbers.
- oompa executes `provider_compact` itself on its own connections
  (`thread/compact/start` for Codex; `--autocompact` argv for Claude at
  spawn; `/compact` injection if the stream-json dialect ever admits it).
- Transcript-path strategies apply to *idle* sessions (resume-time
  context swap) and to non-managed sessions oompa doesn't own.

The deeper integration on the roadmap: gobstopper emits a `compacted`
record with a custom `replacement_history` — the provider's own
resume mechanism performing a fully custom compaction. Fields are
understood (`window_id`, `first/previous_window_id`, `guardian_history`,
`latest_token_usage_record`); correctness requires replaying the window
chain faithfully, so it is gated as experimental until validated.

See `docs/roadmap.md` for the full phased plan: transcript surgery +
undo vault, the oompa `session.compact` effect path, XCB's native
`gobstopper-core` projection and XCB-compatible editor backend, the
aicharts measurement loop, and the proposed
`transcript-foundation` shared crate.

## External strategies and providers

The preferred extension surface is a versioned plugin manifest with an exact
trusted manifest SHA-256, content-addressed bundle files, closed capabilities,
cleared environment, and bounded stdin/stdout/deadline. Strategy plugins
receive normalized items and propose `Edit[]`; provider plugins perform
inspection of bounded source bytes without returning edit proposals, and may use logical record indexes
for whole-document formats such as Devin ATIF. The host validates every edit.
The protocol does not prevent a trusted subprocess from performing other OS
effects. Legacy `preset.command` remains available only with
`trusted_legacy_command = true` and has no sandbox guarantee.

## Failure and safety posture

- Standalone JSONL `apply` and `undo` are copy-only by default; retired in-place
  and no-backup CLI flags fail visibly. Devin `undo` instead restores selected
  payloads to its session store after the idle check; it shares the unresolved
  ownership and current-state binding limits described in the audit.
  Watch's provider-scoped exceptions are
  opt-in: `[provider.devin] auto_apply_store` lets `watch --provider devin`
  run the guarded SQLite write on idle sessions, and
  `[provider.claude_code] auto_apply_inplace` lets `watch --provider
  claude_code` rewrite an observed-idle transcript in place. These direct-write
  modes still have an ownership gap: a provider can begin writing after an idle
  check, and a final reread followed by rename is not compare-and-swap. Opening
  the transcript per write does not remove that race. See the
  [correctness audit](correctness-audit.md) and its custody remediation. Watch auto-apply
  respects `[rollout]` cohorts — control sessions are logged and skipped —
  and a per-session fingerprint suppresses re-evaluation of unchanged
  sources; Claude writes also require the fingerprint to be stable across
  two consecutive passes before mutating. A successful in-place mutation
  additionally holds the session out for `apply_hold_secs` (default 1800)
  so append-over-trigger churn cannot re-apply every interval, and each
  watch pass services sessions in ascending size order so one giant
  apply cannot starve the rest. Suppression fingerprints, settle state,
  and rate-limit clocks persist per-provider to `watch-state-*.json`
  after each pass, so a daemon restart resumes rather than re-planning
  every session once.
- Claude liveness also checks provider markers rather than only mtime: records in
  `~/.claude/sessions/<pid>.json` map live pids to session ids, so a
  session open-but-quiet in a TUI is detected as provider-owned and excluded
  from surgery. This observation is not a lifetime ownership lock.
  With `[provider.claude_code] auto_compact_closed`, a settled
  over-trigger session with *no* live owner is instead compacted by the
  provider itself — `claude --resume <id> -p /compact` — which runs
  Claude's own summarization and fires the `PreCompact` hook where the
  vault snapshot lands. A failed or uncertain native attempt must not fall
  through to direct file mutation. The
  prompt-policy hook adds a last-rung `block_tokens` ceiling (default 0
  = off): at/above it, treatment sessions get `decision: "block"` until
  `/compact` runs. Slash-command prompts are never advised or blocked —
  they are provider UI control, including the headless `/compact` run.
- Before publication, exact source bytes are stored as verified, deduplicated
  1 MiB chunks in the content-addressed vault.
- Participating Rust snapshot/copy/`read_object` operations share custody of the vault directory inode;
  prune holds exclusive custody through reachability analysis, index publication
  and deletion. Legacy multi-object inspection and independent-reader paths
  still require the C3 custody audit. Operation receipts pin their recovery objects. The bounded
  [TLA+ model](../verify/vault/README.md) checks this concurrency protocol and
  negative controls, with explicit exclusions for Rust refinement and filesystem
  crash durability. Current custody support is Unix-only and fails closed on
  other platforms.
- Copy operations bind canonical source path, source hash, provider, and edits
  into a durable intent receipt. Existing targets are never overwritten, and
  incomplete receipts reconcile only against an exact output hash.
- Candidate writes use same-directory private temporary files, compare the
  source again before atomic publication, preserve restrictive permissions,
  sync data/directories, and reject newly introduced verification findings.
- Files are capped at 512 MiB (override via `GOBSTOPPER_MAX_TRANSCRIPT_BYTES`,
  a byte count) and 100,000 records; plugin inputs, outputs,
  manifests, bundles, deadlines, and discovery counts are independently
  bounded.
- Watch mode rate-limits per session (`min_interval_secs`), and plans below
  `min_savings_tokens` are treated as no-ops.
- Native controls delegate writes to the provider. A liveness observation alone
  cannot prove exclusive session ownership; version-specific qualification and
  the remaining direct-write exceptions are tracked in the correctness plan.

## Measured findings (realized audit, offline)

Evidence from `scripts/retention-audit.py` over vault snapshot pairs plus
`scripts/provider-retention-probe.py` synthetic runs. Retention = text
presence in live records, not semantic equivalence.

- Three provider compaction shapes: Codex rewrites history verbatim
  (`replacement_history`), Claude writes a `compact_boundary` plus paraphrased
  summary, Devin appends an additive `summarized_from` summary node that
  shortcuts the live chain.
- Literal vs lexical: Claude summary pairs score ~0-10% literal but ~25-50%
  lexical (>=75% of a check's content tokens in one slot). Codex tracks
  lit~lex (~60-95%). Devin sits between (~5-50% literal, ~60-90% lexical).
- Constraint-class content is the worst-retained kind in Claude summaries
  (per-kind lexical counts), behind procedures and facts.
- Authority loss, not content loss: in constraint-heavy synthetic probes the
  summary preserved rules verbatim but the model then treated them as
  "adversarially-supplied" and refused recall (3 seed framings). Through the
  pinned channel (`--append-system-prompt`) the same rules were echoed
  verbatim, enforced (`migration_allowed: false`), and answered normally.
- Design implication: provenance is the fragile dimension. Digest-style
  compaction carries text but strips provenance (`source_bound` drops to
  plain-masking levels in `eval-study`); pinned items should stay in original
  provider records/roles rather than move onto a summary card.
- Pairing note: hook labels do not bracket the boundary write; pairing keys
  on byte-level mutation markers (`compact_boundary`, `"type":"compacted"`,
  `"summarized_from":[0-9]`).

# gobstopper design

## Thesis

Providers compact near the top of the context window; Codex, for example,
caps its auto-compact limit at 90% of the window. Every turn before that
point pays for the full context as input tokens, and turns near the limit
also get degraded recall ("context rot").

gobstopper treats compaction as a *policy + strategy* problem:

- **when** to compact (trigger policy: threshold, boundary detection,
  rate limits)
- **how** to compact (strategy: provider delegate, elision, structured
  digest, agentic editing)
- **where** to apply it (provider control plane vs. transcript file)

## Research basis

- **Context rot** (Anthropic, ["Effective context engineering for AI
  agents"](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents),
  2025): recall degrades as the token count grows, so context is a finite
  "attention budget".
- **SelfCompact** (Li et al., ["Self-Compacting Language Model
  Agents"](https://arxiv.org/abs/2606.23525), 2026): letting the model decide
  when to compact matches or exceeds fixed-interval summarization, at 30–70%
  lower per-question cost in the authors' benchmarks, and it needs both a
  compaction *tool* and a *rubric*. This is why `agentic` falls back to
  `auto`'s rubric when no editor is configured.
- **Context Compaction Theory** (Tirmazi et al.,
  [arXiv:2608.01326](https://arxiv.org/abs/2608.01326), 2026): models
  compaction as selecting part of the state or generating a bounded message,
  and proves that for some query sets generation needs strictly less budget
  than selection. `elide` is selection; `structured` and `agentic` are
  generation.
- **Observation masking** (the SWE-agent and OpenHands condenser line of
  work): stale tool outputs are the cheapest thing to lose, because their
  conclusions live in the surrounding assistant text.

## Provider levers (verified against pinned versions)

### Codex (0.153.2, app-server v2)

- `thread/compact/start {threadId}`: a ClientRequest that forces compaction
  of a live thread.
- `thread/tokenUsage/updated`: a ServerNotification carrying
  `ThreadTokenUsage { total, last, modelContextWindow }`.
- `model_auto_compact_token_limit`: a config.toml key, clamped at 90% of
  `model_context_window`; lowering it is supported.
- `model_auto_compact_token_limit_scope`: `total` or `body_after_prefix`.
- `compact_prompt`: custom compaction instructions, set in config.
- `tool_output_token_limit`: a per-output cap, set in config.
- `thread/inject_items`: pushes items into a thread (the digest injection path).
- Rollout store: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` with
  `session_meta`, `response_item`, `token_usage_record`, and `compacted`
  records. `compacted` carries `replacement_history`, the post-compaction
  context Codex rebuilds from on resume.

### Claude Code (2.1.x)

- `--autocompact <auto|tokens>`: a command-line window override, 100k–1M.
- `/compact [instructions]`: the in-session command.
- `PreCompact` hook: fires before native compaction, where a strategy can
  steer it.
- Session store: `~/.claude/projects/<cwd-slug>/<session>.jsonl`, a
  `uuid`/`parentUuid` tree in which only the latest leaf's branch is live
  context. Gobstopper computes the live branch and never counts or touches
  dead branches. Assistant lines carry `message.usage`.
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

## Integrating with a session runtime

A program that runs live sessions (its own app-server connections or
Claude Code processes) should not have to parse provider transcripts, so the
interface is numeric:

- `gobstopper policy-check --provider <p> --context-tokens <n>
  --session-active --json` returns `{action, strategy, control}`. It is a
  pure function of the resolved config and the numbers passed in.
- The runtime executes `provider_compact` itself on its own connections:
  `thread/compact/start` for Codex, or `--autocompact` when it launches
  Claude Code.
- Transcript-path strategies apply to idle sessions (a context swap at
  resume) and to sessions that no runtime holds.

This interface was designed for OOMPA, a session runtime that was retired on
2026-09-19 and replaced by xcb. xcb embeds `gobstopper-core` as a library
instead; see `docs/plugin-protocol.md`.

A deeper option is a Gobstopper-written `compacted` record with a custom
`replacement_history`, so Codex's own resume mechanism performs a fully
custom compaction. Gobstopper understands the fields (`window_id`,
`first/previous_window_id`, `guardian_history`,
`latest_token_usage_record`). Correctness requires replaying the window
chain faithfully, so this path is experimental and runs only with
`gobstopper apply --experimental-compacted`.

See `docs/roadmap.md` for the phased plan and its history: transcript
surgery and the undo vault, the retired OOMPA `session.compact` effect path,
xcb's native `gobstopper-core` projection and its compatible editor backend,
the AI Charts measurement loop, and the proposed `transcript-foundation`
shared crate.

## External strategies and providers

The preferred extension surface is a versioned plugin manifest with an exact
trusted manifest SHA-256, content-addressed bundle files, closed capabilities,
cleared environment, and bounded stdin/stdout/deadline. Strategy plugins
receive normalized items and propose `Edit[]`; provider plugins perform
read-only inspection of bounded source bytes and may use logical record indexes
for whole-document formats such as Devin ATIF. The host validates every edit.
Legacy `preset.command` remains available only with
`trusted_legacy_command = true` and has no sandbox guarantee.

## Failure and safety posture

- Standalone `apply` and `undo` are copy-only by default; retired in-place
  and no-backup CLI flags fail visibly. Provider-scoped exceptions are
  opt-in: `[provider.devin] auto_apply_store` lets `watch --provider devin`
  run the guarded SQLite write on idle sessions, and
  `[provider.claude_code] auto_apply_inplace` lets `watch --provider
  claude_code` rewrite an idle transcript in place (Claude opens the file
  per write, so the swap cannot orphan provider appends). Watch auto-apply
  respects `[rollout]` cohorts (control sessions are logged and skipped),
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
- Claude liveness is authoritative, not mtime-inferred: records in
  `~/.claude/sessions/<pid>.json` map live pids to session ids, so a
  session open-but-quiet in a TUI is still provider-owned and never
  mutated. With `[provider.claude_code] auto_compact_closed`, a settled
  over-trigger session with *no* live owner is instead compacted by the
  provider itself (`claude --resume <id> -p /compact`), which runs
  Claude's own summarization after Gobstopper snapshots the session to the
  vault; in-place elision is the fallback. The
  prompt-policy hook adds a last-rung `block_tokens` ceiling (default 0
  = off): at/above it, treatment sessions get `decision: "block"` until
  `/compact` runs. Slash-command prompts are never advised or blocked,
  because they are provider UI controls, including the headless `/compact` run.
- Before publication, exact source bytes are stored as verified, deduplicated
  1 MiB chunks in the content-addressed vault.
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
- Live provider processes stay under their own runtime's control.
  Gobstopper proposes native controls but never writes to a session while
  its runtime holds it.

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

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

## Provider levers (verified against pinned versions)

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

Every strategy lowers to three primitives:

```rust
enum Edit {
    Elide { line_indexes, stub_template },   // in-place payload stub
    InjectDigest { digest },                  // appended state card
    ProviderCompact { control },              // delegate to provider
}
```

Rewrites never remove lines. Claude `parentUuid` chains and Codex
`ordinal` order stay intact; only payloads shrink. That keeps applied
transcripts resumable and auditable, and makes backups trivial.

## Strategy selection (`auto`)

```
tool_result tokens / context ≥ 55%  -> elide
live session or empty transcript    -> sawtooth (provider delegate)
otherwise                           -> structured
```

`agentic` replaces this rubric with an editor model when an
`EditorDriver` is configured; until then it defers to `auto`, per the
SelfCompact finding that an unguided compaction tool is unreliable.

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

## Presets as userspace code

`preset.command` runs a user program: normalized transcript JSON on
stdin, `{edits: [...]}` on stdout. Language-agnostic, sandboxable, and the
lowest-friction way to let users experiment with strategies without
shipping them. A future `EditorDriver` implementation is the same seam
with an LLM behind it.

## Failure and safety posture

- Every apply writes `<file>.gobstopper-bak-<ts>` first (unless
  `--no-backup`).
- Watch mode rate-limits per session (`min_interval_secs`).
- Elision is additive-loss only: stubs record the original byte size.
- `policy-check` and `plan --json` are stable machine surfaces.
- Provider delegation (`sawtooth`) is always preferred for live
  sessions — transcript surgery on a running process's file is refused
  by `auto` and only done explicitly by the user via `apply`.

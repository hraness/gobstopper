# gobstopper design

## Thesis

Long sessions retain stale output alongside constraints and unfinished work.
Earlier compaction is a policy hypothesis to measure, not a guarantee of lower
bills or better recall. Provider caching, summary costs and retrieval work can
change the result.

gobstopper treats compaction as a *policy + strategy* problem:

- **when** to compact (trigger policy: threshold, boundary detection,
  rate limits)
- **how** to compact (strategy: provider delegate, elision, structured
  digest, agentic editing)
- **where** to apply it (provider control plane vs. transcript file)

The current artifact supports inspection and separate Codex/Claude copy
preparation. Released CLI native dispatch is blocked pending qualification;
direct provider-file/store writes are disabled. The
[activation matrix](assurance/qualification.json) and
[recovery runbook](assurance/operations.md) are authoritative for enabled modes.

## Historical research motivation

These notes motivated the strategies; they are not correctness or current
provider-qualification evidence for this implementation.

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
- **CliffCompaction** (Nguyen, Cho, Chen and Dettmers,
  [arXiv:2609.26779](https://arxiv.org/abs/2609.26779), 2026): an API proxy
  that keeps the head and the last `K` turns verbatim, keeps tool results of
  at most 500 characters, drops longer ones, reduces tool calls to
  signatures, and rebuilds every compaction from the original history so a
  compaction is never compacted again. The authors report up to 50% lower
  cost at a bounded context with maintained or improved Terminal-Bench 2.0
  results on the Kimi and GLM models they tested. `cliff` applies the drop
  rule and the protected head and tail to a transcript copy; the vault, not
  the copy, holds the originals. When both passes produce a plan at the same cut,
  the records dropped from the source and then from a `cliff` copy are, together, the records one compaction from the source would drop; one unit test
  checks a synthetic case, and trigger or minimum-savings gating on the smaller copy can make the paths differ. Tool-call signatures and reasoning caps are outside the copy
  transform, which replaces tool-result payloads only.

## Request-time compaction (`gobstopper proxy`)

File compaction cannot get ahead of a running client's own compaction: the
client resends the history it holds in memory, so a rewritten file changes
nothing until a resume. CliffCompaction's answer is to sit in the request
path. Claude Code accepts `ANTHROPIC_BASE_URL`, and Codex accepts a
`model_providers` entry, so a loopback proxy sees every request before the
provider does. Once the proxy compacts, the provider reports the compacted
size and the client's auto-compaction does not reach its trigger.

- `gobstopper_adapters::request` is the pure engine: dialect digests that
  ignore volatile fields (`cache_control`, thinking signatures, Responses
  item `id` and `status`), a hash chain over the original messages, an LRU
  prefix store, the cliff step, the replay of threshold crossings, the
  escalation steps, and image pricing by dimensions. It is a port of the
  reference implementation, which is MIT-licensed (notice in
  `THIRD_PARTY_NOTICES.md`).
- Two behaviors differ from the reference. A rewritten request the provider
  rejects for a reason other than length is resent in its original form.
  When the verbatim floor (fixed request fields plus the head) approaches
  the threshold, the applied threshold becomes the floor plus half the
  configured value. Replays of Codex sessions that Codex had already
  compacted itself showed why: their heads were near 160k estimated tokens,
  and without the adjustment nearly every request compacted again and no
  prefix was reused.
- `crates/gobstopper-cli/src/proxy.rs` owns the socket side: a thread per
  connection on 127.0.0.1, a host check against DNS rebinding, and the
  system `curl` for upstream HTTPS, as the model scorers already use.
  Request headers reach curl as `--variable`/`--expand-header` environment
  values, so tokens stay out of argv. Responses are relayed as they arrive.
- `gobstopper proxy replay` rebuilds the request stream of a recorded Claude
  Code or Codex session and runs it through the engine, with a pairing check
  that separates breakage already in the recording (interrupted or rewound
  turns) from breakage the proxy would introduce.

## Provider levers (historical version-specific observations)

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
edits. Payload transforms retain source record order; supported Claude parent
links and Codex ordinal/window/tool-pair findings are checked before a candidate is
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
`dedupe`, `micro`, `middle`, and `structured`. `cliff` is not an `auto`
candidate: it has no floor and its yield depends only on the size rule, so
it is chosen explicitly or through a preset. Plans below
`min_savings_tokens` are rejected centrally. `agentic` uses the same host
validation and protected-tail rules as built-ins; an empty proposal means
defer.

## Integrating with a session runtime

A program that runs live sessions (its own app-server connections or
Claude Code processes) should not have to parse provider transcripts, so the
interface is numeric:

- `gobstopper policy-check --provider <p> --context-tokens <n>
  --session-active --json` returns `{action, strategy, control}` from the
  resolved configuration and explicit numeric inputs.
- The runtime must establish control of the selected session and test the
  provider operation before executing it on its own connection. A policy
  response performs no compaction.
- File strategies prepare separate candidates. Observed idleness does not
  authorize replacing a provider file.

This interface was designed for the session runtime that preceded xcb,
retired on 2026-09-19. xcb embeds `gobstopper-core` as a library
instead; see [the plugin protocol](plugin-protocol.md).

Ordinary copies use the portable digest form. Structural tests cover supported
window and tool-pair shapes; acceptance by a particular provider version
requires separate resume testing.

A deeper option is a Gobstopper-written `compacted` record with a custom
`replacement_history`, so Codex's own resume mechanism performs a fully
custom compaction. Gobstopper understands the fields (`window_id`,
`first/previous_window_id`, `guardian_history`,
`latest_token_usage_record`). Correctness requires replaying the window
chain faithfully, so this path is experimental and runs only with
`gobstopper apply --experimental-compacted`.

See `docs/roadmap.md` for the phased plan and its history: transcript
surgery and the undo vault, the retired runtime's `session.compact` effect path,
xcb's native `gobstopper-core` projection and its compatible editor backend,
the AI Charts measurement loop, and the proposed `transcript-foundation`
shared crate.

## External strategies and providers

The preferred extension surface is a versioned plugin manifest with an exact
trusted manifest SHA-256, content-addressed bundle files, closed capabilities,
cleared environment, and bounded stdin/stdout/deadline. Strategy plugins
receive normalized items and propose `Edit[]`; provider plugins perform
inspection of bounded source bytes without returning edit proposals. The host
validates every edit.
The protocol does not prevent a trusted subprocess from performing other OS
effects. Legacy `preset.command` remains available only with
`trusted_legacy_command = true` and has no sandbox guarantee.

## Failure and safety posture

- Codex/Claude file `apply` and `undo` publish copies. Arbitrary-path rewrite
  APIs and legacy in-place watch modes refuse mutation: an idle check and reread-before-rename do not establish
  compatible lifetime custody. Existing configuration flags remain accepted,
  but cannot enable those effects. Byte transforms remain available for owned
  in-memory data; fork publication requires an explicit vault root.
- Released CLI native dispatch refuses with `native_unqualified`, including
  existing `auto_compact_closed` opt-ins and standalone native-fork requests.
  Lower-level protocol adapters remain explicit caller-owned primitives, not
  qualified unattended entry points. A non-dry watch pass can archive source
  bytes before reaching that guard. Snapshot failure aborts any admitted
  dispatch; failure or uncertainty never falls through to file mutation.
- Claude liveness also checks provider markers rather than only mtime: records in
  `~/.claude/sessions/<pid>.json` map live pids to session ids, so a
  session open-but-quiet in a TUI is detected as provider-owned and excluded
  from surgery. This observation is not a lifetime ownership lock.
  The guarded Claude adapter uses `claude --resume <id> -p /compact`;
  marker absence alone cannot qualify concurrent startup or provider ownership.
  The prompt-policy hook adds a last-rung `block_tokens` ceiling (default 0
  = off): at/above it, treatment sessions get `decision: "block"` until
  `/compact` runs. Slash-command prompts are never advised or blocked,
  because they are provider UI controls, including the headless `/compact` run.
- Before publication, exact source bytes are stored as verified, deduplicated
  1 MiB chunks in the content-addressed vault.
- Rust multi-object readers and the independent retention reader hold shared
  custody of the vault directory inode. Prune holds exclusive custody through
  strict root decoding, index publication and deletion. Corrupt indexes,
  ambiguous roots and failed manifest unlink stop collection. Custody support
  is Unix-only and fails closed elsewhere.
- Version 2 intents bind exact source and candidate bytes, canonical identity
  and effective inputs before output publication. Retries use retained bytes,
  not a newly computed transform. Pins have no automatic retirement policy.
  Existing targets are never overwritten; reconciliation requires exact bytes.
- New outputs use private files, no-clobber publication and file/directory sync.
  A publication error distinguishes visibility from confirmed durability;
  an already visible output is not reported as an effect-free failure. Stable
  owner-controlled parents and filesystem sync behavior remain assumptions.
- Files are capped at 512 MiB (override via `GOBSTOPPER_MAX_TRANSCRIPT_BYTES`,
  a byte count) and 100,000 records; plugin inputs, outputs,
  manifests, bundles, deadlines, and discovery counts are independently
  bounded.
- Watch mode rate-limits per session (`min_interval_secs`), and plans below
  `min_savings_tokens` are treated as no-ops.
- Native journals retain canonical home/store/session identity and durable
  pre-dispatch intent. Dispatched or unknown work never expires into automatic
  retry. Reconciliation requires already recorded matching Codex terminal IDs.
- Hook settings installation/removal exports private, no-clobber candidate
  bundles only. Callbacks archive an exact local source but lack operation
  correlation, so they record unattributed observations rather than applied work
  or paired retention. MCP inspection never invokes configured executable
  strategies or model scoring; explicit plugin commands remain trusted code.

The [vault TLA+ models](../verify/vault/README.md),
[native dispatch model](../verify/watch/README.md),
[production Rust proof kernels](../verify/core/README.md), and
[Lean transcript algebra](../verify/transcript/README.md) state their own bounds,
assumptions and correspondence obligations. The
[bounded synthetic stress suite](../verify/stress/README.md), process-death
fixtures and negative controls add executable evidence. None proves arbitrary filesystem power loss,
proprietary provider behavior or semantic fidelity of all summaries.

## Inference and measurement boundaries

Scorers are opt-in and preserve mechanical scores for unavailable answers.
Remote scorers receive bounded transcript-derived labels and summaries, with
additional excerpts separately opt-in; those fields are not redacted metadata.
Model output must satisfy finite probability, identity and size checks. Apple
inference resolves only an installed bridge and uses one owned bounded process
per uncached request. Cache keys bind the exact submitted bounded projection,
task and available model/bridge identity. They cannot detect changes to omitted
source context or opaque proprietary weight revisions. Model output cannot bypass
host edit admission or digest budgets.

Usage observations distinguish reported, absent, unknown and reset context and
full versus partial lifetime scope. Snapshot-bound source identity is required
to count a recorded reduction; estimates and unknown values are not zero-cost
observations. Literal retention, lexical token coverage and optional model
judgment have separate meanings and explicit coverage. Neither model judgment
nor a Wilson interval over sampled checks establishes task success.
Evaluation freezes one canonical source image per session. Benchmark output
retains discovered
evaluation failures and distinguishes provider proposals from detached
transforms. The retention study includes an unchanged baseline; its other
arms do not stand in for unexecuted provider-native experiments.

## Historical measured findings (realized audit, offline)

Evidence from `scripts/retention-audit.py` over vault snapshot pairs plus
`scripts/provider-retention-probe.py` synthetic runs. Retention = text
presence in live records, not semantic equivalence. These earlier cohorts are
not live qualification of the current artifact or a general retention bound.

- Two provider compaction shapes: Codex rewrites history verbatim
  (`replacement_history`); Claude writes a `compact_boundary` plus paraphrased
  summary.
- Literal vs lexical: Claude summary pairs score ~0-10% literal but ~25-50%
  lexical (>=75% of a check's content tokens in one slot). Codex tracks
  lit~lex (~60-95%).
- Constraint-class content is the worst-retained kind in Claude summaries
  (per-kind lexical counts), behind procedures and facts.
- In three constraint-heavy synthetic seed framings, follow-up answers refused
  recall and described rules as "adversarially-supplied", despite rule text
  appearing in summaries. Pinned-channel probes echoed rule text and answered
  `migration_allowed: false`. These are recall observations. No migration was
  attempted, and the runs did not independently control prompt framing,
  configuration, repeated instruction injection, or provider state. They do
  not establish behavioral enforcement or a causal effect of instruction role.
- Preserving original provider records and roles is a hypothesis to test.
  `source_bound` scores text and origin preservation; a lower score after a
  digest does not itself establish worse task behavior. Earlier typed-masking
  pilots used preservation labels to score the same outputs, and repeated
  frozen sources do not count as independent replications. The
  [held-out study protocol](retention-study-protocol.md) separates development
  labels, independent scoring, task continuations, and provider qualification.
- Pairing note: hook labels do not bracket the boundary write; pairing keys
  on byte-level mutation markers (`compact_boundary`, `"type":"compacted"`,
  `"summarized_from":[0-9]`).

The native recall probe's scoring version 2 records an outcome for every
registered criterion. Success requires both the four strict fact checks and
all lexical rule markers when that seed declares them. Invalid JSON shapes
and field types produce saved failure results. The constraints arm's markers
cover its four declared rules; pending rollback is scored separately as a fact.
Literal answers, lexical markers, execution completion, and provider-reported
cost completeness remain
separate; task success and behavioral enforcement are unmeasured. Historical
results retain their original scoring and must not be silently regraded.

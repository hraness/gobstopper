<!-- hraness:gobstopper-landing:start -->
# gobstopper

Automatic context compaction for coding-agent sessions — Codex and Claude
Code today, any JSONL-transcript agent tomorrow.

gobstopper is the first cross-provider context compactor that preserves a
content-addressed archive of every conversation state, compacts resumable
Claude Code and Codex transcripts, and proves on real API calls that it
reduces context without inventing answers.

Run it, and it watches your agent sessions. When a session's context
crosses a configured threshold, gobstopper compacts it — using a strategy you choose per session, per provider, or per preset — and stores the
exact pre- and post-state in the vault so you can audit what changed.

<!-- hraness:gobstopper-landing:end -->

## Why

Providers compact late. Codex fires auto-compaction at ~90% of the model's
context window; Claude Code similar. On a 1M-token window that means every
turn near the end of a long session costs ~900k input tokens — and on
subscription plans, those tokens come out of your weekly allowance.

Context is a sawtooth problem. If you compact at trigger `T` and the
summary floor is `F`, steady-state *context occupancy* per turn is roughly
`(T+F)/2`:

| policy | trigger | floor | avg context/turn | relative occupancy |
|---|---|---|---|---|
| provider default (1M window) | ~900k | ~60k | ~480k | 1.0x |
| gobstopper default | 250k | 40k | ~145k | ~3.3x lower occupancy |
| gobstopper aggressive | 150k | 20k | ~85k | ~5.6x lower occupancy |

This is an **occupancy model**, not a measured subscription savings claim.
Actual token cost depends on cache hit rates, whether summary turns are
billed, re-fetches caused by lost detail, and how often compaction itself
runs. Anthropic documents that clearing tool results can
[invalidate the prompt cache](https://platform.claude.com/docs/en/build-with-claude/context-editing).
gobstopper reports measured file-byte changes and observed provider usage
where available; it does not project dollar or quota savings into its
public claims.

## Infinite memory

Every compaction writes the pre- and post-state into a content-addressed
vault (`~/.local/share/gobstopper/vault/`). The same record appears once
across every version it participates in, so keeping every state does not
explode storage.

`gobstopper recall --query <q>` turns that vault into agent-addressable
memory: it searches every archived state-card digest, ranks results by
query relevance, and returns the high-level state of the matching turns.
The agent does not need to remember session IDs — it can ask for the last
time it worked on a file, a goal, or a decision and get a ranked summary
with a snapshot SHA it can `show` or `diff`.

Compaction itself isn't free — each cycle costs one large input call and
risks losing detail — so strategy matters. That is the actual product
here: not "compact earlier" but "compact with the right strategy at the
right boundary."

## Strategies

| id | kind | what it does |
|---|---|---|
| `auto` (default) | dynamic | selects per-session from transcript composition: tool-heavy → `cache_aware`, chatty → `structured`, live/empty → `sawtooth` |
| `sawtooth` | provider | fires the provider's own compaction early (`thread/compact/start` on Codex app-server; `/compact` or `--autocompact` on Claude) |
| `elide` | transcript | stubs stale tool outputs oldest-first until the floor; deterministic, no model call |
|| `cache_aware` | transcript | elides the *latest* stale tool outputs before the protected tail, then injects a state-card digest. Keeps the conversation prefix byte-identical so the provider's prompt-cache hit rate is preserved |
|| `compacted` | transcript | same digest as `cache_aware` but elides stale outputs oldest-first. On Codex the digest is lowered to a provider-native `compacted` record; on Claude it appends as a synthetic `user` turn |
| `structured` | transcript | placeholder state-card digest (`goal`/`decisions`/`files`/`todos`); currently emits item labels, not a real summary. Safe for chat-only sessions but should not be mistaken for a semantic compressor |
| `agentic` | transcript | reserved for a bounded editor-model backend; today `preset.command` is the only extension point and is treated as untrusted code |

Custom strategies are userspace code: a preset can name a `command` that
receives the normalized transcript as JSON on stdin and returns an edit
plan on stdout, or install a versioned `gobstopper-plugin.json` bundle
(see `gobstopper plugin check`). Host-side validation bounds every
proposal: no edit can grow the transcript, leave protected recent output,
bypass linkage checks, or exceed configured digest size.

## Install & use

```sh
cargo install --git https://github.com/hraness/gobstopper gobstopper
# or from a checkout: cargo build --release

gobstopper detect                  # sessions, context sizes, lifetime burn
gobstopper plan <session>          # what would happen, under which strategy
gobstopper eval <session>          # every strategy side-by-side on temp copies
gobstopper apply <session>         # vault snapshot + produce validated fork (idle sessions)
gobstopper verify <session>        # resume-validity check (exit 1 on errors)
gobstopper fork <session>          # clone under a fresh session id + resume cmd
gobstopper undo <session>          # restore a pre-compaction snapshot into a new fork
gobstopper vault                   # list snapshots in the undo vault
gobstopper install-hooks           # Claude + Codex compaction lifecycle hooks
gobstopper watch --dry-run         # the daemon path: poll, threshold, prepare copy
gobstopper explain                 # the occupancy math above
gobstopper recall --query <q>      # search state-card digests across all archived sessions
gobstopper history <session>       # every archived state of one session
gobstopper diff <sha-a> <sha-b>    # structural comparison of two vault snapshots
gobstopper bench                   # benchmark every strategy across discovered sessions
```

Every `apply`/`watch` compaction snapshots the source transcript into a
content-addressed vault (`~/.local/share/gobstopper/vault/`) and publishes
the result as a separate, verified file. The original transcript is never
overwritten by a standalone compaction run; live session surgery must be
dispatched by the session owner. Each compaction appends a numeric record
to `events.jsonl` in the `gobstopper/compaction-events-v1` schema.

Config: `~/.config/gobstopper/config.toml`

```toml
[policy]
strategy = "auto"            # sawtooth | elide | structured | agentic
trigger_tokens = 250_000
floor_tokens = 40_000

[provider.codex]             # per-provider overrides
trigger_tokens = 200_000

[sessions."01a08d7c-…"]      # per-session overrides
strategy = "structured"
trigger_tokens = 120_000

[presets.deep-work]          # named presets, selectable via --preset
strategy = "elide"
trigger_tokens = 150_000

[presets.custom-script]      # userspace code preset
command = "python3 ~/bin/my_compactor.py"
```

Managed sessions (oompa profiles, sandboxed homes) use different roots:
point gobstopper at them with `--codex-home` / `--claude-home`.

## The oompa seam

oompa never parses provider transcript files — that boundary stays intact.
Integration is the `policy-check` subcommand: oompa already records
`token_usage` events (`totalTokens`, `modelContextWindow`) in its neutral
timeline, so it asks gobstopper what to do with them:

```sh
gobstopper policy-check --provider codex --context-tokens 300000 \
    --session-active --json
# {"action":"provider_compact","control":"thread/compact/start", ...}
```

oompa then invokes `thread/compact/start` on its own app-server
connection, or launches Claude sessions with `--autocompact <tokens>`.
Deeper transcript surgery (elide/structured) stays gobstopper-side,
applied to idle sessions or at resume boundaries.

## How providers compact today

| lever | codex | claude code |
|---|---|---|
| auto-compact threshold | `model_auto_compact_token_limit` (config; ≤90% of window) | `--autocompact <100k–1M>` argv |
| on-demand trigger | `thread/compact/start` (app-server v2) | `/compact [instructions]` |
| compaction prompt | `compact_prompt` config | `/compact` instructions |
| tool output cap | `tool_output_token_limit` | — |
| live usage stream | `thread/tokenUsage/updated` notification | `message.usage` per turn |
| transcript store | `~/.codex/sessions/**/rollout-*.jsonl` | `~/.claude/projects/*/*.jsonl` |

Codex persists compaction as a `compacted` rollout record carrying
`replacement_history` — the provider's own resume-time context swap.
gobstopper's transcript path respects that shape (elision inside
`replacement_history` is supported); writing custom `compacted` records
for fully custom summaries is the designed v0.2 path.

## Layout

- `crates/gobstopper-core` — normalized transcript model, the `Edit` IR,
  `Strategy` trait, and all built-in strategies. No I/O.
- `crates/gobstopper-adapters` — session discovery and the Codex/Claude
  JSONL dialects (parse + in-place rewrite; lines are never removed, so
  provider linkage is preserved).
- `crates/gobstopper-cli` — `gobstopper` binary: detect / plan / apply /
  verify / undo / vault / watch / policy-check / presets / explain.

## Live qualification

`gobstopper` has been live-qualified on real provider sessions. The most
recent trial used a 333k-token Claude session, asked the same resume
question under four conditions, and measured the tokens the provider
actually consumed on the next turn:

| condition | input tokens on resume | output tokens | recalled the standing task? |
|---|---|---|---|
| none (original) | 312,722 | 1,405 | yes — reported npm unification and stalled renames |
| `gobstopper elide` | 219,167 | 1,052 | yes — same standing task, stalled renames |
| `gobstopper compacted` | 220,447 | 621 | yes — same standing task from the state-card digest |
| `claude --autocompact 100` | 56,300 | 416 | no — incorrectly claimed the renames were already done and published |

`gobstopper elide` and `compacted` both cut the resume context by about
30% while keeping the answer accurate. Claude's native `--autocompact 100`
cut the resume context by ~82% but produced a confident, inaccurate
summary of the session.

The same question was then asked on a 101k-token Codex session:

| condition | input tokens on resume | output tokens | recalled the standing task? |
|---|---|---|---|
| none (original) | 101,275 | 244 | yes — Oh's memory benchmark and the 0.60 expansion gate |
| `gobstopper elide` | 57,980 | 83 | yes — same 0.545 score and 0.60 gate |
| `gobstopper compacted` | 34,503 | 159 | yes — same BEAM experiment and expansion gate |

On Codex, `compacted` cut resume input tokens by **66%** and `elide` cut
them by **43%**, both with accurate answers. There is no one-shot Codex
native compact to compare against.

The same-session `cache_aware` A/B (339k-token Claude session, floor 310k,
real provider cache counters):

| condition | `cache_read` | `cache_creation` | file-level prefix preserved | cost | accurate? |
|---|---:|---:|---:|---:|---|
| none (original) | 10,010 | 325,647 | — | $6.53 | yes |
| `gobstopper cache_aware` | 13,536 | 258,517 | 107,884 tokens | $5.19 | yes |
| `gobstopper compacted` | 13,536 | 257,505 | 6,639 tokens | $5.18 | yes |

Honest result: `cache_aware` and `compacted` cost the same on the API —
Claude Code's prompt-cache breakpoints sit at ~13.5k regardless of how much
file-level prefix stays byte-identical. What `cache_aware` actually buys is
**16x more preserved prefix at the transcript level**, which is what keeps
`gobstopper diff` audits small and Merkle-dedup efficient across repeated
compactions. Choose it when auditability matters; choose `compacted` when
you want the Codex-native record.

That is the difference gobstopper is built for: measured, auditable
compaction that does not replace the transcript's actual state with a
plausible invention. Every pre- and post-state is in the vault, so you can
`gobstopper diff` the exact structural changes and decide which strategy to
trust.

- **Codex custom `compacted` record** — a gobstopper-written `compacted`
  record with a correct window chain was accepted by `codex exec resume` and
  the model completed a real API turn recalling the elided commands.
- **Claude Code `compacted` / digest resume** — `gobstopper apply
  --in-place --strategy compacted` on a 333k-token Claude session elided 43
  stale tool records and injected a state-card digest. `gobstopper diff`
  against the original vault snapshot showed exactly 43 removed and 46 added
  records (43 stubs + 3 tail records); `claude --resume <session>` succeeded
  and the model recalled the last user prompt and current task state from the
  digest.

## Status

Production-ready for idle and resume-boundary transcript compaction on Codex
and Claude Code. Core detection, planning, and transcript surgery are covered
by a property-tested suite plus the live qualifications above. `apply` and
`watch` always snapshot the source into the content-addressed vault first;
`verify` checks resume-validity, `undo` restores to a new fork (or
`--in-place` back onto the same session id), and `vault` keeps every state.
`sawtooth` can route to Codex's `thread/compact/start` over a private
app-server connection when `codex-cli` is installed and trusted.

The `structured` and `agentic` strategies remain conservative placeholders or
extension points, not proven semantic compressors. Custom strategy code is
treated as untrusted and validated by the host before any transcript is
written. See [docs/design.md](docs/design.md) for the research record and [docs/roadmap.md](docs/roadmap.md) for the full plan.

## License

MIT OR Apache-2.0

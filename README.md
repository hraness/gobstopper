<!-- hraness:gobstopper-landing:start -->
# gobstopper

Automatic context compaction for coding-agent sessions — Codex and Claude
Code today, any JSONL-transcript agent tomorrow.

Run it, and it watches your agent sessions. When a session's context
crosses a configured threshold, gobstopper compacts it — earlier and
smarter than the provider's own defaults — using a strategy you choose per
session, per provider, or per preset.
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

Compaction itself isn't free — each cycle costs one large input call and
risks losing detail — so strategy matters. That is the actual product
here: not "compact earlier" but "compact with the right strategy at the
right boundary."

## Strategies

| id | kind | what it does |
|---|---|---|
| `auto` (default) | dynamic | selects per-session from transcript composition: tool-heavy → `elide`, chatty → `structured`, live/empty → `sawtooth` |
| `sawtooth` | provider | fires the provider's own compaction early (`thread/compact/start` on Codex app-server; `/compact` or `--autocompact` on Claude) |
| `elide` | transcript | stubs stale tool outputs oldest-first until the floor; deterministic, no model call |
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

## Status

Experimental. Core detection, planning, and transcript elision run
against real session files and a property-tested suite. `apply` and `watch`
publish a separate, verified transcript copy; they no longer overwrite the
source transcript directly. `verify` checks resume-validity, `undo` restores
to a new fork, and `vault` keeps content-addressed snapshots. `sawtooth` can
route to Codex's `thread/compact/start` over a private app-server connection
when `codex-cli` is installed and trusted.

The `structured` and `agentic` strategies are placeholders or extension
points, not proven semantic compressors. Custom strategy code is treated as
untrusted and validated by the host before any transcript is written.

Research papers cited in [docs/design.md](docs/design.md) motivate earlier
compaction and observation masking in general; they do not validate
gobstopper's specific savings or superiority. Comparative benchmarks against
provider-native defaults are on the roadmap; current savings claims are
occupancy models only.

See [docs/roadmap.md](docs/roadmap.md) for the phased plan and remaining
work, including live-provider qualification and measured cost comparisons.

## License

MIT OR Apache-2.0

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
summary floor is `F`, steady-state cost per turn is roughly `(T+F)/2`:

| policy | trigger | floor | avg input/turn | relative cost |
|---|---|---|---|---|
| provider default (1M window) | ~900k | ~60k | ~480k | 1.0x |
| gobstopper default | 250k | 40k | ~145k | **~3.3x less** |
| gobstopper aggressive | 150k | 20k | ~85k | **~5.6x less** |

The same arithmetic holds on 400k-class windows (~3x savings at a 150k
trigger). The claimed "~10x" is reachable only in the best case — long
tool-heavy sessions where elision drives the floor near zero — but 3-6x
fewer input tokens per turn is the honest, repeatable range, and it
compounds with a second benefit: **context rot**. Model recall degrades as
context grows, so the tail of a 900k session is not just expensive, it is
measurably dumber. Compacting earlier buys back both quota and quality.

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
| `structured` | transcript | extracts a state card (goal/decisions/files/todos) + keeps recent turns verbatim |
| `agentic` | transcript | a small editor model emits edits through a fixed tool schema (`keep`/`elide`/`summarize`/`defer`); driver-pluggable |

Custom strategies are userspace code: a preset can name a `command` that
receives the normalized transcript as JSON on stdin and returns an edit
plan on stdout.

## Install & use

```sh
cargo install --git https://github.com/hraness/gobstopper gobstopper
# or from a checkout: cargo build --release

gobstopper detect                  # sessions, context sizes, lifetime burn
gobstopper plan <session>          # what would happen, under which strategy
gobstopper eval <session>          # every strategy side-by-side on temp copies
gobstopper apply <session>         # vault snapshot + rewrite (idle sessions)
gobstopper verify <session>        # resume-validity check (exit 1 on errors)
gobstopper fork <session>          # clone under a fresh session id + resume cmd
gobstopper undo <session>          # restore the pre-compaction snapshot
gobstopper vault                   # list snapshots in the undo vault
gobstopper install-hooks           # Claude + Codex compaction lifecycle hooks
gobstopper watch --dry-run         # the daemon path: poll, threshold, fire
gobstopper explain                 # the economics math above
```

Every `apply`/`watch` compaction snapshots the transcript into a
content-addressed vault (`~/.local/share/gobstopper/vault/`) before
writing and appends a numeric record to `events.jsonl` — the
`gobstopper/compaction-events-v1` schema aicharts and oompa consume.

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

v0.2: detection, planning, and transcript elision work against real
session files. `sawtooth` routes to Codex's `thread/compact/start` through
a private `codex app-server --listen stdio://` process — no daemon
required. `verify` checks resume-validity, `undo`/`vault` give reversible
compaction via a content-addressed snapshot store, `policy-check` accepts
`--quota-pressure`, and every compaction emits a numeric
`compaction-events-v1` record. The `agentic` strategy runs an external
editor command (`preset.command`) that returns bounded `Edit` plans —
`hraness/agentmixer`'s `gobstopper-editor` shim is the reference backend —
and falls back to the `auto` rubric when no command is configured.

See [docs/design.md](docs/design.md) for the research basis and
[docs/roadmap.md](docs/roadmap.md) for the phased plan — including how
gobstopper shares foundations with oompa (control plane), aicharts
(measurement), and the agentmixer task runtime (editor-model backend for
the `agentic` strategy).

## License

MIT OR Apache-2.0

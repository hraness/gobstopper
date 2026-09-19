<!-- hraness:gobstopper-landing:start -->
# gobstopper

Automatic context compaction for coding-agent sessions — built-in Codex
and Claude Code adapters, native policy integration for Devin, and a bounded
provider/strategy plugin protocol.

gobstopper is a cross-provider context compactor with exact, content-addressed
recovery snapshots, resumable Claude Code and Codex transcript forks, and a
measurement harness for projected savings, preserved prefix, structural
validity, and probe recall.

Run it, and it watches supported agent sessions. When a session's context
crosses a configured threshold, gobstopper prepares a separate compacted fork
using a strategy selected per session, provider, or preset. The source is
snapshotted and never overwritten by standalone `apply` or `watch`.

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

Every standalone compaction writes the exact source bytes into a
content-addressed vault (`~/.local/share/gobstopper/vault/`) before publishing
a fork. Snapshots use deduplicated 1 MiB chunks, so appended versions reuse
unchanged prefix storage without creating one filesystem object per JSONL
record.

`gobstopper recall --query <q>` turns that vault into agent-addressable
memory: it searches every archived state-card digest, ranks results by
query relevance, and returns the high-level state of the matching turns.
The agent does not need to remember session IDs — it can ask for the last
time it worked on a file, a goal, or a decision and get a ranked summary
with a snapshot SHA it can `show` or `diff`.

`gobstopper mcp` exposes a read-only Model Context Protocol server on stdio —
tools `policy_check`, `list_sessions`, `recall`, `history`, `show`, `diff`,
`plan`, and `verify`. Register it once and an agent can inspect policy and
archived state without gaining a transcript mutation tool:

```sh
claude mcp add gobstopper -- gobstopper mcp
# ~/.codex/config.toml: [mcp_servers.gobstopper] command = "gobstopper", args = ["mcp"]
devin mcp add -s user gobstopper -- gobstopper mcp
```

For Devin, `policy_check` accepts `provider = "devin"` and returns `/compact`
when the configured threshold is crossed. Devin remains the sole owner of its
session store; gobstopper does not edit Devin history. `devin --export out.json`
can be inspected through a read-only provider plugin when offline analysis is
needed.

Compaction itself isn't free — each cycle costs one large input call and
risks losing detail — so strategy matters. That is the actual product
here: not "compact earlier" but "compact with the right strategy at the
right boundary."

## Strategies

| id | kind | what it does |
|---|---|---|
| `auto` (default) | dynamic | live sessions delegate to provider controls (`cache_edits` for eligible Claude sessions); idle sessions choose the best validated file strategy by savings and preserved-prefix score |
| `sawtooth` | provider | requests provider-native compaction (`thread/compact/start` on Codex; `/compact` guidance on Claude) |
| `cache_edits` | provider | emits bounded Claude `tool_use_id` values for API-layer context editing; never rewrites a transcript |
| `elide` | transcript | stubs stale tool outputs oldest-first until the floor |
| `cache_aware` | transcript | elides a tailward stale-output window and injects a bounded state card while preserving the longest practical prefix |
| `compacted` | transcript | elides stale outputs and injects the state card; synthetic Codex `compacted` records require `--experimental-compacted` |
| `scored` | transcript | ranks candidates with deterministic recency, error, reference, TF-IDF, duplicate, and tool-type signals before elision |
| `dedupe` | transcript | removes older exact duplicate tool payloads using payload SHA-256, not summaries |
| `micro` | transcript | keeps the newest configured outputs per stable tool label and stubs older ones |
| `middle` | transcript | protects both ends of the transcript and elides eligible middle outputs |
| `structured` | transcript | emits a bounded metadata-derived state card; it is not semantic summarization |
| `agentic` | extension | accepts bounded edit proposals from an explicitly trusted command or versioned plugin; host validation remains authoritative |

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
gobstopper plan <session> --trigger 100000 --floor 30000    # tune the trade-off
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
gobstopper tune <session>          # preview the adaptive trigger/floor for a session
gobstopper mcp                     # read-only MCP server: the vault as agent tools
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
strategy = "auto"
trigger_tokens = 250_000
floor_tokens = 40_000
min_savings_tokens = 4_096   # reject ineffective plans
adaptive = true              # derive trigger/floor per session — see `gobstopper tune`

[provider.codex]             # per-provider overrides
trigger_tokens = 200_000

[provider.devin]             # numeric policy only; action is native /compact
trigger_tokens = 200_000

[sessions."01a08d7c-…"]      # per-session overrides
strategy = "structured"
trigger_tokens = 120_000

[presets.deep-work]          # named presets, selectable via --preset
strategy = "elide"
trigger_tokens = 150_000

[presets.custom-script]      # legacy userspace code preset
command = "python3 ~/bin/my_compactor.py"
trusted_legacy_command = true
```

Managed sessions (oompa profiles, sandboxed homes) use different roots:
point gobstopper at them with `--codex-home` / `--claude-home`.

`scored` uses the deterministic offline heuristic by default. Experimental
external scoring is opt-in with `GOBSTOPPER_SCORER=llm`,
`GOBSTOPPER_SCORER=jev`, or `GOBSTOPPER_SCORER=apple`; merely setting an API
key never sends data. External scorers receive bounded labels and summaries,
not full tool payloads, and fall back to the heuristic on failure. The
built-in heuristic is the recommended published path because current live
trials did not show a better plan from the LLM scorer.

`GOBSTOPPER_SCORER=jev` scores with TypeSafe's System One API — typed
`noul` keep-probabilities, ~100ms per batch of 64 questions, no prose
generation. Onboarding vaults the key in the OS credential store
(macOS Keychain, Windows Credential Manager, Linux kernel keyring):

```sh
pbpaste | gobstopper auth jev     # or run it bare to use the clipboard
gobstopper auth jev --status      # key source + masked value + live check
gobstopper auth jev --delete      # remove the stored key
```

The key is verified against the API before it is stored; a rejected key
never reaches the keychain. Resolution order at scoring time is
`TYPESAFE_API_KEY` → `GOBSTOPPER_JEV_API_KEY` → OS keychain, so CI keeps
working from env alone. Linux kernel-keyring entries are session-scoped and
do not survive a reboot; use an environment variable for persistent
noninteractive Linux automation. On macOS, a self-built unsigned binary may
show a one-time keychain access prompt on first read.
`GOBSTOPPER_JEV_CONTENT_BYTES` (default `0`) opts in to attaching bounded
per-candidate content excerpts to each question — Jev is a remote API, so
content only leaves the device when explicitly enabled.

`eval` and `bench` now honor `GOBSTOPPER_SCORER` for their `scored` row, so
an A/B run measures the same Jev or Apple ranking used by `plan` rather than
silently substituting the heuristic. `GOBSTOPPER_EVAL_JUDGE=jev` adds a
separate semantic recall score to `eval`: up to 64 extracted facts are asked
as typed `noul` questions against at most 100,000 bytes of the rewritten
transcript (bounded head + tail), crediting facts preserved as paraphrase in
a state card or per-item stub. The judge is off by default because it sends
that bounded rewritten context to the remote API; failures simply omit the
semantic score, while deterministic verbatim probe recall still runs.
Restricting the run to `--strategy scored` uses one judge request:

```sh
GOBSTOPPER_SCORER=jev GOBSTOPPER_EVAL_JUDGE=jev \
  gobstopper eval <session> --strategy scored
```

Gobstopper reads the official `answers.<id>.noul` probability returned by
System One, while retaining bounded compatibility fallbacks for older response
shapes; missing or malformed answers remain neutral at `0.5`. The eval harness
now makes a post-parser Jev-versus-heuristic quality trial possible, but no
ranking-quality win is claimed until that live comparison is rerun.

Successful Jev responses are cached in-process for five minutes, keyed by
endpoint, credential identity, and the exact serialized request. This keeps
`watch` from paying for identical scorer calls on an unchanged transcript
while periodically refreshing against the remote model. The cache stores
only parsed probabilities (not transcript text), clears at 64 entries, and
never caches failures. Set `GOBSTOPPER_JEV_CACHE=0` or
`GOBSTOPPER_JEV_CACHE_TTL_SECS=0` to disable reads; change the TTL with the
latter variable.

`GOBSTOPPER_SCORER=apple` (macOS 26+, Apple Silicon) scores on-device with
Apple Intelligence Foundation Models via the shared `apple-foundation`
bridge — free, private, no API key. The bridge auto-builds to
`~/.local/share/gobstopper/apple-bridge` on first use (or set
`GOBSTOPPER_APPLE_BRIDGE`), requests queue through one persistent process
with guided JSON output, and any failure degrades to neutral scores.
`GOBSTOPPER_APPLE_TIMEOUT_MS`, `_MAX_CANDIDATES`, `_BATCH_SIZE`, and
`_MAX_BATCHES` tune it. Since inference is local, the scorer also reads a
bounded excerpt of each candidate record (`GOBSTOPPER_APPLE_CONTENT_BYTES`,
default 400; `0` restores labels-only scoring) and shrinks its default
batch sizes to fit the ~4k-token context window.

`GOBSTOPPER_DIGEST=apple` goes further: the injected state card is written
by the on-device model instead of keyword extraction. Because inference is
local, it may read bounded excerpts of the records being elided — the
labels-only boundary only exists for remote endpoints. Each field still
lands in the same `DigestBlock` shape via guided output, capped to a small
token overhead, and falls back to the mechanical card on any failure.
`GOBSTOPPER_APPLE_DIGEST_ITEMS`, `_ITEM_BYTES`, and `_TOTAL_BYTES` tune the
excerpt budget, which defaults are sized to the model's ~4k-token window.

The same call also writes a one-line stub per excerpted record — e.g.
`Script completed Wall time 4.4 seconds` — stored in the elide edit's
`per_item_stubs` map and rendered verbatim in place of the `{bytes}`/`{kind}`
template where the payload was removed. Records the model did not cover
keep the generic stub; invalid or oversized stubs are dropped by validation.

Apple requests are cached in-process on the (prompt, schema) pair: `watch`
re-evaluates an unchanged transcript every poll interval, and identical
model inputs return the recorded response instead of another generation —
a live dry-run poll went from ~17s to milliseconds per re-eval. Bounded at
64 entries; `GOBSTOPPER_APPLE_CACHE=0` disables reads. The savings gate
also prices the residual stub text left behind by elision so
`context_tokens_after` doesn't overstate reclaim.

With `adaptive = true`, the effective trigger/floor are re-derived per
session at each decision point: the trigger is capped at a quarter of
the provider-advertised context window, backed off (bounded 2x) when
recent compactions reclaimed too little to be worth a cycle, and
tightened when most of the window is reclaimable tool output. The
adjustment is deterministic and its reasons appear in plan output and
telemetry. `gobstopper tune <session>` previews it.

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
Deeper transcript surgery stays gobstopper-side and publishes verified forks
for explicit resume. It never creates a second writer for an oompa-managed
session.

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
gobstopper's parser and verifier understand that shape, including tool pairs
inside `replacement_history`. Synthetic records are experimental, pair-aware,
transactional, and available only behind `--experimental-compacted`; ordinary
`apply` uses the portable forked digest representation.

## Layout

- `crates/gobstopper-core` — normalized transcript model, the `Edit` IR,
  `Strategy` trait, and all built-in strategies. No I/O.
- `crates/gobstopper-adapters` — bounded session discovery, Codex/Claude
  JSONL parsing and candidate rewriting, transactional no-clobber publication,
  verification, plugin hosting, and the snapshot vault.
- `crates/gobstopper-cli` — `gobstopper` binary: detect / plan / apply /
  verify / undo / vault / history / show / recall / diff / bench /
  mcp / watch / policy-check / presets / explain.

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
- **Claude Code digest resume** — an earlier qualification trial on a separate
  333k-token test copy elided 43 stale tool records and injected a state card;
  `claude --resume` succeeded and recalled the standing task. Current releases
  preserve that evidence while exposing standalone application only through
  no-clobber fork publication.

## Status

The published safety posture is conservative: `plan`, `eval`, `verify`, MCP,
and provider inspection are read-only; standalone `apply`, `watch`, and `undo`
publish separate files and never overwrite a provider-owned source. Candidate
publication is source-hash-bound, no-clobber, snapshotted, structurally
verified, idempotent through durable receipts, and bounded to 128 MiB/100,000
records. The default deterministic strategies and built-in Codex/Claude
adapters are covered by unit, regression, property, and live-resume evidence.

Direct provider controls still belong to the live session owner. Synthetic
Codex `compacted` records, external model scoring, and semantic editor plugins
remain explicitly experimental or trusted extension paths. Devin support is a
native numeric policy/MCP handoff to `/compact`, not direct transcript surgery.
See [docs/design.md](docs/design.md), [docs/roadmap.md](docs/roadmap.md),
[docs/plugin-protocol.md](docs/plugin-protocol.md), and
[docs/devin.md](docs/devin.md) for the boundaries.

## License

MIT OR Apache-2.0

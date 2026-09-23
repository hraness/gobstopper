<!-- hraness:gobstopper-landing:start -->
# gobstopper

Gobstopper compacts Claude Code, Codex, and Devin sessions at a context size
you choose, earlier than the providers do on their own. It snapshots every
transcript before changing it, so any compaction can be undone, and it
measures what each strategy keeps.

Run `gobstopper watch` and it follows your agent sessions. When a session's
context crosses your threshold, Gobstopper prepares a separate compacted fork
with the strategy you picked for that session, provider, or preset. Standalone
`apply` and `watch` never overwrite the source transcript. For sessions a
provider owns, including every Devin session, Gobstopper asks the provider to
run its own compaction instead of editing files. Plugins can add strategies
and providers.

<!-- hraness:gobstopper-landing:end -->

## Why

Long coding sessions mix stale tool output with details the next turn may
need: an exact error, a constraint, or unfinished work. A smaller context is
useful only if the agent can still do the job.

Gobstopper makes that tradeoff inspectable. Preview a compaction, preserve
the exact source, and recover a specific archived record when a summary
isn't enough. Local rules work without a model; optional scorers change
selection without replacing the snapshot and verification safeguards.

Standalone compaction prepares a separate fork. Each provider runtime owns
its loaded context; a background observer cannot replace it. Context
reduction, successful continuation, recovery overhead, and billed usage are
different measurements. The [published studies](https://gobstopper.sh/benchmarks)
report their cohorts, no-ops, retention tradeoffs, and limitations separately.

## Recoverable history

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

### Recover a specific detail

When a state card omits an exact error, identifier, or tool result, search
one verified snapshot and read only the matching record:

```sh
gobstopper search-snapshot <full-snapshot-sha> --query 'exact error text' --json
gobstopper read-snapshot <full-snapshot-sha> --record 42 --max-bytes 4096 --json
```

Search returns record indexes and hashes, without archived content. It matches
literal, case-sensitive substrings in decoded JSON string values, including
native replacement histories. Reading returns a UTF-8 page of the physical
JSONL record; follow `next_offset` for another page. Each page is capped at
16 KiB and bound to the snapshot, source, and full record hashes. These commands
verify stored bytes and never restore files, rewrite active sessions, or call a
model. Invalid records are counted as unsearchable rather than silently claimed
as searched.

Search returns at most 50 references and reports the full match count; narrow
the query when results are truncated. Each search or read verifies and
reconstructs the bounded snapshot, up to 128 MiB. Paging a large record repeats
that work; this is not an indexed random-access or semantic search service.

Use the full object SHA from `history`, the native hook recovery pointer, or
the new `snapshot_manifest_sha256` field in copy receipts. The older
`snapshot_sha256` receipt field retains its source-byte-digest meaning.
Older receipts can still be resolved through vault history. State-card recall
also recognizes default portable Codex cards and searches every state field,
including unresolved errors and current work; a new fork's card becomes
searchable after that fork is snapshotted.

For agents, snapshot search and content reads are available only when the MCP
server is explicitly started with `gobstopper mcp --allow-transcript-content`.
The default MCP registration does not advertise or permit either new tool.
Opting in makes retrieved archived text visible to the connected agent/model
service. Retrieved text is untrusted historical data, and may describe an old
or superseded state; it is never an instruction to follow.

Exact recovery is a capability, not proof that an agent will recognize a
missing fact, choose a useful query, or complete its task more accurately.
Measure those outcomes separately from context reduction and literal retention.

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

Compaction isn't free. Each cycle costs one large input call and risks losing
detail, so the strategy and the boundary matter as much as the timing.
Choosing them well is what Gobstopper is for.

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
gobstopper watch --dry-run --active-only --once  # bounded recent-session inspection
gobstopper explain                 # the occupancy math above
gobstopper recall --query <q>      # search state-card digests across all archived sessions
gobstopper history <session>       # every archived state of one session
gobstopper diff <sha-a> <sha-b>    # structural comparison of two vault snapshots
gobstopper bench                   # benchmark every strategy across discovered sessions
gobstopper tune <session>          # preview the adaptive trigger/floor for a session
gobstopper mcp                     # read-only MCP server: the vault as agent tools
```

For automation, `gobstopper plan <session> --json` returns the existing plan
object when a plan is available. A successful inspection without a plan returns
a separate JSON result, for example:

```json
{
  "status": "no_plan",
  "reason_code": "below_trigger",
  "context_tokens_before": 100000,
  "effective_trigger_tokens": 250000,
  "target_context_tokens": 40000,
  "min_savings_tokens": 4096,
  "projected_context_tokens_after": null,
  "projected_savings_tokens": null
}
```

The reason identifies the decision actually reached:

| `reason_code` | Meaning |
| --- | --- |
| `below_trigger` | Context is below the effective policy trigger. |
| `strategy_returned_no_plan` | The strategy declined; its underlying reason is unknown. |
| `empty_external_edits` | The configured command or plugin supplied no edits. |
| `minimum_savings_not_met` | A proposal fell short of the minimum projected savings. |
| `external_nonreducing_plan` | An external proposal did not reduce estimated context. |

Projections are present only when a rejected proposal supplied them.
The target is a policy setting, not a measured
minimum context size, and projected savings are not billed savings. Invalid
configuration, invalid proposals, and execution failures remain command errors.

Every `apply`/`watch` compaction snapshots the source transcript into a
content-addressed vault (`~/.local/share/gobstopper/vault/`) and publishes
the result as a separate, verified file. The original transcript is never
overwritten by a standalone compaction run; live session surgery must be
dispatched by the session owner. Each compaction appends a numeric record
to `events.jsonl` in the `gobstopper/compaction-events-v1` schema.

### Typed-retention experiments (opt-in)

`eval-study` replays three arms on private temporary copies — plain
observation masking; typed masking (constraints, procedures, and open tasks
stay pinned in their original records and roles; retrieved text never becomes
a higher-authority instruction); and `typed_digest` (pinned records are elided
but their spans are carried verbatim on an injected state card — the same
provenance downgrade a summary imposes, measured explicitly). It does not
change `auto`, call a model, emit live compaction telemetry, or modify the
provider session. A requested floor may remain unreachable rather than
dropping a pinned item.

```sh
gobstopper eval-study /private/source.jsonl --prepare-manifest /private/checks.json
gobstopper eval-study /private/source.jsonl --manifest /private/checks.json --rounds 10 --trigger 1 --floor 40000 --json
```

Preparation refuses an existing destination and writes only hashes, byte spans,
JSON pointers, types, and opaque check IDs, not transcript text. Its labels are
**heuristic candidates, not audited truth**: at most 16 complete lines per type,
with elidable records considered first and source order breaking ties. Reviewed
manifests can instead use `label_source = "reviewed"`; classification coverage
is not measured by retention. The JSON schema is `gobstopper-retention-v1`, with
`source_sha256`, `label_source`, and `checks` entries containing `id`, `kind`,
`record_index`, `pointer`, `start_byte`, `end_byte`, and `sha256` of that exact
UTF-8 span. Types are `constraint`, `procedure`, `open_task`, `fact`, `preference`,
and `episode`. Only the first three are pinned. Source identity, live context,
text-only pointers, span boundaries, duplicate IDs, and hashes are checked
before any replay. Limits: 64 MiB of source for replay (512 MiB — the vault
transit bound — for score-only manifest prep and `--against` audits),
1 MiB of manifest, 256 checks, 4 KiB per span, and 1–10 rounds.

The report separates text presence, same-origin presence, and preservation at
the original source record/pointer. `lexical_retained` is a paraphrase-sensitive
middle tier: a check counts when ≥75% of its normalized content tokens
(lowercase alphanumeric, ≥4 chars, stopwords removed) appear together in one
live slot — useful when a provider summary rephrases rather than repeats, but
it is token coverage, not semantic equivalence. `by_kind` holds `[total,
source-bound retained, lexical retained]`; the elidable subset is reported
separately. Dead
branches and metadata cannot satisfy a check. Pre-existing source verification
errors and newly introduced errors are counted separately. Counts are not
semantic or behavioral scores. Estimated context uses adapter item estimates, not stale provider usage records
or billing. All arms use the same policy, including minimum savings and the
protected recent tool-output tail.

`--against AFTER` switches to a score-only realized audit: the manifest binds
to the session's before-state and retention is scored against independent
after-bytes — no replay, no mutation. Either spec may be a `vault:<sha256>`
snapshot reference (Devin store snapshots are exported to the transcript
dialect first). `scripts/retention-audit.py` scans the vault for consecutive
snapshots whose provider compaction-marker count increased (Claude
`compact_boundary`, Codex `"type":"compacted"` — hook bracket labels alone can
miss the actual write), pairs surgery-labeled snapshots with the next
snapshot, and runs the audit over each pair: realized, per-kind retention of
compactions that already happened — including provider-native ones. Devin's
marker is `metadata.summarized_from` — its `/compact` appends a summary node
rather than rewriting history, so expect flat context deltas and nonzero
source-bound retention.

Without new work, replay is explicitly `static_stress`; unchanged passes do not
count as applied compactions. For Codex/Claude fixtures, optional `growth`
entries (`after_round`, `records`) append complete provider records between
rounds and are verified before use. Checks still refer to the initial source;
this is not a test of revised tasks, independent tasks, or agent reasoning.
Devin growth is rejected until provider-authored chain progression is supported.
Provider-native compaction, semantic summarization, continuation success, cost,
and retrieval are explicitly **unmeasured**, not successful or zero-cost
comparators. The built-in `structured` strategy is not used as a substitute for
a semantic summarizer.

A bounded pilot can freeze up to eight selected session exports and register its
protocol before outcomes. Choose a new private output directory outside Git:

```sh
python3 scripts/compaction-study.py --binary target/release/gobstopper --output /private/new-pilot --session SESSION_ID
```

It pins the executable, source exports, annotation manifests, and hashes;
keeps content private; uses isolated config/telemetry paths; and checks that the
frozen inputs remain unchanged. There are no provider calls. Commands have
output/deadline limits and the study has a 900-second overall deadline.

The separate synthetic-only provider qualification probe makes at most three
Claude commands, capped at $0.25 each, using an isolated configuration directory,
no tools, safe mode, and no MCP servers. It requires explicit opt-in and stops
if that isolated profile is not authenticated; it never copies credentials.
It checks for a persisted native compaction boundary before testing recall.
`--seed-style baseline` uses explicit test framing; `--seed-style naturalistic`
embeds the identical facts in a plausible work narrative; `constraints` makes
the seed rule-dense; `pinned` keeps the rules out of the transcript entirely —
they ride in `--append-system-prompt`, the provider's own pinned-context
channel (safe mode disables CLAUDE.md discovery), while conversational facts
still go through the summarizer. `claude_md` exercises the production pin
channel instead — the same rules land in a workspace `CLAUDE.md` and the arm
drops `--safe-mode` so project memory loads (the isolated config home and
scratch workspace remain the boundary). Rule-bearing styles add a `rules[]`
recall scored per-marker as `constraint_rules_recalled`. Recall is scored twice:
strict exact match (`recall_checks_passed`) and containment
(`recall_checks_lenient`), so a semantically preserved superset answer is not
indistinguishable from a lost fact. After interactive login in that isolated
profile, a fresh probe output directory can reuse it with
`--auth-home /private/previous-probe/claude-home`:

```sh
python3 scripts/provider-retention-probe.py --claude-bin /absolute/path/to/claude --output /private/new-native-probe --allow-provider-calls
```

A passing synthetic probe is not a four-arm real-session comparison or evidence
of billed savings. No new strategy is activated in the watch daemon by these
commands. Design references: [Knowledge Triage](https://arxiv.org/abs/2608.22752),
[The Complexity Trap](https://arxiv.org/abs/2508.21433),
[SelfCompact](https://arxiv.org/abs/2606.23525),
[ACON](https://arxiv.org/abs/2510.00615), and
[LongMemEval](https://arxiv.org/abs/2410.10813).

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

### Monitoring an existing Codex desktop session

Standalone `watch` cannot compact the context already held by another Codex
process. It reports native delegation as `skipped`, with zero credited savings;
the owning runtime must perform that operation. `--active-only` limits discovery
to files updated within the last 180 seconds (a recency heuristic, not proof of
an owning process), and `--once` exits after one pass. A dry run writes no forks
or compaction events. Existing trusted lifecycle hooks can snapshot native
compactions and provide the session with a recovery pointer.

The optional [local monitor](scripts/monitor.md) records numeric observations
for an explicit list of sessions and checks a deterministic dry-run watcher.
It separates observed context drops, native hook activity, and projected
compaction plans; none is automatically counted as Gobstopper-caused usage
savings. Current Codex `event_msg/token_count` accounting and legacy usage
records are both supported, including the advertised model context window.

`scored` uses the deterministic offline heuristic by default. Experimental
model scoring is opt-in with `GOBSTOPPER_SCORER=llm`,
`GOBSTOPPER_SCORER=jev`, or `GOBSTOPPER_SCORER=apple`; merely setting an API
key never sends data. Remote Jev and LLM scorers receive bounded labels and
summaries, including tool arguments, short output tails, and user-prompt
snippets. These are transcript-derived text, not redacted metadata; enabling
a remote scorer sends them to its configured endpoint even when additional
content excerpts are disabled. Apple scoring runs on-device. All model
scorers retain deterministic heuristic scores whenever a model omits an
answer or a request fails. Hosted LLM settings are hard-capped
at 256 candidates, 64 items per batch, 16 batches, and a 100–30,000 ms
timeout. The built-in heuristic remains the recommended published path because
current live trials did not show a better plan from the LLM scorer.

The `scored` strategy also supports an optional keep-score cutoff. For example,
add this named preset to your config:

```toml
[presets.retained]
strategy = "scored"
keep_score_threshold = 0.5
```

Inspect it with `gobstopper plan <session-id> --preset retained`; use the same
flag with `gobstopper apply` to create a fork. Candidates
at or above the cutoff are preserved, even if that prevents reaching the token
target. Missing, invalid, or duplicate candidate scores are also preserved.
The value must be finite and between `0.0` and `1.0`. The heuristic score is a
ranking signal, not a calibrated probability; `0.5` is an experimental example,
not a tuned recommendation. Model scorers still use heuristic fallback for
partial or failed responses, so the cutoff does not guarantee provider
confidence. Set `strategy = "scored"` explicitly: `auto`, `compacted`, and other
strategies ignore the cutoff. Omitting it in a later configuration layer
inherits an earlier value rather than clearing it. The default configuration
has no cutoff and retains its existing behavior.

`GOBSTOPPER_SCORER=jev` scores with TypeSafe's System One API — typed
`noul` keep-probabilities without prose generation. Onboarding vaults the key
in the OS credential store
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
show a one-time keychain access prompt on first read. Remote scorers require
curl 8.3+: bearer credentials are imported from a child-only environment
variable and expanded inside curl, never placed in process argv; Gobstopper
also disables `.curlrc` for these calls so user defaults cannot enable verbose
header logging.
`GOBSTOPPER_JEV_CONTENT_BYTES` (default `0`, maximum `1024`) opts in to
attaching additional bounded per-candidate content excerpts to each question.
The remote scorer already receives the bounded labels and summaries described
above. A post-fix 141k-token A/B run selected the same six records with and
without 400-byte additional excerpts, so those excerpts remain off by default.
Every numeric runtime knob is clamped: 1–64 questions per call, 1–128 state
items, 100–30,000 ms timeout, 1–16 batches per scoring pass, and 1–4
concurrent calls (`GOBSTOPPER_JEV_PARALLEL`, default `2`).
`GOBSTOPPER_JEV_MAX_BATCHES` defaults to `4`. Only the newest
`MAX_Q × MAX_BATCHES` tailward candidates are sent; an older prefix keeps its
deterministic heuristic score. This caps the default at four calls and 256
remote-scored candidates even for unusually large transcripts. Identical
question texts within a pass are asked once: repeated tool outputs share a
single remote answer instead of being billed per item. Batches run through
a bounded worker pool: execution is parallel, but results are overlaid in
stable order, a failed or panicked batch retains heuristic scores, and
cached answers still overlay when a batch's remote half fails. Each pass logs one summary line
to stderr (candidates, unique/cached/sent questions, calls, failures,
elapsed). In a historical trial recorded on September 19, 2026, one three-batch
336k-token Claude session took 148.64s before bounded parallelism and 19.47s
afterward (about 7.6×). This single-session latency observation does not
establish current or provider-wide performance.

`eval` and `bench` now honor `GOBSTOPPER_SCORER` for their `scored` row, so
an A/B run measures the same Jev or Apple ranking used by `plan` rather than
silently substituting the heuristic. `GOBSTOPPER_EVAL_JUDGE=jev` adds a
separate semantic recall score to `eval`. Verbatim survivors are credited
locally; only up to 64 facts absent from the rewritten file become typed
`noul` questions. They are judged against at most 100,000 bytes of bounded
compaction evidence (state cards, elision stubs, and short tool records, with
a head+tail fallback), so semantic recall cannot be lower than verbatim
recall and an intact rewrite costs no judge request. The judge is off by
default because missed-fact evaluation sends that bounded evidence to the
remote API; failures simply omit the semantic score. Restricting the run to
`--strategy scored` uses at most one judge request:

```sh
GOBSTOPPER_SCORER=jev GOBSTOPPER_EVAL_JUDGE=jev \
  gobstopper eval <session> --strategy scored
```

Gobstopper reads the official `answers.<id>.noul` probability returned by
System One, while retaining bounded compatibility fallbacks for older response
shapes. Jev starts from the complete deterministic heuristic ranking and
overlays only valid remote answers; capped candidates, missing answers, and
failed or malformed chunks keep their heuristic scores. Semantic eval omits
its model score instead of crediting unknown facts. In one post-fix 80k-token
A/B run, Jev chose five smaller records where the heuristic chose four larger
ones, reclaimed about 649 more tokens, and both retained all 38 extracted
probes. At a more aggressive floor, one probe lost verbatim was not falsely
credited by the semantic judge (37/38 on both scores). This is single-session
qualification, not a general ranking-quality claim.

Successful Jev answers are cached in-process for five minutes under two
scopes. The scorer caches each question together with the complete scoring
state and model identifier. Unchanged polls reuse judgments, including across
different question batches. A changed goal or tail requires fresh answers
when that change is represented in the bounded scoring state; the cache cannot
detect task changes omitted from that state. The eval judge caches whole exact
requests.
Cache keys cover endpoint, credential identity, and question plus scoring
state or full request text; values are
parsed probabilities only (never transcript text), evict oldest-first at
512 questions and 64 requests, and failures are never cached. The
question layer also persists to
`~/.local/share/gobstopper/jev-cache.json` — only sha256 key digests
mapped to probability + timestamp, never text — so a cold `plan` inside
the TTL can reuse an answer when its question and scoring state match.
Older question-only cache entries cannot match the context-bound keys.
Transient
transport errors and HTTP 5xx responses are retried once; auth rejections
are not. The scorer and judge resolve the API key once per process, so
`watch` does not re-read the OS credential store every pass. Set
`GOBSTOPPER_JEV_CACHE=0` or `GOBSTOPPER_JEV_CACHE_TTL_SECS=0` to disable
both layers;
`GOBSTOPPER_JEV_CACHE_PATH` relocates the disk file;
the TTL maximum is 3,600 seconds.

`GOBSTOPPER_SCORER=apple` (macOS 26+, Apple Silicon) scores on-device with
Apple Intelligence Foundation Models via the shared `apple-foundation`
bridge — free, private, no API key. The bridge auto-builds to
`~/.local/share/gobstopper/apple-bridge` on first use (or set
`GOBSTOPPER_APPLE_BRIDGE`), requests queue through one persistent process
with guided JSON output, and any failure retains heuristic scores.
`GOBSTOPPER_APPLE_TIMEOUT_MS`, `_MAX_CANDIDATES`, `_BATCH_SIZE`, and
`_MAX_BATCHES` tune it, hard-capped at 100–600,000 ms, 256 candidates, 64
labels-only items per batch (8 with content), and 16 batches. Since inference
is local, the scorer also reads a bounded excerpt of each candidate record
(`GOBSTOPPER_APPLE_CONTENT_BYTES`, default and maximum 400; `0` restores
labels-only scoring) and shrinks its default batch sizes to fit the ~4k-token
context window. Each batch's guided schema contains one required `p_<id>` field
per candidate, so omitted or duplicate array IDs cannot silently distort the
ranking; a malformed batch retains its heuristic scores.

`GOBSTOPPER_DIGEST=apple` goes further: the injected state card is written
by the on-device model instead of keyword extraction. Because inference is
local, it may read bounded excerpts of the records being elided without a
remote request. Each field still
lands in the same `DigestBlock` shape via guided output, capped to a small
token overhead, and falls back to the mechanical card on any failure.
`GOBSTOPPER_APPLE_DIGEST_ITEMS`, `_ITEM_BYTES`, and `_TOTAL_BYTES` tune the
excerpt budget, hard-capped at 32 records, 2,048 bytes per record, and 16,000
bytes total; zero disables the model digest and preserves the mechanical card.

The same call also writes a one-line stub per excerpted record — e.g.
`Script completed Wall time 4.4 seconds` — stored in the elide edit's
`per_item_stubs` map and rendered verbatim in place of the `{bytes}`/`{kind}`
template where the payload was removed. Records the model did not cover
keep the generic stub; invalid or oversized stubs are dropped by validation.

Apple requests are cached in-process on the SHA-256 of the (prompt, schema)
pair: `watch` re-evaluates an unchanged transcript every poll interval, and
identical model inputs return the recorded response instead of another
generation — a live dry-run poll went from ~17s to milliseconds per re-eval.
The cache is bounded at 64 entries with oldest-first eviction;
`GOBSTOPPER_APPLE_CACHE=0` disables both reads and writes. Scorer diagnostics
report cached batches separately from real model calls. The savings gate
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

With `[provider.codex] auto_compact_closed`, `watch` drives
`thread/compact/start` itself on idle over-trigger threads via
`codex app-server` (pre/post vault snapshots + realized retention on the
event, same as the devin acp path). Multi-agent v2 sub-agent threads cannot
be resumed by the app-server and are skipped (`watch-apply:sub-agent`
events); plain forks resume normally. Terminal provider outcomes — quota
rejections, structural resume failures, an account plan without the remote
compact task's server-side model, unconfirmed turns — suppress the
session on a session-keyed cooldown (a failed turn still rewrites the
rollout, so a fingerprint-settle cannot hold); transient errors like a
missing `codex` binary retry on the next pass.

## Layout

- `crates/gobstopper-core` — normalized transcript model, the `Edit` IR,
  `Strategy` trait, and all built-in strategies. No I/O.
- `crates/gobstopper-adapters` — bounded session discovery, Codex/Claude
  JSONL parsing and candidate rewriting, transactional no-clobber publication,
  verification, plugin hosting, and the snapshot vault.
- `crates/gobstopper-cli` — `gobstopper` binary: detect / plan / apply /
  verify / undo / vault / history / show / recall / diff / bench /
  mcp / watch / policy-check / presets / explain.

## Current benchmark evidence

The [September 19, 2026 retrospective](https://gobstopper.sh/benchmarks#retrospective-2026-09-19)
evaluated 729 frozen sessions on one Mac. Portable `compacted` projected a
**36.4% median reduction across 73 high-context archived Codex roots**, with
**76.9% sampled-string retention**. Across all 729 sessions, 637 produced no
plan and the median reduction was **0%**. These are offline projections, not
billing savings or task-accuracy measurements. The page includes all cohorts,
limitations, and downloadable aggregate results and methodology.

A separate [paired scored-policy replay](https://gobstopper.sh/benchmarks#retention-policy-2026-09-19)
reused the 114 archived roots with the corrected probe limit. This development
comparison is not held-out validation; the original baseline was already known.
On the 73
high-context tasks, a `0.35` cutoff increased sampled-string retention from
77.05% to 80.78% while median projected reduction fell from 36.44% to 34.96%.
Two tasks produced no plan, and their retention is derived from leaving the
source unchanged. All three registered cutoffs and both disabled baselines are
reported; the default remains unchanged. This is a limited retention/size
tradeoff, not a task-quality result or a recommended optimum.

## Historical live qualification — recorded September 17, 2026

The following single-session experiments were recorded in the repository on
September 17 using earlier builds and workflows. They are separate from the
729-session retrospective and do not establish current provider-wide savings
or general task quality. One 333k-token Claude session was asked the same
resume question under four conditions, measuring provider tokens on that turn:

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
them by **43%**, both with accurate answers to that question. Provider-native
Codex compaction was not included in this historical trial.

The same-session `cache_aware` A/B (339k-token Claude session, floor 310k,
real provider cache counters):

| condition | `cache_read` | `cache_creation` | file-level prefix preserved | cost | accurate? |
|---|---:|---:|---:|---:|---|
| none (original) | 10,010 | 325,647 | — | $6.53 | yes |
| `gobstopper cache_aware` | 13,536 | 258,517 | 107,884 tokens | $5.19 | yes |
| `gobstopper compacted` | 13,536 | 257,505 | 6,639 tokens | $5.18 | yes |

In this trial, `cache_aware` and `compacted` had almost the same API cost and
both recorded 13,536 cache-read tokens, versus 10,010 in the baseline.
`cache_aware` preserved **16x more identical transcript prefix**, an
auditability result; this trial did not establish extra provider cache savings
from the preserved prefix. Current `compacted` uses a portable forked digest;
synthetic Codex-native records require `--experimental-compacted`.

Snapshots and `gobstopper diff` make compaction inspectable and provide a
recovery path. They do not guarantee that omitted facts are unimportant or
that a continuation will retrieve them automatically.

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
remain explicitly experimental or trusted extension paths. Devin support covers
detection, numeric policy/MCP handoff, closed-session `acp` compaction, and
vault-exported session snapshots — never direct transcript surgery.
See [docs/design.md](docs/design.md), [docs/roadmap.md](docs/roadmap.md),
[docs/plugin-protocol.md](docs/plugin-protocol.md), and
[docs/devin.md](docs/devin.md) for the boundaries.

## License

MIT OR Apache-2.0

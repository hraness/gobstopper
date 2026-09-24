# Held-out retention and continuation study protocol

This protocol registers a comparison of unchanged context, observation masking,
and typed preservation on frozen sources and independently reviewed labels.
It is design version 1, dated September 24, 2026. Its execution status is
**not started**. This change supplies no study corpus, reviewed labels,
continuation results, or provider qualification. The software regression tests
use synthetic responses and make no provider calls.

## Questions and outcomes

The primary offline question is whether typed preservation retains more
independently selected task requirements than plain observation masking, at
its measured context cost. The primary continuation question is whether a future
agent completes the registered task without violating a critical constraint.
Offline retention scores cannot answer the continuation question.

Record these outcomes separately:

| Outcome | Measurement | Unit |
|---|---|---|
| Structural validity | Source and output parser/verifier errors, including pre-existing errors | Source and arm |
| Literal retention | Exact evidence text present in the selected live context | Registered check |
| Lexical retention | Frozen tokenizer and threshold applied to one context slot | Registered check |
| Source-bound retention | Retained evidence remains attached to its registered original record and role | Registered check |
| Critical constraint behavior | Executed continuation attempts a forbidden action or omits a required action | Source and arm |
| Task completion | Independent executable assertions over the continuation's outputs | Source and arm |
| Cost and resource use | Context estimates, measured usage components, elapsed time, refetch work, and reported charges with coverage | Source, arm, and attempt |

A recalled rule is never scored as an executed constraint. A JSON answer such
as `migration_allowed: false` is a recall observation unless an independently
observed continuation tests the action. Semantic-equivalence and universal
preservation claims are outside this protocol.

## Corpus and frozen assignment

Use public synthetic fixtures or separately authorized private frozen exports.
Read original sources without mutation. Keep private source bytes, labels, and
artifacts in a new directory with mode `0700`; publish only authorized summaries.
Reading a frozen export does not require provider authentication or a model call.

The target corpus is 45 unique valid source groups: 15 development sources and
30 held-out sources, with five development and 10 held-out sources from each
supported provider dialect. This is a fixed exploratory sample, not a power
calculation or a population-representative sample. If the target cannot be met,
stop enrollment and record the shortfall before evaluating any held-out arm.
Changing the target requires a dated amendment before outcomes are inspected.

Create an enrollment manifest before transformations. Record each source's
canonical byte hash, dialect, original source identity hash, source-group ID,
lineage, byte and estimated-token size, eligibility decision, and reason. Use
one canonical frozen image for every arm. Identical content hashes and sources
from the same session, fork, or task family belong to one group. Detect and
review near-duplicate synthetic templates before assigning groups; different
receipt strings alone do not make independent sources. Grouping decisions
remain frozen for all analyses and resamples.

Use the fixed assignment seed `gobstopper-retention-heldout-v1-20260924`.
Classify each provider's valid groups by the frozen 40,000 estimated-token floor
and the checked build's estimator. Within each provider and floor stratum, sort
eligible groups by SHA-256 of the UTF-8 seed, a NUL byte, and the canonical group
ID. Among groups above the floor, assign the first four to development and the
next eight to held-out evaluation. Among groups at or below the floor, assign
the first to development and the next two to held-out evaluation. Record
excess candidates as unenrolled with their reasons. Freeze the manifest hash
before exposing development results. Never move an inconvenient held-out source
into development.

Keep these floor strata separate in the results. Cases at or below the floor
remain controls for unintended changes and do not inflate the count of
successful transformations. If the available corpus cannot satisfy the strata,
register the shortfall before starting.

## Independent labels and continuations

For each source, a label author reads only the original frozen source and its
task specification. The author records the required facts, constraints,
procedures, pending work, original roles, exact evidence locations, and the
reason each item matters to the task. Each label has a stable ID, criticality,
scoring rule, and an explicit unknown state. Do not infer a requirement from a
transformed output or use the transform's own selected pins as ground truth.

A second reviewer, independent of the transform implementation and label
author, checks every held-out label against the original source. Record author
and reviewer identities and whether each is a person or an AI agent. Resolve
disagreements before freezing; retain the disagreement and resolution history.
Sources with unresolved critical labels are excluded with an explicit reason
before arm outputs exist. An AI review is recorded as an AI review.

The transformation may use development labels while tuning. It must not read
held-out scoring labels, expected answers, task assertions, or their output
paths. Any production labeler used by the typed arm is frozen after development
and operates only on the ordinary source input. A runner that passes the same
label file to both preservation and scoring cannot implement this comparison;
adapt and test that separation before execution. Add a negative control that
changes a held-out scoring label and confirms identical transformed bytes.

Define one primary continuation per held-out group before arm outputs are
created. Use disposable fixture repositories and local simulated services,
with executable success checks and forbidden-action logs. For example, a
fixture may require a change within an allowed directory, preservation of an
in-flight job, and an exact rollback verification command. The assertions are
withheld from the agent and transform. No real deployment, migration, payment,
user repository write, or provider session mutation is a task action.

## Arms and execution order

| Arm | Treatment | What it establishes |
|---|---|---|
| `no_compaction` | Original frozen context, byte-for-byte unchanged | Baseline retention and continuation difficulty |
| `observation_masking` | Existing plain masking with the frozen policy | Effect of removing disposable observations |
| `typed_masking` | Frozen typed selector and masking policy without held-out evaluation labels | Effect of independently selected preservation |

Use the same checked source/build, budgets, floor, keep-recent setting, and
task inputs for both transformed arms. Freeze the full effective configuration,
token estimator identity, plugin/model identities if any, and source hashes in
`registration.json`. Default to deterministic built-ins without executable
extensions. This offline protocol authorizes no new hosted model call.

Each source is its own paired control. Choose the arm execution permutation by
hashing the assignment seed and group ID; record it before running. Use fresh
isolated output directories and byte-identical inputs. Preserve every first
attempt. An infrastructure retry is a linked new attempt with the original
failure retained; it does not add another unique source or silently replace
the primary outcome.

The primary offline endpoint is after one transformation. Up to 10 replay
rounds may be recorded as a secondary stability check, with hashes that show
whether additional rounds change anything. Repeated identical output does not
provide additional independent evidence. Pin injection on every turn, project
memory retrieval, typed digests, and native compaction are different treatments
and require separate registrations; they are not pooled into these arms.

## Invalid and oversized inputs

Register a separate robustness cohort with two malformed and two oversized
fixtures per provider dialect. Construct these fixtures from public synthetic
data, retain their generator seeds, and label the deliberate defect or limit.
Do not use them in the primary retention or task-success denominator.

Freeze the implementation's exact admission, export, and output limits before
running. Classify oversized sources before transformation, and retain cases
that exceed a later output limit. Malformed-source errors, transform-introduced
errors, above-limit refusal, timeouts, and unreadable inputs have separate
statuses. No failure disappears because it lacks a score. The summary lists
enrolled, admitted, transformed, scored, refused, and failed counts per stratum
and arm. Valid below-floor sources remain their own stratum.

## Analysis and cost completeness

The primary retention statistic is each held-out source's fraction of reviewed
checks retained with their original source binding, followed by the mean paired
difference between typed and plain masking across unique source groups. Literal
and lexical retention are secondary endpoints. Report per-provider results and
critical checks separately. Also publish raw check totals, but do not treat checks,
replay rounds, retries, or duplicate pilot exports as independent sources.
Report every assigned arm, including the unchanged arm and all failure statuses.

Use a paired cluster bootstrap over source groups for an exploratory interval:
10,000 resamples and the frozen assignment seed. Report the number of unique
groups and the full paired outcome table beside the interval. For failed or
unscored transformations, show the completion rate and an all-enrolled bounded
analysis assigning missing retention outcomes both zero and one. Do not turn
missing evidence into a single zero estimate. No statistical test from this
small convenience corpus establishes a general semantic guarantee.

Continuation task success and critical violations have their own paired table,
denominator, and missingness reasons. Provider refusals, invalid responses,
timeouts, and structurally unusable contexts remain assigned attempts, with
task success unknown if no valid continuation ran. Show both completed-only
outcomes and the bounds implied by all assigned missing outcomes. Until a
qualified continuation runner executes this stage, report it as not run.

Keep input, output, cache-read, cache-write, tool/refetch, and provider-reported
charge components separate. A null or absent component stays unknown; a
reported zero remains an observation. Store raw units, the usage source,
attempt/session identity, and whether the measurement covers the whole
attempt. A partial subtotal is never labeled total context or complete cost.
Report transform, continuation, retrieval, and retry costs separately and in
a combined total only when all required components are present. Preserve
cache state and order when known; uncontrolled cache state limits causal cost
comparisons. Estimated token reductions are not billed savings, and reported
charges are not invoices.

## Artifacts and admission to a future run

Freeze the registration, source manifest, labels and reviews, and continuation
specifications before any held-out run. Retain the attempt and result files as
execution proceeds, without replacing earlier attempts:

- `registration.json`: protocol version/hash, exact code and binary identities,
  configuration, toolchain, limits, seed, arm order, endpoints, and amendments.
- `sources.json`: all considered sources, frozen grouping/assignment, size and
  validity strata, hashes, consent scope, and every exclusion reason.
- `labels.json` and `label-review.json`: held-out labels, expected outcomes,
  provenance, independent review, disagreements, and final frozen hashes.
- `continuations.json`: task fixtures, executable assertions, forbidden actions,
  time limits, and independent review. This file may remain registered and
  unexecuted while the offline retention stage runs.
- `attempts.jsonl`: one row for every assigned attempt, including failures,
  linked retries, elapsed time, output hashes, and measurement completeness.
- `results.json`: per-source and aggregate outcomes with exact denominators,
  separate recall and behavior results, and all unexecuted stages identified.

Test the runner with synthetic fixtures for malformed/non-object answers,
duplicate JSON keys, missing criteria, cost gaps, source duplication, label
leakage, retry accounting, and missing result artifacts. Check unchanged input
hashes after every arm. Preserve source/configuration changes as failures rather
than continuing with a different experiment. A plan or registration alone is
not an execution receipt.

## Provider qualification remains separate

An offline export comparison does not qualify a provider to resume a copy or
accept native compaction. The released native-dispatch guard remains disabled.
Before a native or hosted continuation study, satisfy the applicable isolated
provider/version cells in [provider qualification](assurance/qualification.json),
including target/session ownership, operation correlation, recovery, timeout
handling, and readback evidence. Register the account, exact provider/model
version, permitted data, command count, budget, and cleanup beforehand. A
synthetic recall probe is one observation and cannot satisfy those cells by
itself. New paid calls and operational activation require their own authorized
scope; this protocol performs neither.

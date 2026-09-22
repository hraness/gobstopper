# Experimental owned Codex sessions

`gobstopper codex-session --experimental` runs a **new Codex session owned by
Gobstopper**. It can evaluate an early-compaction policy between completed turns.
It does not attach to an existing Codex Desktop, CLI or Oompa-owned thread.
Installing Gobstopper's lifecycle hooks alone does not enable this behavior.
Build the current source to use this command; the published v0.2.1 release
artifacts predate it. Identify development installations by their executable hash.

The initial implementation pins Codex CLI **0.155.0**. It uses the documented
[app-server](https://learn.chatgpt.com/docs/app-server) interface and reads the
owned rollout after a completed turn. That rollout dialect is a pinned dependency,
not a promise that future Codex versions use the same storage format.

## Modes

| Mode | Action at an eligible idle boundary |
| --- | --- |
| `off` (default) | Leaves Codex's native compaction timing in control. Records usage. |
| `native` | Requests `thread/compact/start` and waits for the correlated native compaction turn. |
| `custom` | Preserves a private source snapshot, applies Gobstopper's validated transformation, creates a new provider thread, injects the retained history, then commits the new thread mapping after acknowledgement. |

Custom mode preserves the old provider thread. The logical session continues in
a **different provider thread**; it does not replace the in-memory context of an
existing Desktop task. It elides selected old text tool outputs, protects a
configured prefix and recent outputs, and appends a bounded state card. Tool
pairing, order and opaque nontext values are validated before injection.
The state card is lossy: arbitrary facts in elided text may be lost.

A preserved content prefix does not guarantee a cache hit. New-thread overhead,
provider instructions, compaction requests and later reasoning can outweigh
reduced context. Projected reductions are never recorded as actual savings.

## Starting a session

Use an existing authenticated Codex home and an empty, private state-directory
path. Choose the model and reasoning effort explicitly; every continuation uses
the same choices.

```sh
gobstopper codex-session --experimental \
  --state-dir "$HOME/.local/share/gobstopper/my-new-session" \
  --cwd /absolute/path/to/project \
  --model YOUR_MODEL --effort YOUR_EFFORT --mode custom
```

The parent of `--state-dir` must exist. The state directory must not already
exist. The process accepts JSON lines on stdin and emits JSON lines on stdout:

```json
{"type":"turn","text":"Describe the public project structure."}
{"type":"stop"}
```

Wait for `ready` before sending work, and for `turn_completed` before sending the
next command. Integrators can send `{"type":"boundary"}` to evaluate policy before
supplying the next task input; wait for `boundary_complete`. An `inject` command
accepts an `items` array of complete user/assistant messages or linked tool pairs
and emits `injected`. It is intended for explicitly supplied evidence, not for
claiming that a tool was autonomously called. Assistant text is returned in `assistant` events. Numeric receipts
and private recovery snapshots are written under the state directory. Treat that
directory as sensitive because snapshots contain the session history.

The default sandbox is read-only; `--workspace-write` explicitly permits writes
inside the workspace. Approval policy remains `on-request`. This first controller
emits `approval_required` and stops when a provider action needs client approval;
it does not yet provide an interactive approval UI. Do not use it as an unattended
replacement for a full coding client that needs such approvals.

The controller requires exclusive ownership and deliberately rejects reopening
an existing state directory. After an interruption, inspect the receipts and
retained provider threads with an appropriate client. Unknown outcomes are not
automatically retried. Do not open a provider thread concurrently while its
controller is still running.

Policy defaults remain a 250,000-token trigger, 40,000-token projected floor,
eight protected recent outputs, a 16,384-token estimated protected prefix, a
4,096-token minimum projected reduction, and cooldowns of 300 seconds, three
completed turns and 8,192 tokens of growth. These are planning defaults, not a
measured optimal policy. Every value can be overridden on this subcommand without
changing the installed hooks or existing session configuration.

## Live qualification

The synthetic trial under `scripts/early_compaction/` compares three separately
created sessions: native timing (`off`), early native (`native`) and custom
continuation (`custom`). It supplies public synthetic tool evidence in stages,
withholds later demand until after the intervention, and verifies the final
answer against a local deterministic oracle. Supplied evidence is not a test of
autonomous retrieval. No private benchmark histories are needed.

Keep the model, reasoning effort, instructions, tools, fixture seed and policy
fixed across arms. Record executable identities, exact boundaries and all
response-level usage, including native compaction. Missing usage makes an arm
incomparable. Failures and partial trials remain evidence; a rerun needs a named
repair or a separately registered repetition.

Compare total input, cached input, **uncached input**, output, latency and task
correctness. Include the first request after intervention and the remaining
continuation. One successful synthetic trial establishes only bounded protocol
and task compatibility. It does not establish production savings, account quota
savings, superior task quality or benefit for existing Desktop tasks.

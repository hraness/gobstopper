# Recovery and guarded operations

This runbook describes the correctness-foundation implementation. Operational
qualification is a separate obligation: a passed fixture or proof does not
establish compatibility with an installed provider version or account.

## Provider files and settings

Direct Codex/Claude transcript replacement is disabled. A Gobstopper lock is
not custody honored by a provider that already has the file open. Codex/Claude
copy preparation retains the exact original and candidate in the vault and
publishes a new session without replacing a target.

`install-hooks --output <new-file>` and `uninstall-hooks --output <new-file>`
export an inert settings bundle. Without `--output`, both commands refuse.
The output parent must exist, and the output must be a new file. Publication
uses a private file and no-clobber semantics. The bundle includes original
settings bytes, their SHA-256, the candidate and its SHA-256; it may therefore
contain private configuration. Nothing automatically applies it.

Candidate generation preserves unrelated keys, handlers and ordering. Removal
recognizes an exact event, matcher and handler, including its timeout. A wrapper
command containing `gobstopper hook`, a different matcher, or additional handler
fields is not considered owned. Review the candidate through the provider's
settings controls, retain trust prompts, and verify the source precondition in
that provider-owned transaction. Copying a candidate over a concurrently edited
file is not a supported transaction.

## Native operations with an uncertain outcome

Released CLI native dispatch is currently guarded because no provider/version/mode
has qualified live ownership and correlation. This also blocks prior
`auto_compact_closed` opt-ins. The machinery below is exercised by exact isolated
debug fixtures; it does not enable a real provider call. See the
[activation matrix](qualification.json) for admission and future activation criteria.

Native dispatch keeps an append-only journal under
`$XDG_DATA_HOME/gobstopper/native-operations-v1` (default
`~/.local/share/gobstopper/native-operations-v1`). A canonical provider, home,
store and exact session identify a target. Cooperating watchers share a stable
target lock. A durable recovery pin and prepared record precede the durable
dispatched record; dispatch persistence must succeed before calling a provider.

`gobstopper native-operations` inspects operation metadata without calling a
provider. `dispatched` and `unknown` remain blocked across restarts, cooldown
expiry and policy changes. Deleting a watch cache does not clear them. Damaged,
missing or renamed registered journals require repair. Do not delete journal,
registry, lock or pin files to force another attempt.

`gobstopper native-reconcile <exact-operation-sha256>` accepts only already
recorded matching Codex session/turn/item terminal evidence on an unknown
operation. It accepts no caller-supplied success flag and makes no provider
call. Reconciliation records evidence; it does not manufacture a usage
measurement or savings. Missing evidence and weaker session-only contracts
remain blocked. The Codex contract assumes the first compaction item on its
private connection belongs to the requested operation; that association still
requires provider qualification.

Legacy watch state predating generation 9 did not distinguish every uncertain
result. Its native suppression keys are conservatively blocked across watch
lanes, even when their old cooldown has expired. Corrupt or newer state is not
treated as an empty cache. Downgrading to a binary that ignores the journal is
not a safe way to clear an unresolved operation.

## Copies, archives and retention

Version 2 copy receipts bind source identity, effective inputs, source bytes and
candidate bytes. Recovery reuses those retained bytes; it does not rerun a
strategy that may have changed. An existing exact output may be reconciled;
conflicting bytes, a missing completed output, and legacy pending receipts
without a retained candidate require explicit repair.

Non-transcript archive objects remain readable as archive bytes but are refused
as replay inputs. Replay never materializes them into shared temporary filenames
or attempts to repair provider state.

Evaluation validates command-line policy overrides before invoking scorers.
`bench --output <new-file>` publishes one private, no-clobber CSV artifact; an
existing destination is an error, including when it names a transcript or store.
Per-session policy or evaluation failures remain explicit rows with missing
measurements and closed failure categories.

Adaptive past-yield tuning also requires the canonical provider/session/store
identity and valid paired context reports. Legacy estimates, foreign stores,
errors, unknown/reset usage and zero reductions do not become tuning samples.
Identical pairs count once; conflicting reports exclude the pair permanently
within the retained event history. Replay cannot move an old pair ahead of newer
measurements. This remains a heuristic over reported occupancy, not billing or
semantic-retention evidence.

Readers hold shared vault custody while resolving manifests and their objects.
Read-only history and list operations also reject torn or malformed index rows;
they never return a silently incomplete history as success.
Pruning holds exclusive custody and fails closed on malformed index entries,
unknown roots or failed manifest removal. Operation pins have no automatic
retirement policy in this version: recovery space can grow until a reviewed
retirement mechanism exists. Ordinary unpinned hook archives remain subject to
configured retention.

Hook callbacks require an exact source in the configured provider root and
matching session metadata in the same bytes archived. They lack operation
correlation, so duplicate or out-of-order callbacks only produce unattributed
observations. They do not claim applied work, savings or paired retention. A
SessionStart pointer identifies a verified earlier archive of that local
session/store; it does not claim that archive immediately preceded this
compaction. Retrieved historical text remains untrusted data.

## Managed proxy drain and restart

Protocol-1 service control binds a drain to the owned service, executable, PID,
process instance, owner token and monotonic epoch. Acquiring the waiting lease
and starting inference share one mutex. New inference is refused while existing
streams finish; status and control remain available. The waiting lease lasts at
most 30 seconds between renewals and never exceeds its original deadline. An
abandoned waiting controller therefore releases admission automatically.

The controller commits the drain only after active inference reaches zero.
Committed admission does not expire or reopen: an external service-manager stop
may complete late. The private schema-1 drain-operation journal records intent
before dispatch. A lost or failed stop acknowledgment remains unresolved and
blocks replacement activation, including when the port is free or the job is
unregistered. Do not delete that journal or downgrade to a binary that ignores
it to force startup. Such uncertainty may need operator reconciliation; this
protocol does not prove that every interruption can recover automatically.

New managed definitions pin an absolute state directory. After binding its
listener and before accepting inference, each replacement process checks the
saved manifest and drain journal. Waiting intent permits recovery; commit intent,
committed and dispatched stops, malformed data and identity mismatches refuse
startup. This keeps a delayed manager stop from reaching new inference after a
crash and supervisor respawn. Definitions installed before this guard retain
idle-only control until explicitly replaced.

Legacy idle-drain control remains limited to an idle responsive proxy. A
registered service that cannot answer identity/status checks is preserved:
observing a free port does not prevent its manager from restarting it. Native
Linux and Windows service-manager behavior requires separate platform evidence;
synthetic lease and recovery tests establish only their exercised boundaries.

## Bounds and trust

The default transcript read bound is 512 MiB; supported overrides range from
1 KiB to 8 GiB, subject to platform allocation limits. Hook settings are bounded
to 1 MiB, hook stdin to 64 KiB and five seconds. Leaf symlinks and special files
are refused at guarded file boundaries. Parent directories and executable paths
must remain owner-controlled and stable.

Advisory custody coordinates cooperating processes on the same data root. It
does not restrain hostile same-user processes, a provider ignoring that custody,
or another installation using a different root. The models assume durable
abstract writes; runtime process-death tests do not establish power-loss behavior
for every filesystem. See the [assurance ledger](ledger.json),
[vault models](../../verify/vault/README.md) and
[native dispatch model](../../verify/watch/README.md) for exact scopes.

On Unix, a duplicated or fork-inherited descriptor can retain the same lock
after another descriptor closes. Close-on-exec prevents inheritance past a
successful exec; it does not remove the interval between fork and exec. Accounting
releases its read locks explicitly at the end of each scan. Other close-based
custody ends when the last associated descriptor closes, which may occur after
the original owner exits. Extra retention can delay another cooperating process;
the safety models do not establish eventual lock acquisition or crash recovery
for arbitrary inherited descriptors.

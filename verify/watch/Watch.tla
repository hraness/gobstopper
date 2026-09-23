------------------------------ MODULE Watch ------------------------------
EXTENDS Naturals, Sequences, FiniteSets

\* A finite safety model of ONE canonical provider/home/store/session target.
\* Durable append and custody are abstract atomic operations, not filesystem
\* or provider implementation proofs. See README.md for correspondence limits.
CONSTANTS Watchers, MaxOperations, ExactCorrelation,
          AllowExpiredReplay, AllowAckSuccess, OmitRecoveryPin, AllowFallback,
          AllowFabricatedReconciliation

Operations == 1..MaxOperations
NoOwner == "no-owner"
Target == "exact-target"
Stages == {"prepared", "dispatched", "observed_applied", "observed_noop",
           "rejected", "unknown", "reconciled"}
Resolved == {"observed_applied", "observed_noop", "rejected", "reconciled"}
Unresolved == {"dispatched", "unknown"}
Outcomes == {"completed", "rejected"}
Evidence == Outcomes \cup {"none"}
Measurements == {"none", "reduced", "unchanged", "unavailable"}
Phases == {"idle", "owned", "pinned", "prepared", "checkpointed", "sent", "crashed"}
CustodiedPhases == Phases \ {"idle", "crashed"}
Event == [target : {Target}, operation : Operations, generation : 0..1,
          stage : Stages, evidence : Evidence, measurement : Measurements]

VARIABLES journal, allocated, pins, owner, phase, current,
          attempts, provider, ack, observed, foreignSeen,
          cooldownExpired, generation, fallbackCalls

vars == <<journal, allocated, pins, owner, phase, current,
          attempts, provider, ack, observed, foreignSeen,
          cooldownExpired, generation, fallbackCalls>>

LastStage(op) == IF op \notin Operations THEN "absent"
                ELSE IF Len(journal[op]) = 0 THEN "absent"
                     ELSE journal[op][Len(journal[op])].stage
LastEvidence(op) == journal[op][Len(journal[op])].evidence
LastMeasurement(op) == journal[op][Len(journal[op])].measurement
StageSequence(op) == [i \in 1..Len(journal[op]) |-> journal[op][i].stage]
NextEvent(op, stage, evidence, measurement) ==
    [target |-> Target, operation |-> op,
     generation |-> Head(journal[op]).generation,
     stage |-> stage, evidence |-> evidence, measurement |-> measurement]
AppendEvent(op, stage, evidence, measurement) ==
    [journal EXCEPT ![op] = Append(@, NextEvent(op, stage, evidence, measurement))]

Init ==
    /\ journal = [op \in Operations |-> <<>>]
    /\ allocated = 0
    /\ pins = {}
    /\ owner = NoOwner
    /\ phase = [w \in Watchers |-> "idle"]
    /\ current = [w \in Watchers |-> 0]
    /\ attempts = [op \in Operations |-> 0]
    /\ provider = [op \in Operations |-> "idle"]
    /\ ack = FALSE
    /\ observed = "none"
    /\ foreignSeen = FALSE
    /\ cooldownExpired = TRUE
    /\ generation = 0
    /\ fallbackCalls = 0

\* A clock expiry or any change in effective suppression inputs is allowed
\* anywhere. Neither alters a durable unresolved operation in the safe model.
Expire ==
    /\ ~cooldownExpired
    /\ cooldownExpired' = TRUE
    /\ UNCHANGED <<journal, allocated, pins, owner, phase, current,
                   attempts, provider, ack, observed, foreignSeen,
                   generation, fallbackCalls>>

ChangeGeneration ==
    /\ generation = 0
    /\ generation' = 1
    /\ UNCHANGED <<journal, allocated, pins, owner, phase, current,
                   attempts, provider, ack, observed, foreignSeen,
                   cooldownExpired, fallbackCalls>>

Acquire(w) ==
    /\ owner = NoOwner
    /\ phase[w] = "idle"
    /\ owner' = w
    /\ phase' = [phase EXCEPT ![w] = "owned"]
    /\ current' = [current EXCEPT ![w] = 0]
    /\ ack' = FALSE
    /\ observed' = "none"
    /\ UNCHANGED <<journal, allocated, pins, attempts, provider, foreignSeen,
                   cooldownExpired, generation, fallbackCalls>>

Relinquish(w) ==
    /\ owner' = NoOwner
    /\ phase' = [phase EXCEPT ![w] = "idle"]
    /\ current' = [current EXCEPT ![w] = 0]
    /\ ack' = FALSE
    /\ observed' = "none"

FreshAllowed ==
    /\ allocated < MaxOperations
    /\ \/ allocated = 0
       \* A failed Prepared can be superseded by a fresh operation. Its old
       \* immutable record and recovery pin remain; it was never sent.
       \/ LastStage(allocated) = "prepared"
       \/ /\ LastStage(allocated) \in Resolved
          /\ cooldownExpired
       \/ /\ AllowExpiredReplay
          /\ LastStage(allocated) \in Unresolved
          /\ cooldownExpired

\* Recovery data is retained before publishing intent. A crash here can leave
\* an unused pin; the model never collects pins. Vault GC has a separate model.
RetainPin(w) ==
    /\ owner = w
    /\ phase[w] = "owned"
    /\ FreshAllowed
    /\ pins' = IF OmitRecoveryPin THEN pins ELSE pins \cup {allocated + 1}
    /\ phase' = [phase EXCEPT ![w] = "pinned"]
    /\ current' = [current EXCEPT ![w] = allocated + 1]
    /\ UNCHANGED <<journal, allocated, owner, attempts, provider, ack,
                   observed, foreignSeen, cooldownExpired, generation, fallbackCalls>>

PersistPrepared(w) ==
    /\ owner = w
    /\ phase[w] = "pinned"
    /\ current[w] = allocated + 1
    /\ journal' = [journal EXCEPT ![current[w]] =
        <<[target |-> Target, operation |-> current[w], generation |-> generation,
           stage |-> "prepared", evidence |-> "none", measurement |-> "none"]>>]
    /\ allocated' = allocated + 1
    /\ phase' = [phase EXCEPT ![w] = "prepared"]
    /\ UNCHANGED <<pins, owner, current, attempts, provider, ack, observed,
                   foreignSeen, cooldownExpired, generation, fallbackCalls>>

\* Pre-dispatch failure, including a failed durable write, permits no call.
\* The no-effect failure abstraction excludes torn/corrupt writes, not just
\* successful fsync ordering; the runtime must reject damaged journals.
Abandon(w) ==
    /\ owner = w
    /\ phase[w] \in {"owned", "pinned", "prepared"}
    /\ Relinquish(w)
    /\ UNCHANGED <<journal, allocated, pins, attempts, provider, foreignSeen,
                   cooldownExpired, generation, fallbackCalls>>

RejectBeforeDispatch(w) ==
    /\ owner = w
    /\ phase[w] = "prepared"
    /\ journal' = AppendEvent(current[w], "rejected", "none", "none")
    /\ cooldownExpired' = FALSE
    /\ Relinquish(w)
    /\ UNCHANGED <<allocated, pins, attempts, provider, foreignSeen,
                   generation, fallbackCalls>>

PersistDispatched(w) ==
    /\ owner = w
    /\ phase[w] = "prepared"
    /\ LastStage(current[w]) = "prepared"
    /\ journal' = AppendEvent(current[w], "dispatched", "none", "none")
    /\ phase' = [phase EXCEPT ![w] = "checkpointed"]
    /\ cooldownExpired' = FALSE
    /\ UNCHANGED <<allocated, pins, owner, current, attempts, provider, ack,
                   observed, foreignSeen, generation, fallbackCalls>>

\* Crucial separate step: a crash may occur after PersistDispatched but before
\* Send. The journal cannot distinguish that history from a lost outcome.
Send(w) ==
    /\ owner = w
    /\ phase[w] = "checkpointed"
    /\ LastStage(current[w]) = "dispatched"
    /\ attempts[current[w]] = 0
    /\ attempts' = [attempts EXCEPT ![current[w]] = @ + 1]
    /\ provider' = [provider EXCEPT ![current[w]] = "inflight"]
    /\ phase' = [phase EXCEPT ![w] = "sent"]
    /\ UNCHANGED <<journal, allocated, pins, owner, current, ack, observed,
                   foreignSeen, cooldownExpired, generation, fallbackCalls>>

\* The provider may finish after the watcher crashes or times out. No fairness
\* is imposed: it may also never finish, or its evidence may never arrive.
ProviderFinish(op, outcome) ==
    /\ provider[op] = "inflight"
    /\ provider' = [provider EXCEPT ![op] = outcome]
    /\ UNCHANGED <<journal, allocated, pins, owner, phase, current, attempts,
                   ack, observed, foreignSeen, cooldownExpired, generation, fallbackCalls>>

Ack(w) ==
    /\ owner = w
    /\ phase[w] = "sent"
    /\ ack' = TRUE
    /\ UNCHANGED <<journal, allocated, pins, owner, phase, current, attempts,
                   provider, observed, foreignSeen, cooldownExpired, generation, fallbackCalls>>

\* The abstract operation/turn token stands for an actually correlated owned
\* connection + provider identity, not an invented provider operation ID.
\* Foreign session, operation and turn notifications (including failures) do
\* not affect the terminal observation. Duplicates are harmless stutters.
Terminal(w, target, op, turn, outcome) ==
    /\ owner = w
    /\ phase[w] = "sent"
    /\ IF target = Target /\ op = current[w] /\ turn = current[w]
       THEN /\ provider[current[w]] = outcome
            /\ observed' = outcome
            /\ foreignSeen' = foreignSeen
       ELSE /\ observed' = observed
            /\ foreignSeen' = TRUE
    /\ UNCHANGED <<journal, allocated, pins, owner, phase, current, attempts,
                   provider, ack, cooldownExpired, generation, fallbackCalls>>

\* A matching terminal is necessary, but does not assert a token reduction.
\* The exact-target post-observation is abstracted as reduced or unchanged.
\* Byte selection, waiting for delayed publication, and measurement arithmetic
\* are production/test obligations, not proved by this nondeterministic oracle.
ObserveTerminal(w, measurement) ==
    /\ owner = w
    /\ phase[w] = "sent"
    /\ ack
    /\ observed = "completed"
    /\ journal' = AppendEvent(current[w],
           IF measurement = "reduced" THEN "observed_applied" ELSE "observed_noop",
           observed, measurement)
    /\ cooldownExpired' = FALSE
    /\ Relinquish(w)
    /\ UNCHANGED <<allocated, pins, attempts, provider, foreignSeen,
                   generation, fallbackCalls>>

\* The terminal evidence can be durable even when post-state observation is
\* unreadable or inconclusive. This records uncertainty rather than invented
\* applied/no-op accounting. It is distinct from a lost terminal entirely.
UnobservedPostState(w) ==
    /\ owner = w
    /\ phase[w] = "sent"
    /\ ack
    /\ observed = "completed"
    /\ journal' = AppendEvent(current[w], "unknown", observed, "unavailable")
    /\ cooldownExpired' = FALSE
    /\ Relinquish(w)
    /\ UNCHANGED <<allocated, pins, attempts, provider, foreignSeen,
                   generation, fallbackCalls>>

\* An explicitly requested reconciliation makes NO provider call and accepts
\* NO caller-supplied outcome. It requires the already-retained terminal's
\* exact provider identity contract. Session-only/private-connection evidence
\* may justify the original adapter observation under its stated assumption,
\* but does not admit this reconciliation route.
ExplicitReconcile(w) ==
    /\ owner = w
    /\ phase[w] = "owned"
    /\ allocated > 0
    /\ LastStage(allocated) = "unknown"
    /\ \/ /\ ExactCorrelation
          /\ LastEvidence(allocated) = "completed"
          /\ provider[allocated] = "completed"
       \/ AllowFabricatedReconciliation
    /\ journal' = AppendEvent(allocated, "reconciled", LastEvidence(allocated),
                              LastMeasurement(allocated))
    /\ cooldownExpired' = FALSE
    /\ Relinquish(w)
    /\ UNCHANGED <<allocated, pins, attempts, provider, foreignSeen,
                   generation, fallbackCalls>>

Uncertain(w) ==
    /\ owner = w
    /\ phase[w] \in {"checkpointed", "sent"}
    /\ journal' = AppendEvent(current[w], "unknown", "none", "none")
    /\ cooldownExpired' = FALSE
    /\ Relinquish(w)
    /\ UNCHANGED <<allocated, pins, attempts, provider, foreignSeen,
                   generation, fallbackCalls>>

Crash(w) ==
    /\ owner = w
    /\ phase[w] \in CustodiedPhases
    /\ owner' = NoOwner
    /\ phase' = [phase EXCEPT ![w] = "crashed"]
    /\ ack' = FALSE
    /\ observed' = "none"
    /\ UNCHANGED <<journal, allocated, pins, current, attempts, provider,
                   foreignSeen, cooldownExpired, generation, fallbackCalls>>

Restart(w) ==
    /\ phase[w] = "crashed"
    /\ phase' = [phase EXCEPT ![w] = "idle"]
    /\ current' = [current EXCEPT ![w] = 0]
    /\ UNCHANGED <<journal, allocated, pins, owner, attempts, provider, ack,
                   observed, foreignSeen, cooldownExpired, generation, fallbackCalls>>

\* Semantic mutants are disabled in safe.cfg and independently checked for
\* specific invariant violations. They are not error/timeout-as-success tests.
FalseSuccess(w) ==
    /\ AllowAckSuccess
    /\ owner = w
    /\ phase[w] = "sent"
    /\ ack
    /\ observed = "none"
    /\ journal' = AppendEvent(current[w], "observed_applied", "none", "reduced")
    /\ cooldownExpired' = FALSE
    /\ Relinquish(w)
    /\ UNCHANGED <<allocated, pins, attempts, provider, foreignSeen,
                   generation, fallbackCalls>>

Fallback ==
    /\ AllowFallback
    /\ allocated > 0
    /\ LastStage(allocated) \in Unresolved
    /\ fallbackCalls = 0
    /\ fallbackCalls' = 1
    /\ UNCHANGED <<journal, allocated, pins, owner, phase, current, attempts,
                   provider, ack, observed, foreignSeen, cooldownExpired, generation>>

Next ==
    \/ Expire
    \/ ChangeGeneration
    \/ Fallback
    \/ \E op \in Operations, outcome \in Outcomes : ProviderFinish(op, outcome)
    \/ \E w \in Watchers :
        \/ Acquire(w)
        \/ RetainPin(w)
        \/ PersistPrepared(w)
        \/ Abandon(w)
        \/ RejectBeforeDispatch(w)
        \/ PersistDispatched(w)
        \/ Send(w)
        \/ Ack(w)
        \/ \E outcome \in Outcomes :
            \/ Terminal(w, Target, current[w], current[w], outcome)
            \/ Terminal(w, "foreign-target", current[w], current[w], outcome)
            \/ Terminal(w, Target, 0, current[w], outcome)
            \/ Terminal(w, Target, current[w], 0, outcome)
        \/ \E measurement \in {"reduced", "unchanged"} : ObserveTerminal(w, measurement)
        \/ UnobservedPostState(w)
        \/ ExplicitReconcile(w)
        \/ Uncertain(w)
        \/ Crash(w)
        \/ Restart(w)
        \/ FalseSuccess(w)

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ journal \in [Operations -> Seq(Event)]
    /\ allocated \in 0..MaxOperations
    /\ pins \subseteq Operations
    /\ owner \in Watchers \cup {NoOwner}
    /\ phase \in [Watchers -> Phases]
    /\ current \in [Watchers -> 0..MaxOperations]
    /\ attempts \in [Operations -> 0..2]
    /\ provider \in [Operations -> ({"idle", "inflight"} \cup Outcomes)]
    /\ ack \in BOOLEAN /\ observed \in Evidence /\ foreignSeen \in BOOLEAN
    /\ cooldownExpired \in BOOLEAN /\ generation \in 0..1
    /\ fallbackCalls \in 0..1

SingleCustody ==
    /\ \A w \in Watchers : (phase[w] \in CustodiedPhases) \equiv (owner = w)
    /\ \A w \in Watchers :
        phase[w] \in {"pinned", "prepared", "checkpointed", "sent"}
        => current[w] \in Operations

AllowedTraces == {<<>>, <<"prepared">>, <<"prepared", "rejected">>,
    <<"prepared", "dispatched">>,
    <<"prepared", "dispatched", "observed_applied">>,
    <<"prepared", "dispatched", "observed_noop">>,
    <<"prepared", "dispatched", "unknown">>,
    <<"prepared", "dispatched", "unknown", "reconciled">>}

JournalValid ==
    /\ \A op \in Operations :
        /\ StageSequence(op) \in AllowedTraces
        /\ (op <= allocated) \equiv (Len(journal[op]) > 0)
        /\ \A i \in 1..Len(journal[op]) :
            /\ journal[op][i].target = Target
            /\ journal[op][i].operation = op
            /\ journal[op][i].generation = Head(journal[op]).generation

DispatchRecorded == \A op \in Operations : attempts[op] > 0 =>
    "dispatched" \in {journal[op][i].stage : i \in 1..Len(journal[op])}

RecoveryPinned == \A op \in Operations : Len(journal[op]) > 0 => op \in pins
AtMostOnce == \A op \in Operations : attempts[op] <= 1

NoUnresolvedReplay == \A prior, later \in Operations :
    (prior < later /\ attempts[later] > 0) => LastStage(prior) \notin Unresolved

AppliedNeedsTerminal == \A op \in Operations :
    LastStage(op) = "observed_applied" =>
        /\ LastEvidence(op) = "completed" /\ provider[op] = "completed"
        /\ LastMeasurement(op) = "reduced"

NoopNeedsTerminal == \A op \in Operations :
    LastStage(op) = "observed_noop" =>
        /\ LastEvidence(op) = "completed" /\ provider[op] = "completed"
        /\ LastMeasurement(op) = "unchanged"

ReconcileNeedsExactEvidence == \A op \in Operations :
    LastStage(op) = "reconciled" =>
        /\ ExactCorrelation
        /\ LastEvidence(op) = "completed"
        /\ provider[op] = "completed"
        /\ journal[op][3].stage = "unknown"
        /\ journal[op][3].evidence = LastEvidence(op)
        /\ journal[op][3].measurement = LastMeasurement(op)

AtMostOneInflight == Cardinality({op \in Operations : provider[op] = "inflight"}) <= 1
NoFallback == fallbackCalls = 0

\* Negated reachability assertions: an intended counterexample is the witness.
NoAppliedWitness == \A op \in Operations : LastStage(op) # "observed_applied"
NoNoopWitness == \A op \in Operations : LastStage(op) # "observed_noop"
NoUnknownWitness == \A op \in Operations : LastStage(op) # "unknown"
NoReconciledWitness == \A op \in Operations : LastStage(op) # "reconciled"
NoCrashWitness == ~\E w \in Watchers :
    /\ phase[w] = "crashed"
    /\ current[w] \in Operations
    /\ LastStage(current[w]) = "dispatched"
NoTerminalBeforeAckWitness == observed = "none" \/ ack

=============================================================================

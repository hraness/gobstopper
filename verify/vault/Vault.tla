----------------------------- MODULE Vault -----------------------------
EXTENDS Naturals, FiniteSets, TLC

\* A finite abstraction of stable vault-directory custody. Every object
\* publication below is atomic and durable, and objects are valid by construction.
\* This checks custody/order, not hashing, JSON, filesystem syscalls, or fsync.
CONSTANTS UseCustody, RespectPins

Writers == {"a", "b"}
Snapshots == {"old", "a", "b"}
Chunks == {"shared", "old", "a", "b"}
Parts(s) == {"shared", s}
WriterPhases == {"idle", "ready", "chunks", "manifest", "verified",
                 "indexed", "receipt", "done", "crashed"}
PrunePhases == {"idle", "marked", "indexed", "manifests", "done",
                "aborted", "crashed"}
ReaderPhases == {"idle", "reading", "done", "crashed"}

VARIABLES wp, pp, rp, objects, manifests, index, pins, readRef,
          markedIndex, keepIndex, deleteManifests, deleteChunks

vars == <<wp, pp, rp, objects, manifests, index, pins, readRef,
          markedIndex, keepIndex, deleteManifests, deleteChunks>>

Holding(w) == wp[w] \in {"ready", "chunks", "manifest", "verified",
                         "indexed", "receipt"}
Pruning == pp \in {"marked", "indexed", "manifests"}
CanShare == ~UseCustody \/ ~Pruning
CanPrune == ~UseCustody \/
            ((\A w \in Writers : ~Holding(w)) /\ rp # "reading")
DataValid(s) == s \in manifests /\ Parts(s) \subseteq objects
Reachable(ms) == UNION {Parts(s) : s \in ms}
LivePins == IF RespectPins THEN pins ELSE {}

Init ==
    /\ wp = [w \in Writers |-> "idle"]
    /\ pp = "idle"
    /\ rp = "idle"
    /\ objects = Parts("old")
    /\ manifests = {"old"}
    /\ index = {"old"}
    /\ pins = {}
    /\ readRef = "none"
    /\ markedIndex = {}
    /\ keepIndex = {}
    /\ deleteManifests = {}
    /\ deleteChunks = {}

Start(w) ==
    /\ wp[w] = "idle" /\ CanShare
    /\ wp' = [wp EXCEPT ![w] = "ready"]
    /\ UNCHANGED <<pp, rp, objects, manifests, index, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

PublishChunks(w) ==
    /\ wp[w] = "ready"
    /\ objects' = objects \cup Parts(w)
    /\ wp' = [wp EXCEPT ![w] = "chunks"]
    /\ UNCHANGED <<pp, rp, manifests, index, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

PublishManifest(w) ==
    /\ wp[w] = "chunks"
    /\ manifests' = manifests \cup {w}
    /\ wp' = [wp EXCEPT ![w] = "manifest"]
    /\ UNCHANGED <<pp, rp, objects, index, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

VerifySnapshot(w) ==
    /\ wp[w] = "manifest" /\ DataValid(w)
    /\ wp' = [wp EXCEPT ![w] = "verified"]
    /\ UNCHANGED <<pp, rp, objects, manifests, index, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

AppendIndex(w) ==
    /\ wp[w] = "verified"
    /\ index' = index \cup {w}
    /\ wp' = [wp EXCEPT ![w] = "indexed"]
    /\ UNCHANGED <<pp, rp, objects, manifests, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

\* The operation intent already pins its snapshot; completion is unnecessary.
\* Shared custody spans snapshot publication through intent/completed receipt.
PublishReceipt(w) ==
    /\ wp[w] = "indexed"
    /\ pins' = pins \cup {w}
    /\ wp' = [wp EXCEPT ![w] = "receipt"]
    /\ UNCHANGED <<pp, rp, objects, manifests, index, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

Finish(w) ==
    /\ wp[w] = "receipt"
    /\ wp' = [wp EXCEPT ![w] = "done"]
    /\ UNCHANGED <<pp, rp, objects, manifests, index, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

CrashWriter(w) ==
    /\ Holding(w)
    /\ wp' = [wp EXCEPT ![w] = "crashed"]
    /\ UNCHANGED <<pp, rp, objects, manifests, index, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

\* Retention selection is deliberately arbitrary. We check safety for every
\* subset, including keep=0; recency/stream grouping is tested in Rust.
Mark ==
    /\ pp = "idle" /\ CanPrune
    /\ \E kept \in SUBSET index :
        /\ keepIndex' = kept
        /\ deleteManifests' = (index \ kept) \ LivePins
        /\ deleteChunks' = objects \
             Reachable(manifests \ ((index \ kept) \ LivePins))
    /\ markedIndex' = index
    /\ pp' = "marked"
    /\ UNCHANGED <<wp, rp, objects, manifests, index, pins, readRef>>

ReplaceIndex ==
    /\ pp = "marked" /\ index = markedIndex
    /\ index' = keepIndex
    /\ pp' = "indexed"
    /\ UNCHANGED <<wp, rp, objects, manifests, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

AbortChangedIndex ==
    /\ pp = "marked" /\ index # markedIndex
    /\ pp' = "aborted"
    /\ UNCHANGED <<wp, rp, objects, manifests, index, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

DeleteManifests ==
    /\ pp = "indexed"
    /\ manifests' = manifests \ deleteManifests
    /\ pp' = "manifests"
    /\ UNCHANGED <<wp, rp, objects, index, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

DeleteChunks ==
    /\ pp = "manifests"
    /\ objects' = objects \ deleteChunks
    /\ pp' = "done"
    /\ UNCHANGED <<wp, rp, manifests, index, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

CrashPrune ==
    /\ Pruning
    /\ pp' = "crashed"
    /\ UNCHANGED <<wp, rp, objects, manifests, index, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

StartRead ==
    /\ rp = "idle" /\ CanShare
    /\ \E s \in Snapshots : DataValid(s) /\ readRef' = s
    /\ rp' = "reading"
    /\ UNCHANGED <<wp, pp, objects, manifests, index, pins,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

FinishRead ==
    /\ rp = "reading" /\ DataValid(readRef)
    /\ rp' = "done"
    /\ UNCHANGED <<wp, pp, objects, manifests, index, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

CrashRead ==
    /\ rp = "reading"
    /\ rp' = "crashed"
    /\ UNCHANGED <<wp, pp, objects, manifests, index, pins, readRef,
                    markedIndex, keepIndex, deleteManifests, deleteChunks>>

Next == (\E w \in Writers : Start(w) \/ PublishChunks(w) \/
          PublishManifest(w) \/ VerifySnapshot(w) \/ AppendIndex(w) \/
          PublishReceipt(w) \/ Finish(w) \/ CrashWriter(w)) \/
        Mark \/ ReplaceIndex \/ AbortChangedIndex \/ DeleteManifests \/
        DeleteChunks \/ CrashPrune \/ StartRead \/ FinishRead \/ CrashRead

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ wp \in [Writers -> WriterPhases]
    /\ pp \in PrunePhases /\ rp \in ReaderPhases
    /\ objects \subseteq Chunks /\ manifests \subseteq Snapshots
    /\ index \subseteq Snapshots /\ pins \subseteq Writers
    /\ readRef \in Snapshots \cup {"none"}
    /\ markedIndex \subseteq Snapshots /\ keepIndex \subseteq Snapshots
    /\ deleteManifests \subseteq Snapshots /\ deleteChunks \subseteq Chunks

IndexedData == \A s \in index : DataValid(s)
PinnedRecoveryData == \A s \in pins : DataValid(s)
ReaderData == rp = "reading" => DataValid(readRef)
ExclusiveCustody == UseCustody /\ Pruning =>
    (\A w \in Writers : ~Holding(w)) /\ rp # "reading"

\* Deliberately false witness invariants, each run in its own configuration.
NoSuccessfulComposition == ~(
    (\A w \in Writers : wp[w] = "done") /\ pp = "done" /\ rp = "done")
NoPendingRecovery == ~(\E w \in Writers : wp[w] = "crashed" /\ w \in pins)

=============================================================================

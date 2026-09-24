-------------------------- MODULE Publication --------------------------
EXTENDS Naturals, FiniteSets, TLC

\* One publisher, one reader, at most two prune cycles and one restart.
\* Objects/receipts are atomic durable abstract actions. Output visibility
\* and directory-sync confirmation are distinct; process death is not power loss.
CONSTANTS RequireIntent, RequireSync, RejectDamage, StopOnUnlink
Snapshots == {"old", "source", "candidate"}
Parts(ref) == {"shared", ref}
Reachable(refs) == UNION {Parts(ref) : ref \in refs}
VARIABLE s
DataValid(ref) == ref \in s.manifests /\ Parts(ref) \subseteq s.chunks
Pruning == s.prune \in {"marked", "indexed", "manifests"}
CanShare == ~Pruning
CanPrune == ~s.held /\ s.reader # "reading"

Init == s = [phase |-> "idle", held |-> FALSE,
    chunks |-> Parts("old"), manifests |-> {"old"}, index |-> {"old"},
    pins |-> {}, receipt |-> "none", output |-> "none", synced |-> FALSE,
    damage |-> FALSE, restarted |-> FALSE, prune |-> "idle", cycles |-> 0,
    keep |-> {}, retire |-> {}, garbage |-> {}, reader |-> "idle", readRef |-> "none"]

Start == /\ s.phase = "idle" /\ CanShare
         /\ s' = [s EXCEPT !.phase = "ready", !.held = TRUE]
SourceChunks == /\ s.phase = "ready"
                /\ s' = [s EXCEPT !.chunks = @ \cup Parts("source"), !.phase = "sourceChunks"]
SourceManifest == /\ s.phase = "sourceChunks"
                  /\ s' = [s EXCEPT !.manifests = @ \cup {"source"}, !.phase = "sourceManifest"]
AppendIndex == /\ s.phase = "sourceManifest"
               /\ s' = [s EXCEPT !.index = @ \cup {"source"}, !.phase = "indexed"]
TornIndex == /\ s.phase = "sourceManifest"
             /\ s' = [s EXCEPT !.damage = TRUE, !.phase = "crashed", !.held = FALSE]
CandidateChunks == /\ s.phase = "indexed"
                   /\ s' = [s EXCEPT !.chunks = @ \cup Parts("candidate"), !.phase = "candidateChunks"]
CandidateManifest == /\ s.phase = "candidateChunks"
                     /\ s' = [s EXCEPT !.manifests = @ \cup {"candidate"}, !.phase = "candidateManifest"]
Prepare == /\ s.phase = "candidateManifest" /\ DataValid("source") /\ DataValid("candidate")
           /\ s' = [s EXCEPT !.pins = {"source", "candidate"}, !.receipt = "prepared", !.phase = "prepared"]
TornReceipt == /\ s.phase = "candidateManifest"
               /\ s' = [s EXCEPT !.damage = TRUE, !.phase = "crashed", !.held = FALSE]
Publish == /\ (s.phase = "prepared" \/ (~RequireIntent /\ s.phase = "candidateManifest"))
           /\ s.output = "none" /\ DataValid("candidate")
           /\ s' = [s EXCEPT !.output = "candidate", !.phase = "published"]
Conflict == /\ s.phase = "prepared" /\ s.output = "none"
            /\ s' = [s EXCEPT !.output = "foreign"]
RefuseConflict == /\ s.phase = "prepared" /\ s.output = "foreign"
                  /\ s' = [s EXCEPT !.phase = "repair", !.held = FALSE]
ReconcileVisible == /\ s.phase = "prepared" /\ s.output = "candidate"
                    /\ s' = [s EXCEPT !.phase = "published"]
SyncOutput == /\ s.phase = "published"
              /\ s' = [s EXCEPT !.synced = TRUE, !.phase = "synced"]
Complete == /\ (s.phase = "synced" \/ (~RequireSync /\ s.phase = "published"))
            /\ s.receipt = "prepared" /\ s.output = "candidate"
            /\ s' = [s EXCEPT !.receipt = "completed", !.phase = "completed"]
Finish == /\ s.phase = "completed"
          /\ s' = [s EXCEPT !.phase = "done", !.held = FALSE]
Crash == /\ s.held
         /\ s' = [s EXCEPT !.phase = "crashed", !.held = FALSE]
Restart == /\ s.phase = "crashed" /\ ~s.restarted /\ CanShare
           /\ IF s.receipt \in {"prepared", "completed"} /\ ~s.damage
                 THEN /\ DataValid("source") /\ DataValid("candidate")
                      /\ s' = [s EXCEPT !.phase = IF @ = "crashed" /\ s.receipt = "prepared" THEN "prepared" ELSE "completed",
                                        !.held = TRUE, !.restarted = TRUE]
                 ELSE s' = [s EXCEPT !.phase = "repair", !.restarted = TRUE]

Mark == /\ ~Pruning /\ s.cycles < 2 /\ CanPrune /\ (~RejectDamage \/ ~s.damage)
        /\ \E keep \in SUBSET s.index :
            s' = [s EXCEPT !.keep = keep, !.retire = (s.index \ keep) \ s.pins,
                !.garbage = s.chunks \ Reachable(s.manifests \ ((s.index \ keep) \ s.pins)),
                !.prune = "marked", !.cycles = @ + 1]
ReplaceIndex == /\ s.prune = "marked"
                /\ s' = [s EXCEPT !.index = s.keep, !.prune = "indexed"]
RemoveManifests == /\ s.prune = "indexed"
                   /\ s' = [s EXCEPT !.manifests = @ \ s.retire, !.prune = "manifests"]
FailedUnlink == /\ s.prune = "indexed" /\ s.retire # {}
                /\ s' = [s EXCEPT !.prune = IF StopOnUnlink THEN "failed" ELSE "manifests"]
RemoveChunks == /\ s.prune = "manifests"
                /\ s' = [s EXCEPT !.chunks = @ \ s.garbage, !.prune = "done"]
CrashPrune == /\ Pruning
              /\ s' = [s EXCEPT !.prune = "crashed"]
StartRead == /\ s.reader = "idle" /\ CanShare
             /\ \E ref \in Snapshots : DataValid(ref) /\ s' = [s EXCEPT !.reader = "reading", !.readRef = ref]
FinishRead == /\ s.reader = "reading" /\ DataValid(s.readRef)
              /\ s' = [s EXCEPT !.reader = "done"]
CrashRead == /\ s.reader = "reading"
             /\ s' = [s EXCEPT !.reader = "crashed"]

Next == Start \/ SourceChunks \/ SourceManifest \/ AppendIndex \/ TornIndex \/
    CandidateChunks \/ CandidateManifest \/ Prepare \/ TornReceipt \/ Publish \/ Conflict \/
    RefuseConflict \/ ReconcileVisible \/ SyncOutput \/ Complete \/ Finish \/ Crash \/ Restart \/
    Mark \/ ReplaceIndex \/ RemoveManifests \/ FailedUnlink \/ RemoveChunks \/ CrashPrune \/
    StartRead \/ FinishRead \/ CrashRead
Spec == Init /\ [][Next]_s

TypeOK == /\ s.phase \in {"idle", "ready", "sourceChunks", "sourceManifest", "indexed", "candidateChunks",
               "candidateManifest", "prepared", "published", "synced", "completed", "done", "crashed", "repair"}
          /\ s.chunks \subseteq {"shared", "old", "source", "candidate"}
          /\ s.manifests \subseteq Snapshots /\ s.index \subseteq Snapshots /\ s.pins \subseteq Snapshots
          /\ s.receipt \in {"none", "prepared", "completed"}
          /\ s.output \in {"none", "candidate", "foreign"} /\ s.cycles \in 0..2
          /\ s.held \in BOOLEAN /\ s.synced \in BOOLEAN /\ s.damage \in BOOLEAN /\ s.restarted \in BOOLEAN
          /\ s.prune \in {"idle", "marked", "indexed", "manifests", "done", "failed", "crashed"}
          /\ s.reader \in {"idle", "reading", "done", "crashed"}
          /\ s.readRef \in Snapshots \cup {"none"}
          /\ s.keep \subseteq Snapshots /\ s.retire \subseteq Snapshots
          /\ s.garbage \subseteq {"shared", "old", "source", "candidate"}
IndexedData == \A ref \in s.index : DataValid(ref)
PinnedData == \A ref \in s.pins : DataValid(ref)
AllManifestsValid == \A ref \in s.manifests : DataValid(ref)
ReaderData == s.reader = "reading" => DataValid(s.readRef)
ExclusiveCustody == Pruning => ~s.held /\ s.reader # "reading"
OutputHasIntent == s.output = "candidate" => s.receipt \in {"prepared", "completed"}
CompletedDurable == s.receipt = "completed" => s.output = "candidate" /\ s.synced
DamageBlocksCollection == s.damage => ~Pruning
NoRecoveryWitness == ~(s.restarted /\ s.phase = "completed")
=============================================================================

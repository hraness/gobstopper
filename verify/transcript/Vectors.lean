import Transcript
import Lean.Data.Json.FromToJson

open Lean Transcript

namespace Oracle

/- The provider codec fixtures use three paired tool calls, a protected final
   output and two non-elidable user records. These symbols are independent of
   provider JSON, physical line offsets and Rust output decoding. -/
def base : List Record := [
  ⟨0, true, false, true, 1, .quiet⟩,
  ⟨1, true, false, false, 2, .call 1⟩,
  ⟨2, true, true, false, 3, .result 1⟩,
  ⟨3, true, false, false, 4, .call 2⟩,
  ⟨4, true, true, false, 5, .result 2⟩,
  ⟨5, true, false, false, 6, .call 3⟩,
  ⟨6, true, true, true, 7, .result 3⟩,
  ⟨7, true, false, true, 8, .quiet⟩]

def eventJson : Event → Json
  | .quiet => Json.mkObj [("kind", toJson "quiet")]
  | .call key => Json.mkObj [("kind", toJson "call"), ("id", toJson key)]
  | .result key => Json.mkObj [("kind", toJson "result"), ("id", toJson key)]

def recordJson (r : Record) : Json := Json.mkObj [
  ("key", toJson r.key), ("live", toJson r.live),
  ("eligible", toJson r.eligible), ("retained", toJson r.retained),
  ("content", toJson r.content), ("event", eventJson r.event)]

/- A short emitted mask has no >256-byte elision authority on reprojection.
   The retained newest output is never selected by an admitted request, so the
   one-output policy suffix stays fixed in this correspondence fixture family. -/
def reproject (records : List Record) : List Record :=
  records.map (fun r => { r with eligible := r.eligible && r.content != 0 })

def step (records : List Record) (selected : List Nat) (digest : Bool) : List Record × Json :=
  let accepted := admitted records selected
  let masked := execute records selected
  let after := if accepted && digest then appendDigest 8 99 masked else masked
  (reproject after, Json.mkObj [
    ("selected", toJson selected), ("digest", toJson digest), ("admitted", toJson accepted),
    ("after", toJson (after.map recordJson)),
    ("effective_keys", toJson (keys (effective after))),
    ("well_linked", toJson (wellLinked after))])

def caseJson (name : String) (requests : List (List Nat × Bool)) : Json :=
  let (_, steps) := requests.foldl (fun (records, out) (selected, digest) =>
    let (next, row) := step records selected digest
    (next, out ++ [row])) (base, ([] : List Json))
  Json.mkObj [("name", toJson name), ("before", toJson (base.map recordJson)),
              ("keep_recent", toJson (1 : Nat)), ("steps", toJson steps)]

def cases : List Json := [
  caseJson "empty" [([], false)],
  caseJson "first" [([2], false)],
  caseJson "second" [([4], false)],
  caseJson "both" [([2, 4], false)],
  caseJson "reverse_selection" [([4, 2], false)],
  caseJson "protected" [([6], false)],
  caseJson "user" [([0], false)],
  caseJson "call" [([1], false)],
  caseJson "newest_user" [([7], false)],
  caseJson "unknown" [([99], false)],
  caseJson "duplicate" [([2, 2], false)],
  caseJson "mixed_unknown" [([2, 99], false)],
  caseJson "mixed_protected" [([2, 6], false)],
  caseJson "digest_only" [([], true)],
  caseJson "mask_and_digest" [([2], true)],
  caseJson "both_and_digest" [([2, 4], true)],
  caseJson "rejected_with_digest" [([6], true)],
  caseJson "compose" [([2], false), ([4], false)],
  caseJson "repeat" [([2], false), ([2], false)],
  caseJson "repeat_both" [([2, 4], false), ([2, 4], false)]]

def document : Json := Json.mkObj [
  ("schema", toJson (1 : Nat)), ("oracle", toJson "lean-structural-v1"),
  ("cases", toJson cases)]

end Oracle

def main : IO Unit := IO.println Oracle.document.compress

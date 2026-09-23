import Init.Data.List.Lemmas

/- Structural, lossy transcript algebra. Content is an uninterpreted symbol;
   zero denotes a short mask. Supported shape, live-branch authority and policy
   protection are supplied preconditions, not inferred from provider bytes. -/
namespace Transcript

inductive Event where
  | quiet
  | call (id : Nat)
  | result (id : Nat)
  deriving BEq, DecidableEq, Repr

structure Record where
  key : Nat
  live : Bool
  eligible : Bool
  retained : Bool
  content : Nat
  event : Event := .quiet
  deriving BEq, DecidableEq, Repr

def allowed (r : Record) : Bool := r.live && r.eligible && !r.retained

def maskOne (selected : List Nat) (r : Record) : Record :=
  if selected.contains r.key && allowed r then { r with content := 0 } else r

def mask (selected : List Nat) (records : List Record) : List Record :=
  records.map (maskOne selected)

def keys (records : List Record) : List Nat := records.map Record.key
def effective (records : List Record) : List Record := records.filter Record.live
def protectedView (records : List Record) : List Record := records.filter Record.retained
def links (records : List Record) : List Event := records.map Record.event

@[simp] theorem maskOne_key (selected : List Nat) (r : Record) :
    (maskOne selected r).key = r.key := by
  unfold maskOne; split <;> rfl

@[simp] theorem maskOne_live (selected : List Nat) (r : Record) :
    (maskOne selected r).live = r.live := by
  unfold maskOne; split <;> rfl

@[simp] theorem maskOne_protected (selected : List Nat) (r : Record) :
    (maskOne selected r).retained = r.retained := by
  unfold maskOne; split <;> rfl

@[simp] theorem maskOne_event (selected : List Nat) (r : Record) :
    (maskOne selected r).event = r.event := by
  unfold maskOne; split <;> rfl

theorem protected_identity (selected : List Nat) (r : Record) (h : r.retained = true) :
    maskOne selected r = r := by simp [maskOne, allowed, h]

theorem inactive_identity (selected : List Nat) (r : Record) (h : r.live = false) :
    maskOne selected r = r := by simp [maskOne, allowed, h]

theorem selected_is_masked (selected : List Nat) (r : Record)
    (chosen : selected.contains r.key = true) (safe : allowed r = true) :
    (maskOne selected r).content = 0 := by
  unfold maskOne
  rw [chosen, safe]
  rfl

theorem unselected_identity (selected : List Nat) (r : Record)
    (h : selected.contains r.key = false) : maskOne selected r = r := by
  unfold maskOne
  rw [h]
  rfl

theorem maskOne_empty (r : Record) : maskOne [] r = r := by simp [maskOne]

theorem mask_empty (records : List Record) : mask [] records = records := by
  induction records with
  | nil => rfl
  | cons r rs ih => simp_all [mask, maskOne]

theorem length_preserved (selected : List Nat) (records : List Record) :
    (mask selected records).length = records.length := by simp [mask]

theorem order_and_identity_preserved (selected : List Nat) (records : List Record) :
    keys (mask selected records) = keys records := by simp [keys, mask]

theorem links_preserved (selected : List Nat) (records : List Record) :
    links (mask selected records) = links records := by simp [links, mask]

theorem effective_commutes (selected : List Nat) (records : List Record) :
    effective (mask selected records) = mask selected (effective records) := by
  simp [effective, mask, List.filter_map, Function.comp_def]

theorem protected_projection_preserved (selected : List Nat) (records : List Record) :
    protectedView (mask selected records) = protectedView records := by
  induction records with
  | nil => rfl
  | cons r rs ih =>
    cases h : r.retained <;>
      simp_all [protectedView, mask, protected_identity]

theorem maskOne_compose (first second : List Nat) (r : Record) :
    maskOne second (maskOne first r) = maskOne (first ++ second) r := by
  by_cases h₁ : r.key ∈ first <;> by_cases h₂ : r.key ∈ second <;>
    cases h₃ : r.live <;> cases h₄ : r.eligible <;> cases h₅ : r.retained <;>
    simp [maskOne, allowed, h₁, h₂, h₃, h₄, h₅]

theorem composition (first second : List Nat) (records : List Record) :
    mask second (mask first records) = mask (first ++ second) records := by
  simp [mask, List.map_map, Function.comp_def, maskOne_compose]

theorem idempotence (selected : List Nat) (records : List Record) :
    mask selected (mask selected records) = mask selected records := by
  rw [composition]
  simp [mask, maskOne]

/- Admission is explicit. The algebra does not silently interpret unknown or
   duplicated selections as a valid public request. Numeric capacities, stubs
   and provider dialect admission remain Rust/Kani obligations. -/
def unique (ids : List Nat) : Bool := ids.eraseDups.length == ids.length

def admitted (records : List Record) (selected : List Nat) : Bool :=
  unique (keys records) && unique selected &&
    selected.all (fun key => records.any (fun r => r.key == key && allowed r))

theorem admitted_targets_known_safe (records : List Record) (selected : List Nat)
    (accepted : admitted records selected = true) (key : Nat) (chosen : key ∈ selected) :
    ∃ r ∈ records, r.key = key ∧ allowed r = true := by
  simp only [admitted, Bool.and_eq_true, List.all_eq_true, List.any_eq_true,
    beq_iff_eq] at accepted
  exact accepted.2 key chosen

def execute (records : List Record) (selected : List Nat) : List Record :=
  if admitted records selected then mask selected records else records

theorem rejection_identity (records : List Record) (selected : List Nat)
    (h : admitted records selected = false) : execute records selected = records := by
  simp [execute, h]

theorem admitted_execution (records : List Record) (selected : List Nat)
    (h : admitted records selected = true) : execute records selected = mask selected records := by
  simp [execute, h]

/- A tool result must follow its unique call and occur at most once. A pending
   call is permitted. Masking cannot invent, reorder or remove either endpoint. -/
def wellLinkedFrom (calls results : List Nat) : List Event → Bool
  | [] => true
  | .quiet :: rest => wellLinkedFrom calls results rest
  | .call key :: rest => !calls.contains key && wellLinkedFrom (key :: calls) results rest
  | .result key :: rest => calls.contains key && !results.contains key &&
      wellLinkedFrom calls (key :: results) rest

def wellLinked (records : List Record) : Bool := wellLinkedFrom [] [] (links records)

theorem tool_wellformedness_preserved (selected : List Nat) (records : List Record) :
    wellLinked (mask selected records) = wellLinked records := by
  simp [wellLinked, links_preserved]

def appendDigest (fresh payload : Nat) (records : List Record) : List Record :=
  records ++ [{ key := fresh, live := true, eligible := false, retained := true,
                content := payload, event := .quiet }]

theorem digest_keeps_prior_order (fresh payload : Nat) (records : List Record) :
    keys (appendDigest fresh payload records) = keys records ++ [fresh] := by
  simp [keys, appendDigest]

theorem digest_keeps_prior_records (fresh payload : Nat) (records : List Record) :
    (appendDigest fresh payload records).take records.length = records := by
  simp [appendDigest]

theorem digest_fresh_identity (fresh payload : Nat) (records : List Record)
    (old : (keys records).Nodup) (unused : fresh ∉ keys records) :
    (keys (appendDigest fresh payload records)).Nodup := by
  rw [digest_keeps_prior_order]
  apply List.nodup_append.mpr
  refine ⟨old, by simp, ?_⟩
  simp only [List.mem_singleton]
  intro a ha b hb
  subst b
  intro equal
  exact unused (equal ▸ ha)

theorem quiet_append (events : List Event) (calls results : List Nat) :
    wellLinkedFrom calls results (events ++ [.quiet]) = wellLinkedFrom calls results events := by
  induction events generalizing calls results with
  | nil => rfl
  | cons event rest ih => cases event <;> simp [wellLinkedFrom, ih]

theorem digest_preserves_tool_links (fresh payload : Nat) (records : List Record) :
    wellLinked (appendDigest fresh payload records) = wellLinked records := by
  simp [wellLinked, links, appendDigest, quiet_append]

end Transcript

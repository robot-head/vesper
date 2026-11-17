import CodegenComposition

/-!
An executable, conservative range validator for finite data-row streams from
`VerifiedCodegen`. Acceptance discharges `CodegenComposition.Admissible` for
every concrete store satisfying the input facts; it does not run that store to
check the premise. Facts are overwritten on writes, including aliasing writes.
Successful remainder results retain their proved integer ranges. Control flow,
calls, source lowering and native instruction encodings remain outside this validator.
-/
namespace Vesper.CodegenValidation
open Vesper.VerifiedCodegen
open Vesper.CodegenComposition

abbrev Facts := Loc → Option Kind

def Holds (facts : Facts) (store : Store) : Prop :=
  ∀ location kind, facts location = some kind → InRange kind (store location)

def put (facts : Facts) (location : Loc) (kind : Option Kind) : Facts :=
  fun other => if other = location then kind else facts other

def supports (facts : Facts) (location : Loc) (kind : Kind) : Bool :=
  match facts location with
  | none => false
  | some known => decide (kind.lo ≤ known.lo ∧ known.hi ≤ kind.hi)

def constantKind (value : Int) : Option Kind :=
  if fits .bool value then some .bool else
  if fits .u8 value then some .u8 else
  if fits .u32 value then some .u32 else
  if fits .u64 value then some .u64 else
  if fits .i64 value then some .i64 else none

def checkRow (facts : Facts) : Lir → Bool
  | .arith _ kind _ a b | .wrapping _ kind _ a b =>
      kind != .bool && supports facts (.slot a) kind && supports facts (.slot b) kind
  | .neg kind _ a => kind != .bool && supports facts (.slot a) kind
  | .not _ a => supports facts (.slot a) .bool
  | .compare _ kind _ a b => supports facts (.slot a) kind && supports facts (.slot b) kind
  | .branchZero .. | .jump _ | .label _ | .call _ | .enter _ | .ret | .trap .fault => false
  | _ => true

def forgetHeap (facts : Facts) : Facts
  | .heap _ => none
  | other => facts other

def transfer (facts : Facts) : Lir → Facts
  | .const d value => put facts (.slot d) (constantKind value)
  | .move d a => put facts (.slot d) (facts (.slot a))
  | .arith _ kind d _ _ | .wrapping _ kind d _ _ | .neg kind d _ => put facts (.slot d) (some kind)
  | .not d _ | .compare _ _ d _ _ | .fcompare _ d _ _ => put facts (.slot d) (some .bool)
  -- A binary64 bit pattern has no integer-kind fact.
  | .farith _ d _ _ | .fneg d _ => put facts (.slot d) none
  | .storeOut i a => put facts (.out i) (facts (.slot a))
  | .loadOut d i => put facts (.slot d) (facts (.out i))
  | .loadIn d i => put facts (.slot d) (facts (.inn i))
  | .storeIn i a => put facts (.inn i) (facts (.slot a))
  -- Heap words are addressed through run-time pointers: a load knows
  -- nothing, a store forgets every heap fact.
  | .loadPtr d _ _ => put facts (.slot d) none
  | .storePtr .. => forgetHeap facts
  | _ => facts

def validate : List Lir → Facts → Bool
  | [], _ => true
  | row :: rest, facts => checkRow facts row && validate rest (transfer facts row)

theorem supports_sound {facts : Facts} {store : Store} (holds : Holds facts store)
    {location : Loc} {kind : Kind} (accepted : supports facts location kind = true) :
    InRange kind (store location) := by
  cases known : facts location with
  | none => simp [supports, known] at accepted
  | some previous =>
    have range := holds location previous known
    simp [supports, known] at accepted
    unfold InRange at range ⊢
    omega

theorem constant_kind_sound {value : Int} {kind : Kind}
    (known : constantKind value = some kind) : InRange kind value := by
  unfold constantKind at known
  split at known <;> try (simp_all [fits, InRange]; done)
  split at known <;> try (simp_all [fits, InRange]; done)
  split at known <;> try (simp_all [fits, InRange]; done)
  split at known <;> try (simp_all [fits, InRange]; done)
  split at known <;> simp_all [fits, InRange]

theorem put_sound {facts : Facts} {store : Store} (holds : Holds facts store)
    (location : Loc) (kind : Option Kind) (value : Int)
    (range : ∀ k, kind = some k → InRange k value) :
    Holds (put facts location kind) (upd store location value) := by
  intro other k known
  by_cases same : other = location
  · subst other
    simp only [put, ↓reduceIte] at known
    simpa [upd] using range k known
  · simp only [put, ite_eq_right same] at known
    simpa [upd, same] using holds other k known

theorem copy_sound {facts : Facts} {store : Store} (holds : Holds facts store)
    (destination source : Loc) :
    Holds (put facts destination (facts source)) (upd store destination (store source)) :=
  put_sound holds destination (facts source) (store source) (holds source)

theorem check_row_sound {facts : Facts} {store : Store} (holds : Holds facts store)
    {row : Lir} (accepted : checkRow facts row = true) :
    DataRow row ∧ row ≠ .trap .fault ∧ Typed row store := by
  cases row <;> simp_all [checkRow, DataRow, Typed]
  case arith op k d a b =>
    exact ⟨supports_sound holds accepted.1.2, supports_sound holds accepted.2⟩
  case wrapping op k d a b =>
    exact ⟨supports_sound holds accepted.1.2, supports_sound holds accepted.2⟩
  case neg k d a => exact supports_sound holds accepted.2
  case not d a => exact supports_sound holds accepted
  case compare c k d a b => exact ⟨supports_sound holds accepted.1, supports_sound holds accepted.2⟩
  case trap failure => cases failure <;> simp_all

theorem remainder_result_range {kind : Kind} {a b : Int}
    (left : InRange kind a) (right : InRange kind b) (nonzero : b ≠ 0) :
    InRange kind (a.tmod b) := by
  have magnitude : (a.tmod b).natAbs < b.natAbs := by
    rw [Int.natAbs_tmod]
    exact Nat.mod_lt _ (by omega)
  cases kind <;> simp only [InRange, Kind.lo, Kind.hi, H] at left right ⊢
  all_goals try (have nonnegative := Int.tmod_nonneg b (by omega : 0 ≤ a); omega)
  all_goals omega

theorem checked_result_range {op : ArithOp} {kind : Kind} {a b value : Int}
    (left : InRange kind a) (right : InRange kind b)
    (result : checkedArith op kind a b = .ok value) : InRange kind value := by
  cases op with
  | rem =>
    simp only [checkedArith] at result
    split at result
    · cases result
    · rename_i nonzero
      cases result
      exact remainder_result_range left right nonzero
  | add | sub | mul | div =>
    simp only [checkedArith] at result
    split at result
    all_goals try (simp_all [fits, InRange]; done)
    all_goals split at result <;> simp_all [fits, InRange]

theorem wrapping_result_range (kind : Kind) (value : Int) (integer : kind ≠ .bool) :
    InRange kind (wrapInto kind value) := by
  cases kind <;> simp_all [InRange, Kind.lo, Kind.hi, wrapInto, Kind.modulus, toS, W, H]
  all_goals omega

theorem transfer_sound {facts : Facts} {store next : Store} (holds : Holds facts store)
    {row : Lir} (accepted : checkRow facts row = true) (result : lirStep row store = .ok next) :
    Holds (transfer facts row) next := by
  have typed := (check_row_sound holds accepted).2.2
  cases row with
  | const d value =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    exact put_sound holds _ _ _ (fun _ known => constant_kind_sound known)
  | move d a =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    exact copy_sound holds _ _
  | storeOut i a =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    exact copy_sound holds _ _
  | loadOut d i =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    exact copy_sound holds _ _
  | loadIn d i =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    exact copy_sound holds _ _
  | storeIn i a =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    exact copy_sound holds _ _
  | arith op k d a b =>
    cases step : checkedArith op k (store (.slot a)) (store (.slot b)) with
    | abort failure => simp [lirStep, step] at result
    | ok value =>
      simp only [lirStep, step, Res.bind_ok, Res.ok.injEq] at result
      subst next
      apply put_sound holds
      intro k' same
      cases same
      exact checked_result_range typed.2.1 typed.2.2 step
  | wrapping op k d a b =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    apply put_sound holds
    intro k' same
    cases same
    exact wrapping_result_range k _ typed.1
  | neg k d a =>
    simp only [lirStep] at result
    split at result
    · rename_i fitsResult
      cases result
      apply put_sound holds
      intro k' same
      cases same
      simpa [fits, InRange] using fitsResult
    · cases result
  | not d a =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    apply put_sound holds
    intro k' same
    cases same
    simp only [Typed, InRange, Kind.lo, Kind.hi] at typed ⊢
    omega
  | compare c k d a b =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    apply put_sound holds
    intro k' same
    cases same
    split <;> simp [InRange, Kind.lo, Kind.hi]
  | farith op d a b =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    exact put_sound holds _ _ _ (fun _ known => by cases known)
  | fneg d a =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    exact put_sound holds _ _ _ (fun _ known => by cases known)
  | fcompare c d a b =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    apply put_sound holds
    intro k' same
    cases same
    split <;> simp [InRange, Kind.lo, Kind.hi]
  | trap failure => cases result
  | loadPtr d p off =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    exact put_sound holds _ _ _ (fun _ known => by cases known)
  | storePtr p off src =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    intro location kind known
    cases location with
    | heap n => simp [transfer, forgetHeap] at known
    | slot n => simpa [upd] using holds _ kind known
    | out n => simpa [upd] using holds _ kind known
    | inn n => simpa [upd] using holds _ kind known
  | scope o d =>
    simp only [lirStep, Res.ok.injEq] at result
    subst next
    exact holds
  | branchZero _ _ | jump _ | label _ | call _ | enter _ | ret =>
    simp [checkRow] at accepted

/-- A finite Boolean check establishes all semantic operand obligations. -/
theorem validation_sound {rows : List Lir} {facts : Facts}
    (accepted : validate rows facts = true) {store : Store} (holds : Holds facts store) :
    Admissible rows store := by
  induction rows generalizing facts store with
  | nil => trivial
  | cons row rest ih =>
    obtain ⟨head, tail⟩ : checkRow facts row = true ∧ validate rest (transfer facts row) = true :=
      by simpa [validate] using accepted
    obtain ⟨data, safe, typed⟩ := check_row_sound holds head
    exact ⟨data, safe, typed, fun next result => ih tail (transfer_sound holds head result)⟩

theorem validated_program_simulates (rows : List Lir) (facts : Facts)
    (accepted : validate rows facts = true) (store : Store) (machine : Mach)
    (holds : Holds facts store) (related : Rel store machine) :
    Sim (execute rows store) (run (compile rows) machine) :=
  compiled_program_simulates rows store machine (validation_sound accepted holds) related

theorem validated_program_has_no_fault (rows : List Lir) (facts : Facts)
    (accepted : validate rows facts = true) (store : Store) (machine : Mach)
    (holds : Holds facts store) (related : Rel store machine) :
    run (compile rows) machine ≠ .abort .fault :=
  compiled_program_has_no_fault rows store machine (validation_sound accepted holds) related

def emptyFacts : Facts := fun _ => none

theorem empty_facts_hold (store : Store) : Holds emptyFacts store := by
  intro location kind impossible
  cases impossible

/-- Constants may establish every required operand fact, for any initial store. -/
theorem validated_closed_program (rows : List Lir) (accepted : validate rows emptyFacts = true)
    (store : Store) (machine : Mach) (related : Rel store machine) :
    Sim (execute rows store) (run (compile rows) machine) ∧
      run (compile rows) machine ≠ .abort .fault :=
  ⟨validated_program_simulates rows emptyFacts accepted store machine (empty_facts_hold store) related,
    validated_program_has_no_fault rows emptyFacts accepted store machine (empty_facts_hold store) related⟩

theorem addition_validated : validate additionExample emptyFacts = true := by decide

theorem overflow_validated :
    validate [.const 0 255, .const 1 1, .arith .add .u8 2 0 1] emptyFacts = true := by decide

/-- Both the arithmetic destination and the Boolean destination alias inputs. -/
theorem aliasing_writes_validated : validate
    [.const 0 255, .const 1 1, .wrapping .add .u8 0 0 1,
     .compare .eq .u8 0 0 1, .not 0 0] emptyFacts = true := by decide

/-- Frame areas are distinct locations, and a copied fact survives source overwrite. -/
theorem frame_copy_validated : validate
    [.const 0 1, .storeOut 0 0, .const 0 2, .loadOut 1 0,
     .storeIn 0 1, .loadIn 1 0, .not 1 1] emptyFacts = true := by decide

theorem stale_fact_rejected :
    validate [.const 0 1, .const 0 2, .not 1 0] emptyFacts = false := by decide

theorem unknown_operand_rejected : validate [.not 0 1] emptyFacts = false := by decide

theorem unrepresentable_boolean_rejected :
    validate [.const 0 W, .not 1 0] emptyFacts = false := by decide

/-- A remainder overwrites stale facts with its result kind. -/
theorem remainder_updates_destination (facts : Facts) (kind : Kind) (d a b : Nat) :
    transfer facts (.arith .rem kind d a b) (.slot d) = some kind := by
  simp [transfer, put]

theorem remainder_pipeline_validated :
    validate [.const 0 255, .const 1 2, .arith .rem .u8 0 0 1, .arith .add .u8 0 0 1]
      emptyFacts = true := by decide

theorem signed_min_remainder_validated :
    validate [.const 0 (-H), .const 1 (-1), .arith .rem .i64 0 0 1, .neg .i64 0 0]
      emptyFacts = true := by decide

theorem control_and_fault_rejected (facts : Facts) (label words : Nat) :
    checkRow facts (.call label) = false ∧ checkRow facts (.enter words) = false ∧
      checkRow facts (.trap .fault) = false := by
  simp [checkRow]

end Vesper.CodegenValidation

#print axioms Vesper.CodegenValidation.supports_sound
#print axioms Vesper.CodegenValidation.constant_kind_sound
#print axioms Vesper.CodegenValidation.put_sound
#print axioms Vesper.CodegenValidation.copy_sound
#print axioms Vesper.CodegenValidation.check_row_sound
#print axioms Vesper.CodegenValidation.remainder_result_range
#print axioms Vesper.CodegenValidation.checked_result_range
#print axioms Vesper.CodegenValidation.wrapping_result_range
#print axioms Vesper.CodegenValidation.transfer_sound
#print axioms Vesper.CodegenValidation.validation_sound
#print axioms Vesper.CodegenValidation.validated_program_simulates
#print axioms Vesper.CodegenValidation.validated_program_has_no_fault
#print axioms Vesper.CodegenValidation.empty_facts_hold
#print axioms Vesper.CodegenValidation.validated_closed_program
#print axioms Vesper.CodegenValidation.addition_validated
#print axioms Vesper.CodegenValidation.overflow_validated
#print axioms Vesper.CodegenValidation.aliasing_writes_validated
#print axioms Vesper.CodegenValidation.frame_copy_validated
#print axioms Vesper.CodegenValidation.stale_fact_rejected
#print axioms Vesper.CodegenValidation.unknown_operand_rejected
#print axioms Vesper.CodegenValidation.unrepresentable_boolean_rejected
#print axioms Vesper.CodegenValidation.remainder_updates_destination
#print axioms Vesper.CodegenValidation.remainder_pipeline_validated
#print axioms Vesper.CodegenValidation.signed_min_remainder_validated
#print axioms Vesper.CodegenValidation.control_and_fault_rejected

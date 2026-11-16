import VerifiedCodegen

/-!
Composition of the existing instruction-selection simulation for finite,
straight-line slot-IR programs. `Admissible` is a semantic verification
condition: each reachable row has in-range operands, and explicit hardware
faults/control rows are excluded. It is not a theorem about Rust lowering,
frame/call conventions, byte encodings, or the source checker.
-/
namespace Vesper.CodegenComposition
open Vesper.VerifiedCodegen

/-- Slot execution stops at the first abort. -/
def execute : List Lir → Store → Res Store
  | [], s => .ok s
  | row :: rest, s => (lirStep row s).bind (execute rest)

/-- Flatten the actual selection table, without introducing a new selector. -/
def compile (rows : List Lir) : List M := rows.flatMap select

/-- Preconditions for every reachable row, computed over source execution.
An abort does not require facts about its unreachable suffix. -/
def Admissible : List Lir → Store → Prop
  | [], _ => True
  | row :: rest, s => DataRow row ∧ row ≠ .trap .fault ∧ Typed row s ∧
      ∀ next, lirStep row s = .ok next → Admissible rest next

theorem run_append (first second : List M) (m : Mach) :
    run (first ++ second) m = (run first m).bind (run second) := by
  induction first generalizing m with
  | nil => rfl
  | cons i rest ih =>
    simp only [List.cons_append, run]
    cases h : Mach.step i m <;> simp [Res.bind, ih]

theorem compile_append (first second : List Lir) :
    compile (first ++ second) = compile first ++ compile second := by
  simp [compile, List.flatMap_append]

theorem execute_append (first second : List Lir) (s : Store) :
    execute (first ++ second) s = (execute first s).bind (execute second) := by
  induction first generalizing s with
  | nil => rfl
  | cons row rest ih =>
    simp only [List.cons_append, execute]
    cases h : lirStep row s <;> simp [Res.bind, ih]

/-- Forward simulation of the entire flattened instruction stream. -/
theorem compiled_program_simulates (rows : List Lir) (s : Store) (m : Mach)
    (typed : Admissible rows s) (related : Rel s m) :
    Sim (execute rows s) (run (compile rows) m) := by
  induction rows generalizing s m with
  | nil => exact related
  | cons row rest ih =>
    obtain ⟨data, _, operands, tail⟩ := typed
    have sim := select_simulates row s m related data operands
    simp only [compile, List.flatMap_cons, run_append, execute]
    cases hs : lirStep row s with
    | ok next =>
      cases hm : run (select row) m with
      | ok machine =>
        have rel : Rel next machine := by simpa [hs, hm, Sim] using sim
        simpa [compile, Res.bind] using ih next machine (tail next hs) rel
      | abort failure => simp [hs, hm, Sim] at sim
    | abort failure =>
      cases hm : run (select row) m with
      | ok machine => simp [hs, hm, Sim] at sim
      | abort targetFailure =>
        simpa [hs, hm, Sim, Res.bind] using sim

/-- Logical row execution cannot manufacture a hardware fault. -/
theorem row_has_no_fault (row : Lir) (s : Store) (safe : row ≠ .trap .fault) :
    lirStep row s ≠ .abort .fault := by
  cases row <;> simp_all [lirStep]
  case arith op k d a b =>
    cases op <;> simp [checkedArith] <;> split <;> try simp_all
    all_goals split <;> simp_all
  case neg k d a => split <;> simp

theorem execution_has_no_fault (rows : List Lir) (s : Store)
    (typed : Admissible rows s) : execute rows s ≠ .abort .fault := by
  induction rows generalizing s with
  | nil => simp [execute]
  | cons row rest ih =>
    obtain ⟨_, safe, _, tail⟩ := typed
    have noFault := row_has_no_fault row s safe
    simp only [execute]
    cases hs : lirStep row s with
    | ok next => simpa [Res.bind] using ih next (tail next hs)
    | abort failure => simpa [hs, Res.bind] using noFault

/-- The complete selected stream cannot fault, not just each isolated row. -/
theorem compiled_program_has_no_fault (rows : List Lir) (s : Store) (m : Mach)
    (typed : Admissible rows s) (related : Rel s m) :
    run (compile rows) m ≠ .abort .fault := by
  have simulation := compiled_program_simulates rows s m typed related
  have noFault := execution_has_no_fault rows s typed
  intro bad
  cases hs : execute rows s with
  | ok next => simp [hs, bad, Sim] at simulation
  | abort failure =>
    have same : failure = .fault := by simpa [hs, bad, Sim] using simulation
    exact noFault (hs.trans (congrArg Res.abort same))

/-- Aborting prefixes prevent execution of arbitrary suffixes on both sides. -/
theorem abort_skips_suffix (first second : List Lir) (s : Store) (m : Mach)
    (typed : Admissible first s) (related : Rel s m) (failure : Failure)
    (aborts : execute first s = .abort failure) :
    execute (first ++ second) s = .abort failure ∧
      run (compile (first ++ second)) m = .abort failure := by
  constructor
  · simp [execute_append, aborts]
  · have sim := compiled_program_simulates first s m typed related
    cases hm : run (compile first) m with
    | ok next => simp [aborts, hm, Sim] at sim
    | abort target =>
      have same : failure = target := by simpa [aborts, hm, Sim] using sim
      simp [compile_append, run_append, hm, same]

/-- A row cannot rely on the initial store after a preceding row changes it. -/
theorem ill_typed_continuation_rejected (s : Store) :
    ¬ Admissible [.const 0 2, .not 1 0] s := by
  simp [Admissible, DataRow, Typed, lirStep, InRange, Kind.lo, Kind.hi, upd]

def overflowExample : List Lir :=
  [.const 0 255, .const 1 1, .arith .add .u8 2 0 1, .trap .fault]

/-- The final fault is unreachable because checked addition aborts first. -/
theorem overflow_example (s : Store) :
    Admissible overflowExample s ∧ execute overflowExample s = .abort .overflow := by
  simp [overflowExample, Admissible, DataRow, Typed, lirStep, execute,
    checkedArith, fits, InRange, Kind.lo, Kind.hi, upd, Res.bind]

theorem compiled_overflow_example (s : Store) (m : Mach) (related : Rel s m) :
    run (compile overflowExample) m = .abort .overflow := by
  simpa using (abort_skips_suffix overflowExample [] s m
    (overflow_example s).1 related .overflow (overflow_example s).2).2

def additionExample : List Lir :=
  [.const 0 40, .const 1 2, .arith .add .u8 2 0 1]

theorem addition_example (s : Store) :
    Admissible additionExample s ∧
      execute additionExample s = .ok (upd (upd (upd s (.slot 0) 40) (.slot 1) 2) (.slot 2) 42) := by
  simp [additionExample, Admissible, DataRow, Typed, lirStep, execute,
    checkedArith, fits, InRange, Kind.lo, Kind.hi, upd, Res.bind]

theorem compiled_addition_example (s : Store) (m : Mach) (related : Rel s m) :
    ∃ result, run (compile additionExample) m = .ok result ∧ result.mem (.slot 2) = 42 := by
  have sim := compiled_program_simulates additionExample s m (addition_example s).1 related
  rw [(addition_example s).2] at sim
  cases hm : run (compile additionExample) m with
  | abort failure => simp [hm, Sim] at sim
  | ok result =>
    refine ⟨result, rfl, ?_⟩
    have rel := (show Rel _ result from by simpa [hm, Sim] using sim) (.slot 2)
    simpa [upd, enc, W] using rel

end Vesper.CodegenComposition

#print axioms Vesper.CodegenComposition.run_append
#print axioms Vesper.CodegenComposition.compile_append
#print axioms Vesper.CodegenComposition.execute_append
#print axioms Vesper.CodegenComposition.compiled_program_simulates
#print axioms Vesper.CodegenComposition.row_has_no_fault
#print axioms Vesper.CodegenComposition.execution_has_no_fault
#print axioms Vesper.CodegenComposition.compiled_program_has_no_fault
#print axioms Vesper.CodegenComposition.abort_skips_suffix

#print axioms Vesper.CodegenComposition.ill_typed_continuation_rejected
#print axioms Vesper.CodegenComposition.overflow_example
#print axioms Vesper.CodegenComposition.compiled_overflow_example
#print axioms Vesper.CodegenComposition.addition_example
#print axioms Vesper.CodegenComposition.compiled_addition_example

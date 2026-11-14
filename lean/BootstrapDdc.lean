/-!
# §42.4 bootstrap simulation and DDC theorems, and a model of the §42.5 gate

The two §42.4 theorems are stated over abstract evaluators and compilers,
exactly as in the specification. `gate` models the fixed-point gate in
`crates/vesper-codegen-verified` (`audit_ddc_bootstrap_quorum`) over abstract
artifacts, an abstract digest function and a list of verified builder
operators. The theorems are about these Lean models only: they are not a
proof about the Rust crate, about BLAKE3 or about any real compiler. Digest
collision resistance enters only as an explicit injectivity hypothesis.
-/
namespace Vesper.BootstrapDdc

def modelId : String := "vesper.bootstrap.ddc-gate-model/1"

theorem end_to_end_verified_codegen_simulation
    {Ir Mach Outcome : Type}
    (evalIr   : Ir -> Outcome)
    (evalMach : Mach -> Outcome)
    (verifyWitness : Ir -> Ir -> Bool)
    (emitMach      : Ir -> Option Mach)
    (h_witness_sound : forall ir1 ir2, verifyWitness ir1 ir2 = true -> evalIr ir2 = evalIr ir1)
    (h_backend_sim   : forall opt m, emitMach opt = some m -> evalMach m = evalIr opt)
    (srcIr optIr : Ir) (machBin : Mach)
    (hw : verifyWitness srcIr optIr = true)
    (hc : emitMach optIr = some machBin) :
    evalMach machBin = evalIr srcIr := by
  rw [h_backend_sim optIr machBin hc, h_witness_sound srcIr optIr hw]

theorem ddc_eliminates_seed_trojan_on_compiler
    {Src Bin : Type} [DecidableEq Bin]
    (specCompile : Src -> Bin)
    (compileRef  : Src -> Bin)
    (execStage2  : Bin -> Src -> Bin)
    (compilerSrc : Src) (stage2Bin : Bin)
    (h_ref_correct : compileRef compilerSrc = specCompile compilerSrc)
    (h_fixed_pt    : execStage2 stage2Bin compilerSrc = stage2Bin)
    (h_ddc_match   : stage2Bin = compileRef compilerSrc) :
    execStage2 stage2Bin compilerSrc = specCompile compilerSrc := by
  rw [h_fixed_pt, h_ddc_match, h_ref_correct]

/-! ## The fixed-point and quorum gate -/

/-- Digests recorded in `stage3_ddc.lock` and its required quorum. -/
structure Lock (D : Type) where
  stage2 : D
  stage3 : D
  pathB : D
  quorum : Nat

/-- Artifact bytes and the operators whose signatures verified. -/
structure Evidence (B : Type) where
  stage2 : B
  stage3 : B
  pathB : B
  operators : List Nat

inductive GateError where
  | lockNotAtFixedPoint
  | stageFixedPointDivergence
  | ddcCrossCompilerMismatch
  | tamperedDigest
  | insufficientQuorum
  deriving DecidableEq, Repr

/-- Number of distinct operators: one operator signing twice counts once. -/
def distinctOperators (ops : List Nat) : Nat := ops.eraseDups.length

/-- Same check order as the Rust gate. -/
def gate {B D : Type} [DecidableEq D] (hash : B → D) (lock : Lock D) (ev : Evidence B) :
    Except GateError D :=
  if lock.stage2 ≠ lock.stage3 ∨ lock.stage3 ≠ lock.pathB then .error .lockNotAtFixedPoint
  else if hash ev.stage2 ≠ hash ev.stage3 then .error .stageFixedPointDivergence
  else if hash ev.stage3 ≠ hash ev.pathB then .error .ddcCrossCompilerMismatch
  else if hash ev.stage3 ≠ lock.stage3 then .error .tamperedDigest
  else if distinctOperators ev.operators < lock.quorum then .error .insufficientQuorum
  else .ok (hash ev.stage3)

variable {B D : Type} [DecidableEq D]

/-- An accepted gate means every artifact and every locked digest is the
fixed point, and the distinct-operator quorum was met. -/
theorem gate_accepts_only_fixed_point (hash : B → D) (lock : Lock D) (ev : Evidence B) (d : D)
    (h : gate hash lock ev = .ok d) :
    hash ev.stage2 = d ∧ hash ev.stage3 = d ∧ hash ev.pathB = d ∧
      lock.stage2 = d ∧ lock.stage3 = d ∧ lock.pathB = d ∧
      lock.quorum ≤ distinctOperators ev.operators := by
  unfold gate at h
  by_cases h1 : lock.stage2 ≠ lock.stage3 ∨ lock.stage3 ≠ lock.pathB
  · rw [if_pos h1] at h; cases h
  · rw [if_neg h1] at h
    by_cases h2 : hash ev.stage2 ≠ hash ev.stage3
    · rw [if_pos h2] at h; cases h
    · rw [if_neg h2] at h
      by_cases h3 : hash ev.stage3 ≠ hash ev.pathB
      · rw [if_pos h3] at h; cases h
      · rw [if_neg h3] at h
        by_cases h4 : hash ev.stage3 ≠ lock.stage3
        · rw [if_pos h4] at h; cases h
        · rw [if_neg h4] at h
          by_cases h5 : distinctOperators ev.operators < lock.quorum
          · rw [if_pos h5] at h; cases h
          · rw [if_neg h5] at h
            cases h
            have e1 : lock.stage2 = lock.stage3 := Decidable.byContradiction (fun n => h1 (Or.inl n))
            have e2 : lock.stage3 = lock.pathB := Decidable.byContradiction (fun n => h1 (Or.inr n))
            have e3 : hash ev.stage2 = hash ev.stage3 := Decidable.byContradiction h2
            have e4 : hash ev.stage3 = hash ev.pathB := Decidable.byContradiction h3
            have e5 : hash ev.stage3 = lock.stage3 := Decidable.byContradiction h4
            refine ⟨e3, rfl, e4.symm, ?_, e5.symm, ?_, Nat.le_of_not_lt h5⟩
            · rw [e1, e5]
            · rw [← e2, e5]

/-- A lock at its fixed point with Stage 2 and Stage 3 differing is rejected
with the divergence error, whatever the signatures. -/
theorem gate_rejects_stage_divergence (hash : B → D) (lock : Lock D) (ev : Evidence B)
    (hl : lock.stage2 = lock.stage3 ∧ lock.stage3 = lock.pathB)
    (hd : hash ev.stage2 ≠ hash ev.stage3) :
    gate hash lock ev = .error .stageFixedPointDivergence := by
  unfold gate
  have : ¬(lock.stage2 ≠ lock.stage3 ∨ lock.stage3 ≠ lock.pathB) := by
    intro h; cases h with
    | inl h => exact h hl.1
    | inr h => exact h hl.2
  simp only [this, hd, if_false, if_true, ne_eq, not_false_eq_true]

/-- Path A agreeing with itself but not with Path B is the DDC error. -/
theorem gate_rejects_ddc_mismatch (hash : B → D) (lock : Lock D) (ev : Evidence B)
    (hl : lock.stage2 = lock.stage3 ∧ lock.stage3 = lock.pathB)
    (ha : hash ev.stage2 = hash ev.stage3)
    (hb : hash ev.stage3 ≠ hash ev.pathB) :
    gate hash lock ev = .error .ddcCrossCompilerMismatch := by
  unfold gate
  have : ¬(lock.stage2 ≠ lock.stage3 ∨ lock.stage3 ≠ lock.pathB) := by
    intro h; cases h with
    | inl h => exact h hl.1
    | inr h => exact h hl.2
  simp only [this, ha, hb, if_false, if_true, ne_eq, not_true_eq_false, not_false_eq_true]

/-- Too few distinct operators never pass, so a missing signature fails. -/
theorem gate_rejects_missing_signature (hash : B → D) (lock : Lock D) (ev : Evidence B) (d : D)
    (hq : distinctOperators ev.operators < lock.quorum) :
    gate hash lock ev ≠ .ok d := by
  intro h
  have := (gate_accepts_only_fixed_point hash lock ev d h).2.2.2.2.2.2
  exact Nat.lt_irrefl _ (Nat.lt_of_lt_of_le hq this)

/-- With a collision-free digest, acceptance means the three artifacts are
bit-identical. -/
theorem gate_accepts_identical_artifacts (hash : B → D) (lock : Lock D) (ev : Evidence B) (d : D)
    (inj : ∀ x y, hash x = hash y → x = y)
    (h : gate hash lock ev = .ok d) :
    ev.stage2 = ev.stage3 ∧ ev.stage3 = ev.pathB := by
  have ⟨a, b, c, _⟩ := gate_accepts_only_fixed_point hash lock ev d h
  exact ⟨inj _ _ (a.trans b.symm), inj _ _ (b.trans c.symm)⟩

/-- The gate discharges the DDC theorem's hypotheses: Stage 3 is what Stage 2
produced from the compiler source, Path B is the reference compiler's output,
and the reference compiler is correct. Then the self-compiled compiler's
output is the specification's. -/
theorem gate_eliminates_seed_trojan
    {Src : Type} [DecidableEq B]
    (hash : B → D) (lock : Lock D) (ev : Evidence B) (d : D)
    (specCompile compileRef : Src → B) (execStage2 : B → Src → B) (compilerSrc : Src)
    (inj : ∀ x y, hash x = hash y → x = y)
    (h_gate : gate hash lock ev = .ok d)
    (h_stage3 : execStage2 ev.stage2 compilerSrc = ev.stage3)
    (h_pathB : compileRef compilerSrc = ev.pathB)
    (h_ref_correct : compileRef compilerSrc = specCompile compilerSrc) :
    execStage2 ev.stage2 compilerSrc = specCompile compilerSrc := by
  have ⟨e23, e3b⟩ := gate_accepts_identical_artifacts hash lock ev d inj h_gate
  exact ddc_eliminates_seed_trojan_on_compiler specCompile compileRef execStage2 compilerSrc
    ev.stage2 h_ref_correct (h_stage3.trans e23.symm) (e23.trans (e3b.trans h_pathB.symm))

/-- Concrete check: an honest three-way match with three operators passes. -/
example : gate (fun n : Nat => n) ⟨5, 5, 5, 3⟩ ⟨5, 5, 5, [1, 2, 3]⟩ = .ok 5 := rfl
/-- Concrete check: the same operator signing three times is a quorum of one. -/
example : gate (fun n : Nat => n) ⟨5, 5, 5, 3⟩ ⟨5, 5, 5, [1, 1, 1]⟩ = .error .insufficientQuorum :=
  rfl

end Vesper.BootstrapDdc

#print axioms Vesper.BootstrapDdc.end_to_end_verified_codegen_simulation
#print axioms Vesper.BootstrapDdc.ddc_eliminates_seed_trojan_on_compiler
#print axioms Vesper.BootstrapDdc.gate_accepts_only_fixed_point
#print axioms Vesper.BootstrapDdc.gate_rejects_stage_divergence
#print axioms Vesper.BootstrapDdc.gate_rejects_ddc_mismatch
#print axioms Vesper.BootstrapDdc.gate_rejects_missing_signature
#print axioms Vesper.BootstrapDdc.gate_accepts_identical_artifacts
#print axioms Vesper.BootstrapDdc.gate_eliminates_seed_trojan

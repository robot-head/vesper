import Std

/-! Handwritten arithmetic models for §41. These do not prove the Rust harness,
clock, PMU access, statistics assumptions, compiler lowering or hardware behavior.
No proof of spill-free black_box, automatic ratio cancellation or full confidence
inference is claimed. The counter lemma is a selected width-64 modular model;
the executable helper requires an explicit actual width and a <modulus bound. -/
namespace Vesper.MetronomeArithmetic

theorem width64_modular_delta_exact (start delta : Nat)
    (hs : start < 18446744073709551616)
    (hd : delta < 18446744073709551616) :
    (((start + delta) % 18446744073709551616 + 18446744073709551616 - start)
      % 18446744073709551616) = delta := by omega

theorem paired_additive_drift_difference (a b commonDrift : Int) :
    (b + commonDrift) - (a + commonDrift) = b - a := by omega

/-- Even exactly common additive drift does not cancel a ratio denominator.
For A=1, B=2, drift=1, the ratios are 2/1 and 3/2; cross products differ. -/
theorem common_drift_ratio_counterexample :
    (2 + 1) * 1 ≠ 2 * (1 + 1) := by decide

theorem scaled_u64_difference_fits_i128 (a b : Int)
    (ha : 0 ≤ a ∧ a ≤ 18446744073709551615)
    (hb : 0 ≤ b ∧ b ≤ 18446744073709551615) :
    -170141183460469231731687303715884105728 ≤ (b - a) * 10000 ∧
    (b - a) * 10000 ≤ 170141183460469231731687303715884105727 := by omega

#print axioms width64_modular_delta_exact
#print axioms paired_additive_drift_difference
#print axioms common_drift_ratio_counterexample
#print axioms scaled_u64_difference_fits_i128
end Vesper.MetronomeArithmetic

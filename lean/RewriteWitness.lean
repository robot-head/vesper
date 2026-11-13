import Std

/-!
# Mathematical replay of bounded integer rewrite witnesses

The semantic functions below describe strict, deterministic integer expressions
with one empty abortive overflow failure. They do not model arbitrary effects,
source binding, Rust admission/checking, code generation, or machine execution.
Generated declarations reference earlier semantic functions and equality proofs;
sharing those declarations does not give expression evaluation a CSE/let meaning.

Constants require an explicit range proof. Unsigned values decode through
`toNat`, signed i64 values through two's-complement `toInt`; the nominal Value
wrapper keeps u64 and i64 distinct despite their common bit width.
-/

namespace Vesper.RewriteWitness

inductive Kind where
  | u8 | u32 | u64 | i64

def Kind.width : Kind → Nat
  | .u8 => 8
  | .u32 => 32
  | .u64 | .i64 => 64

structure Value (k : Kind) where
  bits : BitVec k.width
  deriving DecidableEq

abbrev Env (k : Kind) := Nat → Value k
abbrev Sem (k : Kind) := Env k → Option (Value k)

private def lower : Kind → Int
  | .i64 => -9223372036854775808
  | _ => 0

private def upper : Kind → Int
  | .u8 => 255
  | .u32 => 4294967295
  | .u64 => 18446744073709551615
  | .i64 => 9223372036854775807

def Fits (k : Kind) (n : Int) : Prop := lower k ≤ n ∧ n ≤ upper k

instance (k : Kind) (n : Int) : Decidable (Fits k n) :=
  inferInstanceAs (Decidable (lower k ≤ n ∧ n ≤ upper k))

def toInt {k : Kind} (v : Value k) : Int :=
  match k with
  | .i64 => v.bits.toInt
  | .u8 | .u32 | .u64 => v.bits.toNat

def literalValue (k : Kind) (n : Int) (h : Fits k n) : Value k :=
  let _ := h
  ⟨BitVec.ofInt k.width n⟩

-- Work directly over Nat here. Some Std signed-bound helpers, and omega
-- applied to an unsplit conjunction, introduce Classical.choice in Lean 4.24.
private theorem signedNatBounds (n : Nat) (h : n < 18446744073709551616) :
    -9223372036854775808 ≤
      (if 2 * n < 18446744073709551616 then (n : Int) else (n : Int) - 18446744073709551616) ∧
    (if 2 * n < 18446744073709551616 then (n : Int) else (n : Int) - 18446744073709551616)
      ≤ 9223372036854775807 := by
  by_cases hc : 2 * n < 18446744073709551616
  · simp only [if_pos hc]
    constructor <;> omega
  · simp only [if_neg hc]
    constructor <;> omega

theorem value_fits {k : Kind} (v : Value k) : Fits k (toInt v) := by
  cases k with
  | u8 =>
    have h : v.bits.toNat < 256 := v.bits.isLt
    change 0 ≤ (v.bits.toNat : Int) ∧ (v.bits.toNat : Int) ≤ 255
    constructor <;> omega
  | u32 =>
    have h : v.bits.toNat < 4294967296 := v.bits.isLt
    change 0 ≤ (v.bits.toNat : Int) ∧ (v.bits.toNat : Int) ≤ 4294967295
    constructor <;> omega
  | u64 =>
    have h : v.bits.toNat < 18446744073709551616 := v.bits.isLt
    change 0 ≤ (v.bits.toNat : Int) ∧ (v.bits.toNat : Int) ≤ 18446744073709551615
    constructor <;> omega
  | i64 =>
    change -9223372036854775808 ≤ v.bits.toInt ∧ v.bits.toInt ≤ 9223372036854775807
    rw [BitVec.toInt_eq_toNat_cond]
    exact signedNatBounds v.bits.toNat v.bits.isLt

theorem literal_exact (k : Kind) (n : Int) (h : Fits k n) :
    toInt (literalValue k n h) = n := by
  cases k with
  | u8 =>
    change ((BitVec.ofInt 8 n).toNat : Int) = n
    rw [BitVec.toNat_ofInt,
      Int.emod_eq_of_lt h.1 (by have hh := h.2; change n ≤ 255 at hh; omega)]
    exact Int.toNat_of_nonneg h.1
  | u32 =>
    change ((BitVec.ofInt 32 n).toNat : Int) = n
    rw [BitVec.toNat_ofInt,
      Int.emod_eq_of_lt h.1 (by have hh := h.2; change n ≤ 4294967295 at hh; omega)]
    exact Int.toNat_of_nonneg h.1
  | u64 =>
    change ((BitVec.ofInt 64 n).toNat : Int) = n
    rw [BitVec.toNat_ofInt,
      Int.emod_eq_of_lt h.1 (by have hh := h.2; change n ≤ 18446744073709551615 at hh; omega)]
    exact Int.toNat_of_nonneg h.1
  | i64 =>
    change (BitVec.ofInt 64 n).toInt = n
    apply BitVec.toInt_ofInt_eq_self (by decide)
    · exact h.1
    · have hh := h.2
      change n ≤ 9223372036854775807 at hh
      omega

theorem value_toInt_injective {k : Kind} {a b : Value k}
    (h : toInt a = toInt b) : a = b := by
  cases a with
  | mk a =>
    cases b with
    | mk b =>
      have hab : a = b := by
        cases k with
        | u8 => exact BitVec.toNat_inj.mp (Int.ofNat.inj h)
        | u32 => exact BitVec.toNat_inj.mp (Int.ofNat.inj h)
        | u64 => exact BitVec.toNat_inj.mp (Int.ofNat.inj h)
        | i64 => exact BitVec.toInt_inj.mp h
      cases hab
      rfl

theorem literal_roundtrip {k : Kind} (v : Value k) :
    literalValue k (toInt v) (value_fits v) = v := by
  apply value_toInt_injective
  exact literal_exact k (toInt v) (value_fits v)

private theorem zeroFits (k : Kind) : Fits k 0 := by cases k <;> decide
private theorem oneFits (k : Kind) : Fits k 1 := by cases k <;> decide

def const (k : Kind) (n : Int) (h : Fits k n) : Sem k :=
  fun _ => some (literalValue k n h)
def var (k : Kind) (id : Nat) : Sem k := fun env => some (env id)
def zero (k : Kind) : Sem k := const k 0 (zeroFits k)
def one (k : Kind) : Sem k := const k 1 (oneFits k)

inductive IntOp where
  | add | sub | mul

private def exactInt (op : IntOp) (a b : Int) : Int :=
  match op with
  | .add => a + b
  | .sub => a - b
  | .mul => a * b

def checkedValue {k : Kind} (op : IntOp) (a b : Value k) : Option (Value k) :=
  let n := exactInt op (toInt a) (toInt b)
  if h : Fits k n then some (literalValue k n h) else none

def wrappingValue {k : Kind} (op : IntOp) (a b : Value k) : Value k :=
  ⟨match op with
    | .add => a.bits + b.bits
    | .sub => a.bits - b.bits
    | .mul => a.bits * b.bits⟩

def checked {k : Kind} (op : IntOp) (a b : Sem k) : Sem k := fun env =>
  match a env, b env with
  | some x, some y => checkedValue op x y
  | _, _ => none

def wrapping {k : Kind} (op : IntOp) (a b : Sem k) : Sem k := fun env =>
  match a env, b env with
  | some x, some y => some (wrappingValue op x y)
  | _, _ => none

def Total {k : Kind} (x : Sem k) : Prop := ∀ env, ∃ v, x env = some v

theorem total_const (k : Kind) (n : Int) (h : Fits k n) : Total (const k n h) :=
  fun _ => ⟨literalValue k n h, rfl⟩
theorem total_var (k : Kind) (id : Nat) : Total (var k id) :=
  fun env => ⟨env id, rfl⟩
theorem total_of_eq {k : Kind} {x y : Sem k} (h : x = y) (hy : Total y) : Total x := by
  cases h
  exact hy

theorem congr_checked {k : Kind} {a a' b b' : Sem k} (op : IntOp)
    (ha : a = a') (hb : b = b') : checked op a b = checked op a' b' := by
  cases ha
  cases hb
  rfl
theorem congr_wrapping {k : Kind} {a a' b b' : Sem k} (op : IntOp)
    (ha : a = a') (hb : b = b') : wrapping op a b = wrapping op a' b' := by
  cases ha
  cases hb
  rfl

private def zeroValue (k : Kind) : Value k := literalValue k 0 (zeroFits k)
private def oneValue (k : Kind) : Value k := literalValue k 1 (oneFits k)

private theorem zeroInt (k : Kind) : toInt (zeroValue k) = 0 := literal_exact k 0 _
private theorem oneInt (k : Kind) : toInt (oneValue k) = 1 := literal_exact k 1 _
private theorem zeroBits (k : Kind) : (zeroValue k).bits = BitVec.ofNat k.width 0 := by
  cases k <;> rfl
private theorem oneBits (k : Kind) : (oneValue k).bits = BitVec.ofNat k.width 1 := by
  cases k <;> rfl

private theorem checkedExact {k : Kind} (op : IntOp) (a b : Value k) (n : Int)
    (hn : Fits k n) (h : exactInt op (toInt a) (toInt b) = n) :
    checkedValue op a b = some (literalValue k n hn) := by
  simp only [checkedValue, h, dif_pos hn]

private theorem cAddZero {k : Kind} (a : Value k) :
    checkedValue .add a (zeroValue k) = some a := by
  have h := checkedExact .add a (zeroValue k) (toInt a) (value_fits a)
    (by simp only [exactInt, zeroInt, Int.add_zero])
  simpa only [literal_roundtrip] using h
private theorem cZeroAdd {k : Kind} (a : Value k) :
    checkedValue .add (zeroValue k) a = some a := by
  have h := checkedExact .add (zeroValue k) a (toInt a) (value_fits a)
    (by simp only [exactInt, zeroInt, Int.zero_add])
  simpa only [literal_roundtrip] using h
private theorem cSubZero {k : Kind} (a : Value k) :
    checkedValue .sub a (zeroValue k) = some a := by
  have h := checkedExact .sub a (zeroValue k) (toInt a) (value_fits a)
    (by simp only [exactInt, zeroInt, Int.sub_zero])
  simpa only [literal_roundtrip] using h
private theorem cMulOne {k : Kind} (a : Value k) :
    checkedValue .mul a (oneValue k) = some a := by
  have h := checkedExact .mul a (oneValue k) (toInt a) (value_fits a)
    (by simp only [exactInt, oneInt, Int.mul_one])
  simpa only [literal_roundtrip] using h
private theorem cOneMul {k : Kind} (a : Value k) :
    checkedValue .mul (oneValue k) a = some a := by
  have h := checkedExact .mul (oneValue k) a (toInt a) (value_fits a)
    (by simp only [exactInt, oneInt, Int.one_mul])
  simpa only [literal_roundtrip] using h
private theorem cMulZero {k : Kind} (a : Value k) :
    checkedValue .mul a (zeroValue k) = some (zeroValue k) :=
  checkedExact .mul a (zeroValue k) 0 (zeroFits k)
    (by simp only [exactInt, zeroInt, Int.mul_zero])
private theorem cZeroMul {k : Kind} (a : Value k) :
    checkedValue .mul (zeroValue k) a = some (zeroValue k) :=
  checkedExact .mul (zeroValue k) a 0 (zeroFits k)
    (by simp only [exactInt, zeroInt, Int.zero_mul])
private theorem cSelfSub {k : Kind} (a : Value k) :
    checkedValue .sub a a = some (zeroValue k) :=
  checkedExact .sub a a 0 (zeroFits k) (by simp only [exactInt, Int.sub_self])

-- Do not use Std BitVec.mul_one/one_mul: their 4.24 axiom closures contain
-- Classical.choice. This direct modular identity needs only natural bounds.
private theorem widthPowGtOne (k : Kind) : 1 < 2 ^ k.width := by cases k <;> decide
private theorem bitsMulOne (k : Kind) (a : BitVec k.width) :
    a * BitVec.ofNat k.width 1 = a := by
  apply BitVec.toNat_inj.mp
  rw [BitVec.toNat_mul]
  change (a.toNat * (1 % 2 ^ k.width)) % 2 ^ k.width = a.toNat
  rw [Nat.mod_eq_of_lt (widthPowGtOne k), Nat.mul_one]
  exact Nat.mod_eq_of_lt a.isLt

private theorem wAddZero {k : Kind} (a : Value k) : wrappingValue .add a (zeroValue k) = a := by
  change (Value.mk (a.bits + (zeroValue k).bits) : Value k) = Value.mk a.bits
  rw [zeroBits, BitVec.add_zero]
private theorem wSubZero {k : Kind} (a : Value k) : wrappingValue .sub a (zeroValue k) = a := by
  change (Value.mk (a.bits - (zeroValue k).bits) : Value k) = Value.mk a.bits
  rw [zeroBits, BitVec.sub_zero]
private theorem wMulOne {k : Kind} (a : Value k) : wrappingValue .mul a (oneValue k) = a := by
  change (Value.mk (a.bits * (oneValue k).bits) : Value k) = Value.mk a.bits
  rw [oneBits, bitsMulOne]
private theorem wMulZero {k : Kind} (a : Value k) :
    wrappingValue .mul a (zeroValue k) = zeroValue k := by
  change (Value.mk (a.bits * (zeroValue k).bits) : Value k) = Value.mk (zeroValue k).bits
  rw [zeroBits, BitVec.mul_zero]
private theorem wSelfSub {k : Kind} (a : Value k) : wrappingValue .sub a a = zeroValue k := by
  change (Value.mk (a.bits - a.bits) : Value k) = Value.mk (zeroValue k).bits
  rw [zeroBits, BitVec.sub_self]

theorem r01_add_zero {k : Kind} (x : Sem k) : checked .add x (zero k) = x := by
  funext env
  cases hx : x env <;> simp only [checked, zero, const, hx]
  exact cAddZero _
theorem r02_zero_add {k : Kind} (x : Sem k) : checked .add (zero k) x = x := by
  funext env
  cases hx : x env <;> simp only [checked, zero, const, hx]
  exact cZeroAdd _
theorem r03_sub_zero {k : Kind} (x : Sem k) : checked .sub x (zero k) = x := by
  funext env
  cases hx : x env <;> simp only [checked, zero, const, hx]
  exact cSubZero _
theorem r04_mul_one {k : Kind} (x : Sem k) : checked .mul x (one k) = x := by
  funext env
  cases hx : x env <;> simp only [checked, one, const, hx]
  exact cMulOne _
theorem r05_one_mul {k : Kind} (x : Sem k) : checked .mul (one k) x = x := by
  funext env
  cases hx : x env <;> simp only [checked, one, const, hx]
  exact cOneMul _
theorem r06_mul_zero {k : Kind} (x : Sem k) (hx : Total x) :
    checked .mul x (zero k) = zero k := by
  funext env
  obtain ⟨v, hv⟩ := hx env
  simp only [checked, zero, const, hv]
  exact cMulZero v
theorem r07_zero_mul {k : Kind} (x : Sem k) (hx : Total x) :
    checked .mul (zero k) x = zero k := by
  funext env
  obtain ⟨v, hv⟩ := hx env
  simp only [checked, zero, const, hv]
  exact cZeroMul v
theorem r08_self_sub {k : Kind} (x : Sem k) (hx : Total x) :
    checked .sub x x = zero k := by
  funext env
  obtain ⟨v, hv⟩ := hx env
  simp only [checked, zero, const, hv]
  exact cSelfSub v
theorem r09_wadd_zero {k : Kind} (x : Sem k) : wrapping .add x (zero k) = x := by
  funext env
  cases hx : x env <;> simp only [wrapping, zero, const, hx]
  exact congrArg some (wAddZero _)
theorem r10_wsub_zero {k : Kind} (x : Sem k) : wrapping .sub x (zero k) = x := by
  funext env
  cases hx : x env <;> simp only [wrapping, zero, const, hx]
  exact congrArg some (wSubZero _)
theorem r11_wmul_one {k : Kind} (x : Sem k) : wrapping .mul x (one k) = x := by
  funext env
  cases hx : x env <;> simp only [wrapping, one, const, hx]
  exact congrArg some (wMulOne _)
theorem r12_wadd_comm {k : Kind} (x y : Sem k) : wrapping .add x y = wrapping .add y x := by
  funext env
  cases hx : x env <;> cases hy : y env <;>
    simp only [wrapping, hx, hy, wrappingValue, BitVec.add_comm]
theorem r13_wmul_comm {k : Kind} (x y : Sem k) : wrapping .mul x y = wrapping .mul y x := by
  funext env
  cases hx : x env <;> cases hy : y env <;>
    simp only [wrapping, hx, hy, wrappingValue, BitVec.mul_comm]
theorem r14_wadd_assoc {k : Kind} (x y z : Sem k) :
    wrapping .add (wrapping .add x y) z = wrapping .add x (wrapping .add y z) := by
  funext env
  cases hx : x env <;> cases hy : y env <;> cases hz : z env <;>
    simp only [wrapping, hx, hy, hz, wrappingValue, BitVec.add_assoc]
theorem r15_wmul_assoc {k : Kind} (x y z : Sem k) :
    wrapping .mul (wrapping .mul x y) z = wrapping .mul x (wrapping .mul y z) := by
  funext env
  cases hx : x env <;> cases hy : y env <;> cases hz : z env <;>
    simp only [wrapping, hx, hy, hz, wrappingValue, BitVec.mul_assoc]
theorem r16_wadd_cancel {k : Kind} (x y : Sem k) (hy : Total y) :
    wrapping .sub (wrapping .add x y) y = x := by
  funext env
  obtain ⟨v, hv⟩ := hy env
  cases hx : x env <;>
    simp only [wrapping, hx, hv, wrappingValue, BitVec.add_sub_cancel]
theorem r17_wmul_zero {k : Kind} (x : Sem k) (hx : Total x) :
    wrapping .mul x (zero k) = zero k := by
  funext env
  obtain ⟨v, hv⟩ := hx env
  simp only [wrapping, zero, const, hv]
  exact congrArg some (wMulZero v)
theorem r18_wself_sub {k : Kind} (x : Sem k) (hx : Total x) :
    wrapping .sub x x = zero k := by
  funext env
  obtain ⟨v, hv⟩ := hx env
  simp only [wrapping, zero, const, hv]
  exact congrArg some (wSelfSub v)

theorem r19_checked_add_constants {k : Kind} (a b r : Int)
    (ha : Fits k a) (hb : Fits k b) (hr : Fits k r)
    (h : checkedValue .add (literalValue k a ha) (literalValue k b hb) = some (literalValue k r hr)) :
    checked .add (const k a ha) (const k b hb) = const k r hr := by funext env; exact h
theorem r20_checked_sub_constants {k : Kind} (a b r : Int)
    (ha : Fits k a) (hb : Fits k b) (hr : Fits k r)
    (h : checkedValue .sub (literalValue k a ha) (literalValue k b hb) = some (literalValue k r hr)) :
    checked .sub (const k a ha) (const k b hb) = const k r hr := by funext env; exact h
theorem r21_checked_mul_constants {k : Kind} (a b r : Int)
    (ha : Fits k a) (hb : Fits k b) (hr : Fits k r)
    (h : checkedValue .mul (literalValue k a ha) (literalValue k b hb) = some (literalValue k r hr)) :
    checked .mul (const k a ha) (const k b hb) = const k r hr := by funext env; exact h
theorem r22_wrapping_add_constants {k : Kind} (a b r : Int)
    (ha : Fits k a) (hb : Fits k b) (hr : Fits k r)
    (h : wrappingValue .add (literalValue k a ha) (literalValue k b hb) = literalValue k r hr) :
    wrapping .add (const k a ha) (const k b hb) = const k r hr := by funext env; exact congrArg some h
theorem r23_wrapping_sub_constants {k : Kind} (a b r : Int)
    (ha : Fits k a) (hb : Fits k b) (hr : Fits k r)
    (h : wrappingValue .sub (literalValue k a ha) (literalValue k b hb) = literalValue k r hr) :
    wrapping .sub (const k a ha) (const k b hb) = const k r hr := by funext env; exact congrArg some h
theorem r24_wrapping_mul_constants {k : Kind} (a b r : Int)
    (ha : Fits k a) (hb : Fits k b) (hr : Fits k r)
    (h : wrappingValue .mul (literalValue k a ha) (literalValue k b hb) = literalValue k r hr) :
    wrapping .mul (const k a ha) (const k b hb) = const k r hr := by funext env; exact congrArg some h

end Vesper.RewriteWitness

-- Report every fixed catalog/support root, including rules unused by a fixture.
#print axioms Vesper.RewriteWitness.r01_add_zero
#print axioms Vesper.RewriteWitness.r02_zero_add
#print axioms Vesper.RewriteWitness.r03_sub_zero
#print axioms Vesper.RewriteWitness.r04_mul_one
#print axioms Vesper.RewriteWitness.r05_one_mul
#print axioms Vesper.RewriteWitness.r06_mul_zero
#print axioms Vesper.RewriteWitness.r07_zero_mul
#print axioms Vesper.RewriteWitness.r08_self_sub
#print axioms Vesper.RewriteWitness.r09_wadd_zero
#print axioms Vesper.RewriteWitness.r10_wsub_zero
#print axioms Vesper.RewriteWitness.r11_wmul_one
#print axioms Vesper.RewriteWitness.r12_wadd_comm
#print axioms Vesper.RewriteWitness.r13_wmul_comm
#print axioms Vesper.RewriteWitness.r14_wadd_assoc
#print axioms Vesper.RewriteWitness.r15_wmul_assoc
#print axioms Vesper.RewriteWitness.r16_wadd_cancel
#print axioms Vesper.RewriteWitness.r17_wmul_zero
#print axioms Vesper.RewriteWitness.r18_wself_sub
#print axioms Vesper.RewriteWitness.r19_checked_add_constants
#print axioms Vesper.RewriteWitness.r20_checked_sub_constants
#print axioms Vesper.RewriteWitness.r21_checked_mul_constants
#print axioms Vesper.RewriteWitness.r22_wrapping_add_constants
#print axioms Vesper.RewriteWitness.r23_wrapping_sub_constants
#print axioms Vesper.RewriteWitness.r24_wrapping_mul_constants
#print axioms Vesper.RewriteWitness.value_fits
#print axioms Vesper.RewriteWitness.literal_exact
#print axioms Vesper.RewriteWitness.value_toInt_injective
#print axioms Vesper.RewriteWitness.literal_roundtrip
#print axioms Vesper.RewriteWitness.total_const
#print axioms Vesper.RewriteWitness.total_var
#print axioms Vesper.RewriteWitness.total_of_eq
#print axioms Vesper.RewriteWitness.congr_checked
#print axioms Vesper.RewriteWitness.congr_wrapping

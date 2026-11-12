/-!
# Baseline128 delimiter-count/XOR scanner: algorithm and bounds model

Model identifier: `vesper.simd.baseline128.algorithm-model/1`.

This file is a Lean model of the algorithm that `crates/vesper-simd/src/lib.rs`
(`define_scanner`, export `vesper_simd_scan_baseline128_v1`) builds as
Cranelift IR. It is **not** a certificate for Cranelift, its x86_64/AArch64
instruction selection, the emitted object code, object linking, CPU feature
discovery, source `eff [Isa[P]]` typing, scalable vectors or performance. It
proves that the block/mask/XOR algorithm, *as transcribed here*, computes the
scalar reference and only reads inside the logical slice.

The transcription follows the IR block by block:

* `entry`: `full_len = band_imm(len, -16)`, `needle = splat(delim)`, start
  `(offset, count, acc) = (0, 0, splat 0)`.
* `vector_head`/`vector_body`: while `offset <u full_len`, do one unaligned
  16-byte load at `input + offset`, a lane-wise unsigned `icmp eq` producing
  `0xff`/`0x00` lanes, then for lanes 0..15 in order
  `count += uextend.i64(band_imm(extractlane(eq, lane), 1))`, then
  `acc = bxor(acc, chunk)` and `offset += 16`.
* `reduce`: `x = 0; for lane 0..15: x ^= extractlane(acc, lane)`.
* `scalar_head`/`scalar_body`: while `offset <u len`, load one byte, scalar
  `icmp eq` producing `i8` 1/0, `count += uextend.i64`, `x ^= byte`,
  `offset += 1`.
* `finish`: store `x` to the output byte and return `count`.

All machine words are `BitVec 64` with Cranelift's wrapping `iadd`; bytes are
`BitVec 8`. The loops use explicit fuel only so that the definitions are total:
a fuel-exhausted run returns `none`, and the theorems show the exact iteration
counts (`len / 16` vector, `len % 16` scalar) suffice. Every load is appended to
an explicit read log, which the bounds theorems inspect.

Assumptions are stated as hypotheses, not axioms:

* `MemoryModel`: the flat byte memory holds `data` at
  `input + i` for every `i < data.length`, and `len = data.length` (so the
  length is representable in `u64` by construction of `BitVec 64`).
* `NoWrap`: `input.toNat + len.toNat ≤ 2^64`, i.e. the readable slice does not
  wrap around the address space. This is what a valid Rust slice / C object
  provides; it is the only ABI fact the address-level bounds use. The `u64`
  counter, offset and loop-increment representability are proved, not assumed.

Proofs use only core `BitVec`/`List`/`Nat` lemmas, induction, `omega`, `simp`
and kernel `decide` on closed 64-bit constants. There is no `sorry`, no project
axiom, no `native_decide` and no `bv_decide`. The `#print axioms` lines at the
end list the actual transitive axioms.
-/

namespace Vesper.SimdScanner

def modelId : String := "vesper.simd.baseline128.algorithm-model/1"
def modelClaim : String :=
  "algorithm/bounds model only; not a Cranelift, ISA-lowering or machine-code certificate"

abbrev Byte := BitVec 8
abbrev Word := BitVec 64
/-- Flat 64-bit byte-addressed memory. -/
abbrev Mem := Word → Byte
/-- An `I8X16` value. Lane `i` of a load at `a` is the byte at `a + i` on both
little-endian targets. -/
abbrev Vec16 := Fin 16 → Byte

/-! ## Scalar reference (the specification) -/

/-- Number of bytes equal to the delimiter (unsigned byte equality). -/
def refCount (data : List Byte) (delim : Byte) : Nat := data.count delim
/-- XOR of all bytes, starting from zero. -/
def refXor (data : List Byte) : Byte := data.foldl (· ^^^ ·) 0#8

/-! ## Cranelift operations used by the scanner -/

/-- `extractlane v i` for the constant lanes `0..15` the IR uses. -/
def lane (v : Vec16) (i : Nat) : Byte := v ⟨i % 16, Nat.mod_lt _ (by decide)⟩
def splat (b : Byte) : Vec16 := fun _ => b
/-- Unaligned `load.i8` (no `aligned`/`notrap` flags). -/
def load8 (mem : Mem) (a : Word) : Byte := mem a
/-- Unaligned `load.i8x16`. -/
def load16 (mem : Mem) (a : Word) : Vec16 := fun i => mem (a + BitVec.ofNat 64 i.val)
/-- Lane-wise `icmp eq` on `i8x16`: all-ones lane when equal, zero otherwise. -/
def icmpEqVec (x y : Vec16) : Vec16 :=
  fun i => if x i = y i then BitVec.allOnes 8 else 0#8
def bxorVec (x y : Vec16) : Vec16 := fun i => x i ^^^ y i
/-- Scalar `icmp eq` on `i8` yields an `i8` 1 or 0. -/
def icmpEqByte (x y : Byte) : Byte := if x = y then 1#8 else 0#8
def uextend64 (b : Byte) : Word := b.setWidth 64
/-- `band_imm(len, -16)`: the immediate is the sign-extended 64-bit `-16`. -/
def fullLen (len : Word) : Word := len &&& BitVec.ofInt 64 (-16)

/-! ## The scanner program -/

/-- One logged load: the loop offset, the effective address, and the kind. -/
inductive Read where
  | vector (offset addr : Word)
  | scalar (offset addr : Word)
  deriving Repr, DecidableEq

structure VState where
  off : Word
  count : Word
  acc : Vec16
  reads : List Read

structure SState where
  off : Word
  count : Word
  xor : Byte
  reads : List Read

/-- The 16 unrolled per-lane mask-count additions of `vector_body`. -/
def maskCount (count : Word) (eq : Vec16) : Word :=
  (List.range 16).foldl (fun c i => c + uextend64 (lane eq i &&& 1#8)) count

def vecBody (mem : Mem) (input : Word) (needle : Vec16) (s : VState) : VState :=
  let address := input + s.off
  let chunk := load16 mem address
  let eq := icmpEqVec chunk needle
  { off := s.off + 16#64
    count := maskCount s.count eq
    acc := bxorVec s.acc chunk
    reads := s.reads ++ [Read.vector s.off address] }

/-- A guarded `while` loop with explicit fuel. Each fuel unit runs one guard
test and, if it holds, one body. `exited` records that the guard failed. The
loops are built from core `Nat.repeat`, so this module has no recursive
definitions of its own (the trusted auditor rejects compiler `_unsafe_rec`
helpers); `runLoop_zero`/`runLoop_succ` give the usual recursion equations. -/
inductive LoopState (σ : Type) where
  | running (s : σ)
  | exited (s : σ)

def loopStep {σ : Type} (guard : σ → Bool) (body : σ → σ) : LoopState σ → LoopState σ
  | .running s => if guard s then .running (body s) else .exited s
  | .exited s => .exited s

/-- `none` means the fuel ran out while the guard still held. -/
def runLoop {σ : Type} (guard : σ → Bool) (body : σ → σ) (fuel : Nat) (s : σ) : Option σ :=
  match Nat.repeat (loopStep guard body) fuel (.running s) with
  | .running t => if guard t then none else some t
  | .exited t => some t

theorem repeat_succ' {α : Type} (f : α → α) (n : Nat) (a : α) :
    Nat.repeat f (n + 1) a = Nat.repeat f n (f a) := by
  induction n generalizing a with
  | zero => rfl
  | succ n ih => show f (Nat.repeat f (n + 1) a) = f (Nat.repeat f n (f a)); rw [ih]

theorem repeat_exited {σ : Type} (guard : σ → Bool) (body : σ → σ) (n : Nat) (s : σ) :
    Nat.repeat (loopStep guard body) n (.exited s) = .exited s := by
  induction n with
  | zero => rfl
  | succ n ih => show loopStep guard body (Nat.repeat _ n _) = _; rw [ih]; rfl

theorem runLoop_zero {σ : Type} (guard : σ → Bool) (body : σ → σ) (s : σ) :
    runLoop guard body 0 s = if guard s then none else some s := rfl

theorem runLoop_succ {σ : Type} (guard : σ → Bool) (body : σ → σ) (fuel : Nat) (s : σ) :
    runLoop guard body (fuel + 1) s =
      if guard s then runLoop guard body fuel (body s) else some s := by
  unfold runLoop
  rw [repeat_succ']
  by_cases h : guard s
  · simp only [loopStep, h, if_true]
  · have hf : guard s = false := by simpa using h
    simp only [loopStep, hf, Bool.false_eq_true, if_false, repeat_exited]

def vecLoop (mem : Mem) (input : Word) (needle : Vec16) (full : Word) :
    Nat → VState → Option VState :=
  runLoop (fun s => s.off.ult full) (vecBody mem input needle)

/-- The `reduce` block. -/
def reduceXor (acc : Vec16) : Byte :=
  (List.range 16).foldl (fun x i => x ^^^ lane acc i) 0#8

def scalarBody (mem : Mem) (input : Word) (delim : Byte) (s : SState) : SState :=
  let address := input + s.off
  let byte := load8 mem address
  { off := s.off + 1#64
    count := s.count + uextend64 (icmpEqByte byte delim)
    xor := s.xor ^^^ byte
    reads := s.reads ++ [Read.scalar s.off address] }

def scalarLoop (mem : Mem) (input : Word) (delim : Byte) (len : Word) :
    Nat → SState → Option SState :=
  runLoop (fun s => s.off.ult len) (scalarBody mem input delim)

structure Outcome where
  count : Word
  xor : Byte
  reads : List Read

/-- The whole scanner, `entry` to `finish`, with loop fuel `vf`/`sf`. -/
def scan (mem : Mem) (input len : Word) (delim : Byte) (vf sf : Nat) : Option Outcome := do
  let full := fullLen len
  let v ← vecLoop mem input (splat delim) full vf
    { off := 0#64, count := 0#64, acc := splat 0#8, reads := [] }
  let s ← scalarLoop mem input delim len sf
    { off := v.off, count := v.count, xor := reduceXor v.acc, reads := v.reads }
  pure { count := s.count, xor := s.xor, reads := s.reads }

/-! ## Assumptions and the read-legality predicate -/

/-- The memory holds `data` at `input`, and `len` is its length. -/
structure MemoryModel (mem : Mem) (input len : Word) (data : List Byte) : Prop where
  length : data.length = len.toNat
  bytes : ∀ i (h : i < data.length), mem (input + BitVec.ofNat 64 i) = data[i]

/-- The readable slice `[input, input + len)` does not wrap the address space. -/
def NoWrap (input len : Word) : Prop := input.toNat + len.toNat ≤ 2 ^ 64

/-- Offset-level legality: a full-width read lies entirely inside the logical
slice (no overread justified by padding); a tail read is one in-range byte. -/
def Read.Legal (input len : Word) : Read → Prop
  | .vector off addr => addr = input + off ∧ off.toNat + 16 ≤ len.toNat
  | .scalar off addr => addr = input + off ∧ off.toNat < len.toNat

/-- Address-level range of a read as natural numbers `[lo, hi)`. -/
def Read.range : Read → Nat × Nat
  | .vector _ addr => (addr.toNat, addr.toNat + 16)
  | .scalar _ addr => (addr.toNat, addr.toNat + 1)

/-! ## Basic lemmas -/

theorem fullLen_toNat (len : Word) : (fullLen len).toNat = len.toNat / 16 * 16 := by
  have hm : BitVec.ofInt 64 (-16) = ~~~(BitVec.ofNat 64 (2 ^ 4 - 1)) := by decide
  have h : fullLen len = (len >>> 4) <<< 4 := by
    apply BitVec.eq_of_getLsbD_eq
    intro i hi
    rw [fullLen, hm, BitVec.getLsbD_and, BitVec.getLsbD_not, BitVec.getLsbD_ofNat,
      Nat.testBit_two_pow_sub_one, BitVec.getLsbD_shiftLeft, BitVec.getLsbD_ushiftRight]
    by_cases h4 : i < 4
    · simp [h4]
    · have : 4 + (i - 4) = i := by omega
      simp [h4, hi, this]
  rw [h, BitVec.toNat_shiftLeft, BitVec.toNat_ushiftRight, Nat.shiftLeft_eq,
    Nat.shiftRight_eq_div_pow]
  have := len.isLt
  apply Nat.mod_eq_of_lt
  have : len.toNat / 2 ^ 4 * 2 ^ 4 ≤ len.toNat := Nat.div_mul_le_self _ _
  omega

/-- Per-lane mask conversion: `uextend(band(0xff, 1)) = 1`, `uextend(band(0, 1)) = 0`. -/
theorem mask_lane_bit (p : Prop) [Decidable p] :
    uextend64 ((if p then BitVec.allOnes 8 else 0#8) &&& 1#8) = if p then 1#64 else 0#64 := by
  by_cases h : p <;> simp only [h, if_true, if_false] <;> decide

theorem scalar_eq_bit (x y : Byte) :
    uextend64 (icmpEqByte x y) = if x = y then 1#64 else 0#64 := by
  unfold icmpEqByte
  by_cases h : x = y <;> simp only [h, if_true, if_false] <;> decide

theorem toNat_add_of_lt {a b : Word} (h : a.toNat + b.toNat < 2 ^ 64) :
    (a + b).toNat = a.toNat + b.toNat := by
  rw [BitVec.toNat_add]; exact Nat.mod_eq_of_lt h

theorem toNat_bit (p : Prop) [Decidable p] :
    (if p then 1#64 else 0#64).toNat = if p then 1 else 0 := by
  by_cases h : p <;> simp [h]

theorem xor_foldl_shift (x : Byte) (l : List Byte) :
    l.foldl (· ^^^ ·) x = x ^^^ l.foldl (· ^^^ ·) 0#8 := by
  induction l generalizing x with
  | nil => simp
  | cons b bs ih =>
    simp only [List.foldl_cons]
    rw [ih (x ^^^ b), ih (0#8 ^^^ b), BitVec.zero_xor, BitVec.xor_assoc]

theorem refXor_append (a b : List Byte) : refXor (a ++ b) = refXor a ^^^ refXor b := by
  unfold refXor; rw [List.foldl_append, xor_foldl_shift]

theorem refCount_append (a b : List Byte) (d : Byte) :
    refCount (a ++ b) d = refCount a d + refCount b d := by
  unfold refCount; exact List.count_append

theorem take_succ_of_lt {l : List Byte} {i : Nat} (h : i < l.length) :
    l.take (i + 1) = l.take i ++ [l[i]] := by
  rw [List.take_succ, List.getElem?_eq_getElem h]; rfl

theorem refCount_single (b d : Byte) : refCount [b] d = if b = d then 1 else 0 := by
  unfold refCount; by_cases h : b = d <;> simp [h]

theorem refXor_single (b : Byte) : refXor [b] = b := by
  simp [refXor]

theorem lane_lt {i : Nat} (h : i < 16) (v : Vec16) : lane v i = v ⟨i, h⟩ := by
  simp [lane, Nat.mod_eq_of_lt h]

/-- Bytes `off .. off + m` of the slice, read through the memory model. -/
theorem mem_lane {mem : Mem} {input len : Word} {data : List Byte}
    (hm : MemoryModel mem input len data) (off : Word) (i : Nat)
    (h : off.toNat + i < data.length) :
    mem (input + off + BitVec.ofNat 64 i) = data[off.toNat + i] := by
  have : input + off + BitVec.ofNat 64 i = input + BitVec.ofNat 64 (off.toNat + i) := by
    apply BitVec.eq_of_toNat_eq
    simp only [BitVec.toNat_add, BitVec.toNat_ofNat]
    omega
  rw [this]; exact hm.bytes _ h

/-! ## One vector block -/

/-- The first `m ≤ 16` unrolled mask additions add exactly the number of
matching bytes among the first `m` lanes, without wrapping. -/
theorem maskCount_prefix (c : Word) (chunk : Vec16) (d : Byte) (bytes : List Byte)
    (hlen : bytes.length = 16) (hlanes : ∀ i (h : i < 16), lane chunk i = bytes[i])
    (m : Nat) (hm : m ≤ 16) (hc : c.toNat + 16 < 2 ^ 64) :
    ((List.range m).foldl
        (fun c i => c + uextend64 (lane (icmpEqVec chunk (splat d)) i &&& 1#8)) c).toNat
      = c.toNat + refCount (bytes.take m) d := by
  induction m with
  | zero => simp [refCount]
  | succ m ih =>
    have hlt : m < 16 := by omega
    rw [List.range_succ, List.foldl_append, List.foldl_cons, List.foldl_nil,
      take_succ_of_lt (by omega), refCount_append]
    have hlane : lane (icmpEqVec chunk (splat d)) m
        = if bytes[m] = d then BitVec.allOnes 8 else 0#8 := by
      rw [lane_lt hlt, icmpEqVec, ← lane_lt hlt chunk, hlanes m hlt]; rfl
    rw [hlane, mask_lane_bit, refCount_single]
    have hprev := ih (by omega)
    have hbound : refCount (bytes.take m) d ≤ m := by
      unfold refCount
      have := List.count_le_length (a := d) (l := bytes.take m)
      simp [List.length_take] at this; omega
    rw [toNat_add_of_lt, hprev, toNat_bit]
    · omega
    · rw [hprev, toNat_bit]; split <;> omega

theorem maskCount_block (c : Word) (chunk : Vec16) (d : Byte) (bytes : List Byte)
    (hlen : bytes.length = 16) (hlanes : ∀ i (h : i < 16), lane chunk i = bytes[i])
    (hc : c.toNat + 16 < 2 ^ 64) :
    (maskCount c (icmpEqVec chunk (splat d))).toNat = c.toNat + refCount bytes d := by
  have := maskCount_prefix c chunk d bytes hlen hlanes 16 (by omega) hc
  rw [show bytes.take 16 = bytes by rw [← hlen]; exact List.take_length] at this
  exact this

def laneXor (m : Nat) (v : Vec16) : Byte :=
  (List.range m).foldl (fun x i => x ^^^ lane v i) 0#8

theorem laneXor_bxor (m : Nat) (a c : Vec16) :
    laneXor m (bxorVec a c) = laneXor m a ^^^ laneXor m c := by
  induction m with
  | zero => simp [laneXor]
  | succ m ih =>
    simp only [laneXor, List.range_succ, List.foldl_append, List.foldl_cons,
      List.foldl_nil] at ih ⊢
    rw [ih]
    have : lane (bxorVec a c) m = lane a m ^^^ lane c m := rfl
    rw [this]; ac_rfl

theorem laneXor_bytes (chunk : Vec16) (bytes : List Byte) (hlen : bytes.length = 16)
    (hlanes : ∀ i (h : i < 16), lane chunk i = bytes[i]) (m : Nat) (hm : m ≤ 16) :
    laneXor m chunk = refXor (bytes.take m) := by
  induction m with
  | zero => simp [laneXor, refXor]
  | succ m ih =>
    have hlt : m < 16 := by omega
    simp only [laneXor, List.range_succ, List.foldl_append, List.foldl_cons,
      List.foldl_nil] at ih ⊢
    rw [ih (by omega), take_succ_of_lt (by omega), refXor_append, refXor_single, hlanes m hlt]

/-- XOR composition: reducing the lane-wise accumulator after adding a block
equals the previous reduction XOR the block's bytes. -/
theorem reduceXor_block (acc chunk : Vec16) (bytes : List Byte) (hlen : bytes.length = 16)
    (hlanes : ∀ i (h : i < 16), lane chunk i = bytes[i]) :
    reduceXor (bxorVec acc chunk) = reduceXor acc ^^^ refXor bytes := by
  have h := laneXor_bytes chunk bytes hlen hlanes 16 (by omega)
  rw [show bytes.take 16 = bytes by rw [← hlen]; exact List.take_length] at h
  show laneXor 16 _ = laneXor 16 _ ^^^ _
  rw [laneXor_bxor, h]

/-! ## Loop invariants -/

/-- Vector-loop invariant after `j` blocks. -/
structure VInv (input len : Word) (data : List Byte) (d : Byte) (j : Nat) (s : VState) :
    Prop where
  off : s.off.toNat = 16 * j
  bound : 16 * j ≤ len.toNat / 16 * 16
  count : s.count.toNat = refCount (data.take (16 * j)) d
  xor : reduceXor s.acc = refXor (data.take (16 * j))
  reads : ∀ r ∈ s.reads, r.Legal input len

theorem refCount_take_le (data : List Byte) (d : Byte) (k : Nat) :
    refCount (data.take k) d ≤ k := by
  unfold refCount
  have := List.count_le_length (a := d) (l := data.take k)
  simp [List.length_take] at this; omega

theorem vecBody_inv {mem : Mem} {input len : Word} {data : List Byte} {d : Byte}
    (hm : MemoryModel mem input len data) {j : Nat} {s : VState}
    (inv : VInv input len data d j s) (hmore : 16 * j < len.toNat / 16 * 16) :
    VInv input len data d (j + 1) (vecBody mem input (splat d) s) := by
  have hn := len.isLt
  have hdl := hm.length
  have hfull : len.toNat / 16 * 16 ≤ len.toNat := Nat.div_mul_le_self _ _
  have hstep : 16 * j + 16 ≤ len.toNat / 16 * 16 := by omega
  let bytes := (data.drop (16 * j)).take 16
  have hblen : bytes.length = 16 := by simp [bytes, List.length_take]; omega
  have hlanes : ∀ i (h : i < 16),
      lane (load16 mem (input + s.off)) i = bytes[i] := by
    intro i h
    have hoff := inv.off
    rw [lane_lt h]
    simp only [load16, bytes, List.getElem_take, List.getElem_drop]
    rw [mem_lane hm s.off i (by omega)]
    simp [inv.off]
  have htake : data.take (16 * (j + 1)) = data.take (16 * j) ++ bytes := by
    rw [show 16 * (j + 1) = 16 * j + 16 by omega, List.take_add]
  have hcnt := refCount_take_le data d (16 * j)
  refine ⟨?_, ?_, ?_, ?_, ?_⟩
  · simp only [vecBody]; rw [toNat_add_of_lt] <;> simp [inv.off] <;> omega
  · omega
  · simp only [vecBody]
    rw [maskCount_block _ _ d bytes hblen hlanes (by rw [inv.count]; omega), inv.count,
      htake, refCount_append]
  · simp only [vecBody]
    rw [reduceXor_block _ _ bytes hblen hlanes, inv.xor, htake, refXor_append]
  · intro r hr
    simp only [vecBody, List.mem_append, List.mem_singleton] at hr
    rcases hr with hr | rfl
    · exact inv.reads r hr
    · exact ⟨rfl, by rw [inv.off]; omega⟩

theorem ult_iff_toNat {a b : Word} : a.ult b = true ↔ a.toNat < b.toNat := by
  rw [BitVec.ult_iff_lt, BitVec.lt_def]

theorem vecLoop_spec {mem : Mem} {input len : Word} {data : List Byte} {d : Byte}
    (hm : MemoryModel mem input len data) :
    ∀ (fuel j : Nat) (s : VState), VInv input len data d j s →
      len.toNat / 16 ≤ j + fuel →
      ∃ v, vecLoop mem input (splat d) (fullLen len) fuel s = some v ∧
        VInv input len data d (len.toNat / 16) v := by
  intro fuel
  induction fuel with
  | zero =>
    intro j s inv hf
    have hj : j = len.toNat / 16 := by have := inv.bound; omega
    subst hj
    refine ⟨s, ?_, inv⟩
    have : ¬ (s.off.ult (fullLen len) = true) := by
      rw [ult_iff_toNat, fullLen_toNat, inv.off]; omega
    simp [vecLoop, runLoop_zero, this]
  | succ fuel ih =>
    intro j s inv hf
    by_cases hmore : 16 * j < len.toNat / 16 * 16
    · have hu : s.off.ult (fullLen len) = true := by
        rw [ult_iff_toNat, fullLen_toNat, inv.off]; exact hmore
      simp only [vecLoop, runLoop_succ, hu, if_true]
      exact ih (j + 1) _ (vecBody_inv hm inv hmore) (by omega)
    · have hj : j = len.toNat / 16 := by have := inv.bound; omega
      subst hj
      have : ¬ (s.off.ult (fullLen len) = true) := by
        rw [ult_iff_toNat, fullLen_toNat, inv.off]; omega
      exact ⟨s, by simp [vecLoop, runLoop_succ, this], inv⟩

/-- Scalar-loop invariant after the vector loop and `k` tail bytes. -/
structure SInv (input len : Word) (data : List Byte) (d : Byte) (k : Nat) (s : SState) :
    Prop where
  off : s.off.toNat = len.toNat / 16 * 16 + k
  bound : k ≤ len.toNat % 16
  count : s.count.toNat = refCount (data.take (len.toNat / 16 * 16 + k)) d
  xor : s.xor = refXor (data.take (len.toNat / 16 * 16 + k))
  reads : ∀ r ∈ s.reads, r.Legal input len

theorem scalarBody_inv {mem : Mem} {input len : Word} {data : List Byte} {d : Byte}
    (hm : MemoryModel mem input len data) {k : Nat} {s : SState}
    (inv : SInv input len data d k s) (hmore : k < len.toNat % 16) :
    SInv input len data d (k + 1) (scalarBody mem input d s) := by
  have hn := len.isLt
  have hdl := hm.length
  have hdiv := Nat.div_add_mod len.toNat 16
  have hpos : len.toNat / 16 * 16 + k < data.length := by omega
  have hbyte : load8 mem (input + s.off) = data[len.toNat / 16 * 16 + k] := by
    have := mem_lane hm s.off 0 (by rw [inv.off]; omega)
    simp only [BitVec.add_zero, Nat.add_zero] at this
    simp only [load8, this, inv.off]
  have htake : data.take (len.toNat / 16 * 16 + (k + 1))
      = data.take (len.toNat / 16 * 16 + k) ++ [data[len.toNat / 16 * 16 + k]] := by
    rw [← Nat.add_assoc]; exact take_succ_of_lt hpos
  have hcnt := refCount_take_le data d (len.toNat / 16 * 16 + k)
  refine ⟨?_, ?_, ?_, ?_, ?_⟩
  · simp only [scalarBody]; rw [toNat_add_of_lt] <;> simp [inv.off] <;> omega
  · omega
  · simp only [scalarBody]
    rw [hbyte, scalar_eq_bit, htake, refCount_append, refCount_single, toNat_add_of_lt,
      inv.count, toNat_bit]
    rw [inv.count, toNat_bit]; split <;> omega
  · simp only [scalarBody]
    rw [hbyte, htake, refXor_append, refXor_single, inv.xor]
  · intro r hr
    simp only [scalarBody, List.mem_append, List.mem_singleton] at hr
    rcases hr with hr | rfl
    · exact inv.reads r hr
    · exact ⟨rfl, by rw [inv.off]; omega⟩

theorem scalarLoop_spec {mem : Mem} {input len : Word} {data : List Byte} {d : Byte}
    (hm : MemoryModel mem input len data) :
    ∀ (fuel k : Nat) (s : SState), SInv input len data d k s →
      len.toNat % 16 ≤ k + fuel →
      ∃ t, scalarLoop mem input d len fuel s = some t ∧
        SInv input len data d (len.toNat % 16) t := by
  have hdiv := Nat.div_add_mod len.toNat 16
  intro fuel
  induction fuel with
  | zero =>
    intro k s inv hf
    have hk : k = len.toNat % 16 := by have := inv.bound; omega
    subst hk
    have : ¬ (s.off.ult len = true) := by rw [ult_iff_toNat, inv.off]; omega
    exact ⟨s, by simp [scalarLoop, runLoop_zero, this], inv⟩
  | succ fuel ih =>
    intro k s inv hf
    by_cases hmore : k < len.toNat % 16
    · have hu : s.off.ult len = true := by rw [ult_iff_toNat, inv.off]; omega
      simp only [scalarLoop, runLoop_succ, hu, if_true]
      exact ih (k + 1) _ (scalarBody_inv hm inv hmore) (by omega)
    · have hk : k = len.toNat % 16 := by have := inv.bound; omega
      subst hk
      have : ¬ (s.off.ult len = true) := by rw [ult_iff_toNat, inv.off]; omega
      exact ⟨s, by simp [scalarLoop, runLoop_succ, this], inv⟩

/-! ## Main theorems -/

/-- **Equivalence, counts and XOR, whole block + tail loop.** For an arbitrary
byte sequence, delimiter and base address, with any fuel at least the exact
iteration counts, the modeled scanner terminates and returns the scalar
reference's count (as a non-wrapping `u64`) and XOR. Every logged read is legal. -/
theorem scan_correct {mem : Mem} {input len : Word} {data : List Byte} (d : Byte)
    (hm : MemoryModel mem input len data) (vf sf : Nat)
    (hvf : len.toNat / 16 ≤ vf) (hsf : len.toNat % 16 ≤ sf) :
    ∃ o, scan mem input len d vf sf = some o ∧
      o.count.toNat = refCount data d ∧
      o.count = BitVec.ofNat 64 (refCount data d) ∧
      refCount data d < 2 ^ 64 ∧
      o.xor = refXor data ∧
      (∀ r ∈ o.reads, r.Legal input len) := by
  have hdl := hm.length
  have hdiv := Nat.div_add_mod len.toNat 16
  have hlenlt := len.isLt
  have v0 : VInv input len data d 0 { off := 0#64, count := 0#64, acc := splat 0#8, reads := [] } := by
    refine ⟨by simp, by omega, by simp [refCount], ?_, by simp⟩
    simp [reduceXor, refXor, lane, splat, List.range_succ]
  obtain ⟨v, hv, vinv⟩ := vecLoop_spec hm vf 0 _ v0 (by omega)
  have s0 : SInv input len data d 0
      { off := v.off, count := v.count, xor := reduceXor v.acc, reads := v.reads } := by
    refine ⟨?_, by omega, ?_, ?_, vinv.reads⟩
    · simp only [vinv.off]; omega
    · simp only [vinv.count]; congr 2; omega
    · simp only [vinv.xor]; congr 2; omega
  obtain ⟨t, ht, tinv⟩ := scalarLoop_spec hm sf 0 _ s0 (by omega)
  have hall : data.take (len.toNat / 16 * 16 + len.toNat % 16) = data := by
    rw [List.take_of_length_le]; omega
  have hcount : t.count.toNat = refCount data d := by rw [tinv.count, hall]
  have hle : refCount data d ≤ data.length := by
    unfold refCount; exact List.count_le_length
  refine ⟨{ count := t.count, xor := t.xor, reads := t.reads }, ?_, hcount, ?_, by omega,
    by rw [tinv.xor, hall], tinv.reads⟩
  · simp [scan, hv, ht]
  · apply BitVec.eq_of_toNat_eq; rw [hcount, BitVec.toNat_ofNat, Nat.mod_eq_of_lt (by omega)]

/-- With exactly `len / 16` vector and `len % 16` scalar iterations of fuel the
run completes; fuel is only a totality device. -/
theorem scan_exact_fuel {mem : Mem} {input len : Word} {data : List Byte} (d : Byte)
    (hm : MemoryModel mem input len data) :
    ∃ o, scan mem input len d (len.toNat / 16) (len.toNat % 16) = some o ∧
      o.count.toNat = refCount data d ∧ o.xor = refXor data :=
  let ⟨o, h, hc, _, _, hx, _⟩ := scan_correct d hm _ _ (Nat.le_refl _) (Nat.le_refl _)
  ⟨o, h, hc, hx⟩

/-- Too little vector fuel is never reported as success (the guard still holds). -/
theorem vecLoop_insufficient_fuel {mem : Mem} {input len : Word} {data : List Byte} {d : Byte}
    (hm : MemoryModel mem input len data) :
    ∀ (fuel j : Nat) (s : VState), VInv input len data d j s →
      j + fuel < len.toNat / 16 →
      vecLoop mem input (splat d) (fullLen len) fuel s = none := by
  intro fuel
  induction fuel with
  | zero =>
    intro j s inv hf
    have hu : s.off.ult (fullLen len) = true := by
      rw [ult_iff_toNat, fullLen_toNat, inv.off]; omega
    simp [vecLoop, runLoop_zero, hu]
  | succ fuel ih =>
    intro j s inv hf
    have hmore : 16 * j < len.toNat / 16 * 16 := by omega
    have hu : s.off.ult (fullLen len) = true := by
      rw [ult_iff_toNat, fullLen_toNat, inv.off]; exact hmore
    simp only [vecLoop, runLoop_succ, hu, if_true]
    exact ih (j + 1) _ (vecBody_inv hm inv hmore) (by omega)

/-! ## Concrete bounds lemmas -/

/-- Full-width reads: the block's offset range `[off, off + 16)` is inside the
logical slice `[0, len)` and its offset is a multiple of 16 below `len / 16 * 16`
(implied by legality plus the invariant). -/
theorem vector_read_offsets {input len off addr : Word}
    (h : (Read.vector off addr).Legal input len) :
    off.toNat + 16 ≤ len.toNat ∧ addr = input + off := ⟨h.2, h.1⟩

/-- Address-level bounds: under `NoWrap`, every legal read's byte range is a
subrange of `[input, input + len)` with no address wrap-around, and each lane
`i < 16` of a vector read is the byte at natural address `addr + i`. -/
theorem legal_read_in_slice {input len : Word} (hw : NoWrap input len) (r : Read)
    (h : r.Legal input len) :
    input.toNat ≤ r.range.1 ∧ r.range.2 ≤ input.toNat + len.toNat := by
  unfold NoWrap at hw
  cases r with
  | vector off addr =>
    obtain ⟨ha, hl⟩ := h
    subst ha
    simp only [Read.range]
    rw [toNat_add_of_lt (by omega)]; omega
  | scalar off addr =>
    obtain ⟨ha, hl⟩ := h
    subst ha
    simp only [Read.range]
    rw [toNat_add_of_lt (by omega)]; omega

theorem vector_lane_address {input len off addr : Word} (hw : NoWrap input len)
    (h : (Read.vector off addr).Legal input len) (i : Nat) (hi : i < 16) :
    (addr + BitVec.ofNat 64 i).toNat = input.toNat + off.toNat + i ∧
      input.toNat + off.toNat + i < input.toNat + len.toNat := by
  unfold NoWrap at hw
  obtain ⟨ha, hl⟩ := h
  subst ha
  have hi' : i < 2 ^ 64 := by omega
  rw [toNat_add_of_lt, toNat_add_of_lt, BitVec.toNat_ofNat, Nat.mod_eq_of_lt hi']
  · omega
  · omega
  · rw [toNat_add_of_lt (by omega), BitVec.toNat_ofNat, Nat.mod_eq_of_lt hi']; omega

/-- All reads of a whole run are within the slice. -/
theorem scan_reads_in_slice {mem : Mem} {input len : Word} {data : List Byte} (d : Byte)
    (hm : MemoryModel mem input len data) (hw : NoWrap input len) (vf sf : Nat)
    (hvf : len.toNat / 16 ≤ vf) (hsf : len.toNat % 16 ≤ sf) :
    ∃ o, scan mem input len d vf sf = some o ∧
      ∀ r ∈ o.reads, input.toNat ≤ r.range.1 ∧ r.range.2 ≤ input.toNat + len.toNat :=
  let ⟨o, h, _, _, _, _, hr⟩ := scan_correct d hm vf sf hvf hsf
  ⟨o, h, fun r hmem => legal_read_in_slice hw r (hr r hmem)⟩

/-- Zero length: no reads, zero count, zero XOR. -/
theorem scan_zero_length (mem : Mem) (input : Word) (d : Byte) (vf sf : Nat) :
    scan mem input 0#64 d vf sf = some { count := 0#64, xor := 0#8, reads := [] } := by
  have hf : (fullLen 0#64) = 0#64 := by decide
  have h0 : ¬ ((0#64).ult (0#64) = true) := by decide
  have hr : reduceXor (splat 0#8) = 0#8 := by
    simp [reduceXor, lane, splat, List.range_succ]
  cases vf <;> cases sf <;> simp [scan, vecLoop, scalarLoop, runLoop_zero, runLoop_succ, hf, h0, hr]

/-- Every remainder size `len % 16 ∈ [0, 16)` is a legal tail: the vector part
covers exactly `len / 16` blocks and the scalar tail exactly `len % 16` bytes. -/
theorem block_tail_decomposition (len : Word) :
    (fullLen len).toNat + len.toNat % 16 = len.toNat ∧ len.toNat % 16 < 16 ∧
      (fullLen len).toNat % 16 = 0 := by
  rw [fullLen_toNat]; omega

/-- Spec-level decomposition: the reference over the whole slice is the
reference over the `len / 16` full blocks composed with the tail. -/
theorem reference_block_tail (data : List Byte) (d : Byte) :
    refCount data d = refCount (data.take (data.length / 16 * 16)) d
        + refCount (data.drop (data.length / 16 * 16)) d ∧
      refXor data = refXor (data.take (data.length / 16 * 16))
        ^^^ refXor (data.drop (data.length / 16 * 16)) := by
  constructor
  · rw [← refCount_append, List.take_append_drop]
  · rw [← refXor_append, List.take_append_drop]

/-! ## Executable fixture semantics

`lean/SimdScanner/Fixture.lean` drives `runFixture` from stdin for the Rust
differential test `crates/vesper-simd/tests/model.rs`. It evaluates the
*modeled machine program* `scan`, not the reference. The IO driver lives in that
separate file so this audited module contains no `partial`/`unsafe` code. -/

/-- Memory holding `bytes` at `base`; addresses outside read as `0xAA`, which a
correct in-bounds run never observes. -/
def fixtureMem (bytes : Array Byte) (base : Word) : Mem := fun a =>
  let i := (a - base).toNat
  if h : i < bytes.size then bytes[i] else 0xAA#8

def runFixture (base : Word) (d : Byte) (bytes : Array Byte) : Option Outcome :=
  let len := BitVec.ofNat 64 bytes.size
  scan (fixtureMem bytes base) base len d (bytes.size / 16) (bytes.size % 16)

end Vesper.SimdScanner

#print axioms Vesper.SimdScanner.scan_correct
#print axioms Vesper.SimdScanner.scan_exact_fuel
#print axioms Vesper.SimdScanner.vecLoop_insufficient_fuel
#print axioms Vesper.SimdScanner.scan_reads_in_slice
#print axioms Vesper.SimdScanner.vector_lane_address
#print axioms Vesper.SimdScanner.scan_zero_length
#print axioms Vesper.SimdScanner.block_tail_decomposition
#print axioms Vesper.SimdScanner.reference_block_tail
#print axioms Vesper.SimdScanner.fullLen_toNat
#print axioms Vesper.SimdScanner.legal_read_in_slice
#print axioms Vesper.SimdScanner.mask_lane_bit
#print axioms Vesper.SimdScanner.maskCount_block
#print axioms Vesper.SimdScanner.reduceXor_block

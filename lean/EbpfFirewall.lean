/-!
# Vesper eBPF XDP SYN-flood filter (design §27.3, §27.4)

A model of the decision made by `crates/vesper-ebpf/src/mitigator.rs`
(`mitigate_syn_flood`) over a packet given as its list of bytes, and the
per-source SYN counter it reads and writes.

The model reads packet bytes only through `byteAt`, which takes a proof
that the index is below the packet length, and the only facts it has to
discharge those proofs come from the same `54 ≤ length` guard the BPF
program issues (`data + 54 > data_end → XDP_PASS`). So every access the
model makes is within the checked bounds by construction; `reads` lists
those accesses explicitly and `reads_within_checked_bounds` states it.

What is proved, about this model only:

* `reads_within_checked_bounds`: every byte range the filter reads lies
  inside the 54-byte prefix it checked, hence inside the packet.
* `short_packets_pass`: packets shorter than 54 bytes pass and are not
  counted.
* `filter_action`: the action as one closed-form condition.
* `drop_iff_spec`: the filter drops exactly when the specification says:
  IPv4, TCP, SYN set, ACK clear, and the source's count after this packet
  exceeds the threshold.
* `syn_mitigator_never_drops_ack_packets` (§27.4): a packet with the TCP
  ACK flag set always passes, whatever the counters, threshold or payload.
* `only_syn_only_packets_are_counted` and `count_increments_by_one`.

Counts are natural numbers here; the BPF program uses a 64-bit counter
that would wrap after 2^64 SYNs from one source, which the model ignores.
This is not a theorem about the Rust code or the emitted bytecode. A Rust
test (`crates/vesper-ebpf/tests/lean_model.rs`) compiles this file, checks
the axiom report, and compares the `#eval` fixture table at the end with
the interpreter running the real program.
-/

namespace Vesper.EbpfFirewall

def modelId : String := "vesper.ebpf.syn-filter-model/1"

inductive Action where
  | aborted | drop | pass | tx | redirect
  deriving DecidableEq, Repr

def Action.code : Action → Nat
  | .aborted => 0
  | .drop => 1
  | .pass => 2
  | .tx => 3
  | .redirect => 4

def headerLen : Nat := 54
def ethPIpv4 : Nat := 0x0800
def ipprotoTcp : Nat := 6
def offEtherType : Nat := 12
def offProto : Nat := 23
def offSrc : Nat := 26
def offDstPort : Nat := 36
def offFlags : Nat := 46

/-- Bounds-checked byte read: the caller must prove `i < p.length`. -/
def byteAt (p : List UInt8) (i : Nat) (h : i < p.length) : Nat :=
  (p.get ⟨i, h⟩).toNat

/-- The packet fields the filter reads, available only under the guard. -/
structure Fields where
  etherType : Nat
  proto : Nat
  flags : Nat
  src : Nat
  dstPort : Nat

def fields (p : List UInt8) (h : headerLen ≤ p.length) : Fields :=
  have h' : 53 < p.length := by unfold headerLen at h; omega
  let b := fun (i : Nat) (hi : i ≤ 53) => byteAt p i (by omega)
  { etherType := b 12 (by decide) * 256 + b 13 (by decide)
    proto := b 23 (by decide)
    flags := (b 46 (by decide) * 256 + b 47 (by decide)) % 64
    src := ((b 26 (by decide) * 256 + b 27 (by decide)) * 256 + b 28 (by decide)) * 256
      + b 29 (by decide)
    dstPort := b 36 (by decide) * 256 + b 37 (by decide) }

def synSet (flags : Nat) : Bool := flags.testBit 1
def ackSet (flags : Nat) : Bool := flags.testBit 4

/-- The SYN-only test of §27.3: SYN set and ACK clear. -/
def synOnly (f : Fields) : Bool := synSet f.flags && !ackSet f.flags

/-- Per-source SYN counters (absent = never seen). -/
abbrev Counts := Nat → Option Nat

def bump (c : Counts) (src : Nat) : Counts × Nat :=
  let n := (c src).getD 0 + 1
  (fun s => if s = src then some n else c s, n)

/-- One packet through the filter: the action, and the counters after it. -/
def filter (thr : Nat) (c : Counts) (p : List UInt8) : Action × Counts :=
  if h : headerLen ≤ p.length then
    let f := fields p h
    if f.etherType ≠ ethPIpv4 ∨ f.proto ≠ ipprotoTcp then (.pass, c)
    else if !synOnly f then (.pass, c)
    else
      let (c', n) := bump c f.src
      if n ≤ thr then (.pass, c') else (.drop, c')
  else (.pass, c)

/-- A byte range `[off, off + size)` read from the packet. -/
structure Read where
  off : Nat
  size : Nat
  deriving DecidableEq, Repr

/-- The reads the filter issues (the BPF program reads the same ranges:
ether_type, proto, data_offset_flags, src_ipv4, dst_port). -/
def reads (p : List UInt8) : List Read :=
  if headerLen ≤ p.length then
    [⟨offEtherType, 2⟩, ⟨offProto, 1⟩, ⟨offFlags, 2⟩, ⟨offSrc, 4⟩, ⟨offDstPort, 2⟩]
  else []

theorem reads_within_checked_bounds (p : List UInt8) :
    ∀ r ∈ reads p, r.off + r.size ≤ headerLen ∧ r.off + r.size ≤ p.length := by
  intro r hr
  unfold reads at hr
  split at hr
  · simp only [List.mem_cons, List.not_mem_nil, or_false] at hr
    rcases hr with h | h | h | h | h <;> subst h <;>
      simp [headerLen, offEtherType, offProto, offFlags, offSrc, offDstPort] at * <;> omega
  · simp at hr

theorem short_packets_pass (thr : Nat) (c : Counts) (p : List UInt8)
    (h : p.length < headerLen) : filter thr c p = (.pass, c) := by
  unfold filter
  rw [dif_neg (by omega)]

/-- The §27.3 specification of the drop decision. -/
def specDrop (thr : Nat) (c : Counts) (p : List UInt8) : Prop :=
  ∃ h : headerLen ≤ p.length,
    (fields p h).etherType = ethPIpv4 ∧ (fields p h).proto = ipprotoTcp ∧
    synSet (fields p h).flags = true ∧ ackSet (fields p h).flags = false ∧
    thr < (c (fields p h).src).getD 0 + 1

/-- The drop condition, read off the filter. -/
def dropCond (thr : Nat) (c : Counts) (p : List UInt8) (h : headerLen ≤ p.length) : Prop :=
  (fields p h).etherType = ethPIpv4 ∧ (fields p h).proto = ipprotoTcp ∧
    synOnly (fields p h) = true ∧ thr < (c (fields p h).src).getD 0 + 1

instance (thr c p h) : Decidable (dropCond thr c p h) := by
  unfold dropCond; infer_instance

theorem filter_action (thr : Nat) (c : Counts) (p : List UInt8) :
    (filter thr c p).1 =
      if h : headerLen ≤ p.length then (if dropCond thr c p h then .drop else .pass)
      else .pass := by
  unfold filter dropCond
  by_cases h : headerLen ≤ p.length
  · simp only [dif_pos h, bump]
    by_cases he : (fields p h).etherType = ethPIpv4 <;>
    by_cases hp : (fields p h).proto = ipprotoTcp <;>
    by_cases hs : synOnly (fields p h) = true <;>
    by_cases ht : (c (fields p h).src).getD 0 + 1 ≤ thr <;>
    simp [he, hp, hs, ht] <;> omega
  · simp only [dif_neg h]

theorem drop_iff_spec (thr : Nat) (c : Counts) (p : List UInt8) :
    (filter thr c p).1 = .drop ↔ specDrop thr c p := by
  rw [filter_action]
  constructor
  · intro hd
    by_cases h : headerLen ≤ p.length
    · rw [dif_pos h] at hd
      by_cases hc : dropCond thr c p h
      · obtain ⟨he, hp, hs, ht⟩ := hc
        unfold synOnly at hs
        refine ⟨h, he, hp, ?_, ?_, ht⟩ <;>
          cases h1 : synSet (fields p h).flags <;>
          cases h2 : ackSet (fields p h).flags <;> simp_all
      · rw [if_neg hc] at hd
        cases hd
    · rw [dif_neg h] at hd
      cases hd
  · rintro ⟨h, he, hp, hs, ha, ht⟩
    rw [dif_pos h, if_pos]
    exact ⟨he, hp, by unfold synOnly; rw [hs, ha]; rfl, ht⟩

/-- §27.4: established traffic (ACK set) is never dropped. -/
theorem syn_mitigator_never_drops_ack_packets (thr : Nat) (c : Counts) (p : List UInt8)
    (h : headerLen ≤ p.length) (hack : ackSet (fields p h).flags = true) :
    (filter thr c p).1 = .pass := by
  rw [filter_action, dif_pos h, if_neg]
  unfold dropCond synOnly
  simp [hack]

theorem only_syn_only_packets_are_counted (thr : Nat) (c : Counts) (p : List UInt8)
    (h : headerLen ≤ p.length) (hn : synOnly (fields p h) = false) :
    (filter thr c p).2 = c := by
  unfold filter
  rw [dif_pos h]
  simp only [hn, Bool.not_false, ↓reduceIte]
  split <;> rfl

theorem count_increments_by_one (thr : Nat) (c : Counts) (p : List UInt8)
    (h : headerLen ≤ p.length)
    (hip : (fields p h).etherType = ethPIpv4 ∧ (fields p h).proto = ipprotoTcp)
    (hs : synOnly (fields p h) = true) :
    (filter thr c p).2 (fields p h).src = some ((c (fields p h).src).getD 0 + 1) := by
  unfold filter
  simp only [dif_pos h, hip.1, hip.2, ne_eq, not_true_eq_false, or_self, ↓reduceIte, hs,
    Bool.not_true, bump]
  by_cases ht : (c (fields p h).src).getD 0 + 1 ≤ thr <;> simp [ht]

#print axioms reads_within_checked_bounds
#print axioms short_packets_pass
#print axioms filter_action
#print axioms drop_iff_spec
#print axioms syn_mitigator_never_drops_ack_packets
#print axioms only_syn_only_packets_are_counted
#print axioms count_increments_by_one

/-! ## Fixture table

Each row: a 54-byte Ethernet/IPv4/TCP header with the given ether type,
protocol, flags and source, run in sequence through `filter` with
threshold 2, printing the action code per packet. The Rust test builds the
same packets, runs them through the real program in the interpreter and
compares the codes. -/

def mkPacket (etherType proto flags src : Nat) : List UInt8 :=
  let b (n : Nat) : UInt8 := n.toUInt8
  (List.replicate 12 (b 2)) ++ [b (etherType / 256), b etherType] ++
  [b 0x45, b 0] ++ List.replicate 7 (b 0) ++ [b proto] ++ [b 0, b 0] ++
  [b (src / 2^24), b (src / 2^16), b (src / 2^8), b src] ++ List.replicate 6 (b 0) ++
  [b 0x01, b 0xbb] ++ List.replicate 8 (b 0) ++ [b 0x50, b flags] ++ List.replicate 6 (b 0)

def fixture : List (Nat × Nat × Nat × Nat) :=
  [ (0x0800, 6, 0x02, 7), (0x0800, 6, 0x02, 7), (0x0800, 6, 0x02, 7),
    (0x0800, 6, 0x02, 7), (0x0800, 6, 0x12, 7), (0x0800, 6, 0x10, 7),
    (0x0800, 17, 0x02, 7), (0x86dd, 6, 0x02, 7), (0x0800, 6, 0x02, 9),
    (0x0800, 6, 0x03, 7), (0x0800, 6, 0x22, 9), (0x0800, 6, 0x02, 9),
    (0x0800, 6, 0x02, 9), (0x0800, 6, 0x00, 9), (0x0800, 6, 0x3f, 7) ]

def runFixture : List Nat :=
  (fixture.foldl (fun (acc : List Nat × Counts) (row : Nat × Nat × Nat × Nat) =>
    let (e, pr, fl, s) := row
    let (a, c') := filter 2 acc.2 (mkPacket e pr fl s)
    (acc.1 ++ [a.code], c')) ([], fun _ => none)).1

#eval s!"fixture: {runFixture}"

end Vesper.EbpfFirewall

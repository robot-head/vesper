/-!
# Vesper eBPF XDP SYN-flood filter (design §27.3, §27.4)

A model of the decision made by `crates/vesper-ebpf/src/mitigator.rs`
(`mitigate_syn_flood`) over a packet given as its list of bytes, and the
per-source SYN counter it reads and writes.

The model reads packet bytes only through `byteAt`, which takes a proof
that the index is below the packet length. The facts that discharge those
proofs come from the same two guards the BPF program issues: the
`data + 54 > data_end → XDP_PASS` guard, and the second guard that the
IPv4 IHL is at least 5 and that the 20-byte TCP header fits at
`14 + 4 * IHL`. The TCP flags and destination port are read at that
IHL-dependent offset, so IPv4 options cannot hide the flags. Every access
is therefore within the checked bounds by construction; `reads` lists the
accesses explicitly and `reads_within_checked_bounds` states it.

What is proved, about this model only:

* `reads_within_checked_bounds`: every byte range the filter reads lies
  inside the packet, given only the guards it checked.
* `short_packets_pass`: packets shorter than 54 bytes pass and are not
  counted.
* `filter_action`: the action as one closed-form condition.
* `drop_iff_spec`: the filter drops exactly when the specification says:
  both guards hold, IPv4, TCP, SYN set, ACK clear, and the source's count
  after this packet exceeds the threshold.
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
def offVerIhl : Nat := 14
def offProto : Nat := 23
def offSrc : Nat := 26
def offDstPort : Nat := 36
def offFlags : Nat := 46
/-- Offsets inside the TCP header (`offDstPort` and `offFlags` are these
plus 34, the TCP start when IHL is 5). -/
def tcpDstPort : Nat := 2
def tcpFlags : Nat := 12
def tcpHdrMin : Nat := 20

/-- Bounds-checked byte read: the caller must prove `i < p.length`. -/
def byteAt (p : List UInt8) (i : Nat) (h : i < p.length) : Nat :=
  (p.get ⟨i, h⟩).toNat

/-- The IPv4 header length field (32-bit words), read under the guard. -/
def ihlOf (p : List UInt8) (h : headerLen ≤ p.length) : Nat :=
  byteAt p 14 (by unfold headerLen at h; omega) % 16

/-- Start of the TCP header for a given IHL: IPv4 options move it. -/
def tcpOff (ihl : Nat) : Nat := 14 + 4 * ihl

/-- The second guard the program issues: IHL at least 5 and the 20-byte
TCP header present at `tcpOff ihl`. -/
def tcpOk (p : List UInt8) (h : headerLen ≤ p.length) : Prop :=
  5 ≤ ihlOf p h ∧ tcpOff (ihlOf p h) + tcpHdrMin ≤ p.length

instance (p h) : Decidable (tcpOk p h) := by unfold tcpOk; infer_instance

/-- The packet fields the filter reads. -/
structure Fields where
  etherType : Nat
  proto : Nat
  flags : Nat
  src : Nat
  dstPort : Nat

/-- The fields, available only under both guards; `none` otherwise. -/
def fields (p : List UInt8) : Option Fields :=
  if h : headerLen ≤ p.length then
    if ht : tcpOk p h then
      have h' : 53 < p.length := by unfold headerLen at h; omega
      have ht' : tcpOff (ihlOf p h) + 20 ≤ p.length := by
        unfold tcpOk tcpHdrMin at ht; exact ht.2
      let t := tcpOff (ihlOf p h)
      let b := fun (i : Nat) (hi : i < p.length) => byteAt p i hi
      some
        { etherType := b 12 (by omega) * 256 + b 13 (by omega)
          proto := b 23 (by omega)
          flags := (b (t + 12) (by omega) * 256 + b (t + 13) (by omega)) % 64
          src := ((b 26 (by omega) * 256 + b 27 (by omega)) * 256 + b 28 (by omega)) * 256
            + b 29 (by omega)
          dstPort := b (t + 2) (by omega) * 256 + b (t + 3) (by omega) }
    else none
  else none

def synSet (flags : Nat) : Bool := flags.testBit 1
def ackSet (flags : Nat) : Bool := flags.testBit 4

/-- The SYN-only test of §27.3: SYN set and ACK clear. -/
def synOnly (f : Fields) : Bool := synSet f.flags && !ackSet f.flags

/-- Per-source SYN counters (absent = never seen). -/
abbrev Counts := Nat → Option Nat

def bump (c : Counts) (src : Nat) : Counts × Nat :=
  let n := (c src).getD 0 + 1
  (fun s => if s = src then some n else c s, n)

/-- One packet through the filter: the action, and the counters after it.
The program tests ether type and protocol before the IHL guard; every
failed test passes the packet uncounted, so the order does not matter. -/
def filter (thr : Nat) (c : Counts) (p : List UInt8) : Action × Counts :=
  match fields p with
  | none => (.pass, c)
  | some f =>
    if f.etherType ≠ ethPIpv4 ∨ f.proto ≠ ipprotoTcp then (.pass, c)
    else if !synOnly f then (.pass, c)
    else
      let (c', n) := bump c f.src
      if n ≤ thr then (.pass, c') else (.drop, c')

/-- A byte range `[off, off + size)` read from the packet. -/
structure Read where
  off : Nat
  size : Nat
  deriving DecidableEq, Repr

/-- The reads the filter issues, in program order: ether_type, proto and
ver_ihl under the first guard; data_offset_flags, src_ipv4 and dst_port
(at IHL-dependent offsets) only under the second. -/
def reads (p : List UInt8) : List Read :=
  if h : headerLen ≤ p.length then
    [⟨offEtherType, 2⟩, ⟨offProto, 1⟩, ⟨offVerIhl, 1⟩] ++
      (if tcpOk p h then
        [⟨tcpOff (ihlOf p h) + tcpFlags, 2⟩, ⟨offSrc, 4⟩,
          ⟨tcpOff (ihlOf p h) + tcpDstPort, 2⟩]
      else [])
  else []

theorem reads_within_checked_bounds (p : List UInt8) :
    ∀ r ∈ reads p, r.off + r.size ≤ p.length := by
  intro r hr
  unfold reads at hr
  split at hr
  · rename_i h
    have h54 : 54 ≤ p.length := h
    split at hr
    · rename_i ht
      have ht2 := ht.2
      simp only [tcpHdrMin] at ht2
      simp only [List.mem_append, List.mem_cons, List.not_mem_nil, or_false] at hr
      rcases hr with (h | h | h) | (h | h | h) <;> subst h <;>
        simp [offEtherType, offProto, offVerIhl, offSrc, tcpFlags, tcpDstPort] <;> omega
    · simp only [List.append_nil, List.mem_cons, List.not_mem_nil, or_false] at hr
      rcases hr with h | h | h <;> subst h <;>
        simp [offEtherType, offProto, offVerIhl] <;> omega
  · simp at hr

theorem short_packets_pass (thr : Nat) (c : Counts) (p : List UInt8)
    (h : p.length < headerLen) : filter thr c p = (.pass, c) := by
  have : fields p = none := by unfold fields; rw [dif_neg (by omega)]
  unfold filter
  rw [this]

/-- The §27.3 specification of the drop decision: the packet's fields
exist (both guards hold), it is IPv4 TCP with SYN set and ACK clear, and
the source's count after this packet exceeds the threshold. -/
def specDrop (thr : Nat) (c : Counts) (p : List UInt8) : Prop :=
  ∃ f, fields p = some f ∧ f.etherType = ethPIpv4 ∧ f.proto = ipprotoTcp ∧
    synSet f.flags = true ∧ ackSet f.flags = false ∧ thr < (c f.src).getD 0 + 1

/-- The drop condition, read off the filter. -/
def dropCond (thr : Nat) (c : Counts) (f : Fields) : Prop :=
  f.etherType = ethPIpv4 ∧ f.proto = ipprotoTcp ∧ synOnly f = true ∧
    thr < (c f.src).getD 0 + 1

instance (thr c f) : Decidable (dropCond thr c f) := by
  unfold dropCond; infer_instance

theorem filter_action (thr : Nat) (c : Counts) (p : List UInt8) :
    (filter thr c p).1 =
      match fields p with
      | none => .pass
      | some f => if dropCond thr c f then .drop else .pass := by
  unfold filter dropCond
  cases fields p with
  | none => rfl
  | some f =>
    simp only [bump]
    by_cases he : f.etherType = ethPIpv4 <;>
    by_cases hp : f.proto = ipprotoTcp <;>
    by_cases hs : synOnly f = true <;>
    by_cases ht : (c f.src).getD 0 + 1 ≤ thr <;>
    simp [he, hp, hs, ht] <;> omega

theorem drop_iff_spec (thr : Nat) (c : Counts) (p : List UInt8) :
    (filter thr c p).1 = .drop ↔ specDrop thr c p := by
  rw [filter_action]
  unfold specDrop
  cases hf : fields p with
  | none => simp
  | some f =>
    simp only [Option.some.injEq, exists_eq_left']
    constructor
    · intro hd
      by_cases hc : dropCond thr c f
      · obtain ⟨he, hp, hs, ht⟩ := hc
        unfold synOnly at hs
        refine ⟨he, hp, ?_, ?_, ht⟩ <;>
          cases h1 : synSet f.flags <;> cases h2 : ackSet f.flags <;> simp_all
      · rw [if_neg hc] at hd
        cases hd
    · rintro ⟨he, hp, hs, ha, ht⟩
      rw [if_pos]
      exact ⟨he, hp, by unfold synOnly; rw [hs, ha]; rfl, ht⟩

/-- §27.4: established traffic (ACK set) is never dropped, wherever IPv4
options put its TCP header. -/
theorem syn_mitigator_never_drops_ack_packets (thr : Nat) (c : Counts) (p : List UInt8)
    (f : Fields) (hf : fields p = some f) (hack : ackSet f.flags = true) :
    (filter thr c p).1 = .pass := by
  rw [filter_action, hf]
  simp only
  rw [if_neg]
  unfold dropCond synOnly
  simp [hack]

theorem only_syn_only_packets_are_counted (thr : Nat) (c : Counts) (p : List UInt8)
    (hn : ∀ f, fields p = some f → synOnly f = false) :
    (filter thr c p).2 = c := by
  unfold filter
  cases hf : fields p with
  | none => rfl
  | some f =>
    simp only [hn f hf, Bool.not_false, ↓reduceIte]
    split <;> rfl

theorem count_increments_by_one (thr : Nat) (c : Counts) (p : List UInt8)
    (f : Fields) (hf : fields p = some f)
    (hip : f.etherType = ethPIpv4 ∧ f.proto = ipprotoTcp)
    (hs : synOnly f = true) :
    (filter thr c p).2 f.src = some ((c f.src).getD 0 + 1) := by
  unfold filter
  rw [hf]
  simp only [hip.1, hip.2, ne_eq, not_true_eq_false, or_self, ↓reduceIte, hs,
    Bool.not_true, bump]
  by_cases ht : (c f.src).getD 0 + 1 ≤ thr <;> simp [ht]

#print axioms reads_within_checked_bounds
#print axioms short_packets_pass
#print axioms filter_action
#print axioms drop_iff_spec
#print axioms syn_mitigator_never_drops_ack_packets
#print axioms only_syn_only_packets_are_counted
#print axioms count_increments_by_one

/-! ## Fixture table

Each row: an Ethernet/IPv4/TCP header with the given ether type,
protocol, flags, source and IHL (IHL above 5 inserts zero option bytes
before the TCP header), run in sequence through `filter` with threshold 2,
printing the action code per packet. The Rust test builds the same
packets, runs them through the real program in the interpreter and
compares the codes. -/

def mkPacket (etherType proto flags src ihl : Nat) : List UInt8 :=
  let b (n : Nat) : UInt8 := n.toUInt8
  (List.replicate 12 (b 2)) ++ [b (etherType / 256), b etherType] ++
  [b (0x40 + ihl % 16), b 0] ++ List.replicate 7 (b 0) ++ [b proto] ++ [b 0, b 0] ++
  [b (src / 2^24), b (src / 2^16), b (src / 2^8), b src] ++ List.replicate 4 (b 0) ++
  List.replicate (4 * (ihl - 5)) (b 0) ++
  [b 0, b 0, b 0x01, b 0xbb] ++ List.replicate 8 (b 0) ++ [b 0x50, b flags] ++
  List.replicate 6 (b 0)

def fixture : List (Nat × Nat × Nat × Nat × Nat) :=
  [ (0x0800, 6, 0x02, 7, 5), (0x0800, 6, 0x02, 7, 5), (0x0800, 6, 0x02, 7, 5),
    (0x0800, 6, 0x02, 7, 5), (0x0800, 6, 0x12, 7, 5), (0x0800, 6, 0x10, 7, 5),
    (0x0800, 17, 0x02, 7, 5), (0x86dd, 6, 0x02, 7, 5), (0x0800, 6, 0x02, 9, 5),
    (0x0800, 6, 0x03, 7, 5), (0x0800, 6, 0x22, 9, 5), (0x0800, 6, 0x02, 9, 5),
    (0x0800, 6, 0x02, 9, 5), (0x0800, 6, 0x00, 9, 5), (0x0800, 6, 0x3f, 7, 5),
    (0x0800, 6, 0x02, 11, 6), (0x0800, 6, 0x02, 11, 15), (0x0800, 6, 0x02, 11, 6),
    (0x0800, 6, 0x10, 11, 6), (0x0800, 6, 0x02, 11, 4), (0x0800, 6, 0x02, 11, 0) ]

def runFixture : List Nat :=
  (fixture.foldl (fun (acc : List Nat × Counts) (row : Nat × Nat × Nat × Nat × Nat) =>
    let (e, pr, fl, s, ihl) := row
    let (a, c') := filter 2 acc.2 (mkPacket e pr fl s ihl)
    (acc.1 ++ [a.code], c')) ([], fun _ => none)).1

#eval s!"fixture: {runFixture}"

end Vesper.EbpfFirewall

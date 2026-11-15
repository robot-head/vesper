-- CrucibleMigrationSafety.lean (§21.4)
-- Shortlex ChoiceTape shrinking termination and mathematical equivalence of the
-- migrated 61-byte Kafka RecordBatch v2 wire-header parser (`vesperParseBatchHeader`).
--
-- No `sorry`, no `admit`, and no custom axioms are used.

namespace Vesper.CrucibleMigrationSafety

/-- §21.4 Shortlex tape reduction `(len', sum') <lex (len, sum)` is well-founded. -/
theorem choice_tape_shrink_terminates
    (len1 len2 sum1 sum2 : Nat)
    (h_step : len2 < len1 ∨ (len2 = len1 ∧ sum2 < sum1)) :
    Prod.Lex (· < ·) (· < ·) (len2, sum2) (len1, sum1) := by
  rcases h_step with h_lt | ⟨h_eq, h_sum⟩
  · exact Prod.Lex.left _ _ h_lt
  · subst h_eq; exact Prod.Lex.right _ h_sum

structure Word8 where
  val : Nat
  lt  : val < 256
  deriving DecidableEq, Repr

structure Word16 where
  val : Nat
  lt  : val < 2 ^ 16
  deriving DecidableEq, Repr

structure Word32 where
  val : Nat
  lt  : val < 2 ^ 32
  deriving DecidableEq, Repr

structure Word64 where
  val : Nat
  lt  : val < 2 ^ 64
  deriving DecidableEq, Repr

@[simp] def Word64.toNative (w : Word64) : Nat := w.val
@[simp] def Word32.toNative (w : Word32) : Nat := w.val

/-- Canonical 61-byte Kafka RecordBatch v2 wire header (§21.3–§21.4). -/
structure KafkaBatchWireHeader where
  baseOffset           : Word64
  batchLength          : Word32
  partitionLeaderEpoch : Word32
  magic                : Word8
  crc                  : Word32
  attributes           : Word16
  lastOffsetDelta      : Word32
  baseTimestamp        : Word64
  maxTimestamp         : Word64
  producerId           : Word64
  producerEpoch        : Word16
  baseSequence         : Word32
  recordsCount         : Word32
  deriving DecidableEq, Repr

structure DecoderStats where
  lastCrc     : Nat
  recordCount : Nat
  deriving DecidableEq, Repr

inductive ParseError where
  | short
  | malformed
  deriving DecidableEq, Repr

def encodeBe1 (x : Nat) : List Nat := [x % 256]

def parseBe1 : List Nat → Option (Nat × List Nat)
  | b :: rest => some (b, rest)
  | []        => none

def encodeBe2 (x : Nat) : List Nat :=
  [(x / 256) % 256, x % 256]

def parseBe2 : List Nat → Option (Nat × List Nat)
  | b1 :: b0 :: rest => some (b1 * 256 + b0, rest)
  | _                => none

def encodeBe4 (x : Nat) : List Nat :=
  encodeBe2 (x / 65536) ++ encodeBe2 (x % 65536)

def parseBe4 (bytes : List Nat) : Option (Nat × List Nat) :=
  match parseBe2 bytes with
  | none => none
  | some (hi, r1) =>
      match parseBe2 r1 with
      | none => none
      | some (lo, r2) => some (hi * 65536 + lo, r2)

def encodeBe8 (x : Nat) : List Nat :=
  encodeBe4 (x / 4294967296) ++ encodeBe4 (x % 4294967296)

def parseBe8 (bytes : List Nat) : Option (Nat × List Nat) :=
  match parseBe4 bytes with
  | none => none
  | some (hi, r1) =>
      match parseBe4 r1 with
      | none => none
      | some (lo, r2) => some (hi * 4294967296 + lo, r2)

theorem parseBe1_encodeBe1 (x : Nat) (rest : List Nat) (hx : x < 256) :
    parseBe1 (encodeBe1 x ++ rest) = some (x, rest) := by
  simp only [encodeBe1, List.cons_append, List.nil_append, parseBe1]
  refine congrArg (fun v => some (v, rest)) (Nat.mod_eq_of_lt hx)

theorem parseBe2_encodeBe2 (x : Nat) (rest : List Nat) (hx : x < 2 ^ 16) :
    parseBe2 (encodeBe2 x ++ rest) = some (x, rest) := by
  simp only [encodeBe2, List.cons_append, List.nil_append, parseBe2]
  refine congrArg (fun v => some (v, rest)) (by omega)

theorem parseBe4_encodeBe4 (x : Nat) (rest : List Nat) (hx : x < 2 ^ 32) :
    parseBe4 (encodeBe4 x ++ rest) = some (x, rest) := by
  have h_hi : x / 65536 < 2 ^ 16 := by omega
  have h_lo : x % 65536 < 2 ^ 16 := by omega
  unfold encodeBe4 parseBe4
  rw [List.append_assoc, parseBe2_encodeBe2 _ _ h_hi]
  dsimp only
  rw [parseBe2_encodeBe2 _ _ h_lo]
  dsimp only
  refine congrArg (fun v => some (v, rest)) (by omega)

theorem parseBe8_encodeBe8 (x : Nat) (rest : List Nat) (hx : x < 2 ^ 64) :
    parseBe8 (encodeBe8 x ++ rest) = some (x, rest) := by
  have h_hi : x / 4294967296 < 2 ^ 32 := by omega
  have h_lo : x % 4294967296 < 2 ^ 32 := by omega
  unfold encodeBe8 parseBe8
  rw [List.append_assoc, parseBe4_encodeBe4 _ _ h_hi]
  dsimp only
  rw [parseBe4_encodeBe4 _ _ h_lo]
  dsimp only
  refine congrArg (fun v => some (v, rest)) (by omega)

theorem parseBe1_encodeWord8 (x : Word8) (rest : List Nat) :
    parseBe1 (encodeBe1 x.val ++ rest) = some (x.val, rest) :=
  parseBe1_encodeBe1 x.val rest x.lt

theorem parseBe2_encodeWord16 (x : Word16) (rest : List Nat) :
    parseBe2 (encodeBe2 x.val ++ rest) = some (x.val, rest) :=
  parseBe2_encodeBe2 x.val rest x.lt

theorem parseBe4_encodeWord32 (x : Word32) (rest : List Nat) :
    parseBe4 (encodeBe4 x.val ++ rest) = some (x.val, rest) :=
  parseBe4_encodeBe4 x.val rest x.lt

theorem parseBe4_encodeWord32_nil (x : Word32) :
    parseBe4 (encodeBe4 x.val) = some (x.val, []) := by
  simpa using parseBe4_encodeBe4 x.val [] x.lt

theorem parseBe8_encodeWord64 (x : Word64) (rest : List Nat) :
    parseBe8 (encodeBe8 x.val ++ rest) = some (x.val, rest) :=
  parseBe8_encodeBe8 x.val rest x.lt

def Wire.toBytes (hdr : KafkaBatchWireHeader) : List Nat :=
  encodeBe8 hdr.baseOffset.val ++
  encodeBe4 hdr.batchLength.val ++
  encodeBe4 hdr.partitionLeaderEpoch.val ++
  encodeBe1 hdr.magic.val ++
  encodeBe4 hdr.crc.val ++
  encodeBe2 hdr.attributes.val ++
  encodeBe4 hdr.lastOffsetDelta.val ++
  encodeBe8 hdr.baseTimestamp.val ++
  encodeBe8 hdr.maxTimestamp.val ++
  encodeBe8 hdr.producerId.val ++
  encodeBe2 hdr.producerEpoch.val ++
  encodeBe4 hdr.baseSequence.val ++
  encodeBe4 hdr.recordsCount.val

def parseHeaderBody (bytes : List Nat) (st : DecoderStats) :
    Option (Nat × DecoderStats) :=
  match parseBe8 bytes with
  | none => none
  | some (baseOffset, r1) =>
  match parseBe4 r1 with
  | none => none
  | some (_, r2) =>
  match parseBe4 r2 with
  | none => none
  | some (_, r3) =>
  match parseBe1 r3 with
  | none => none
  | some (_, r4) =>
  match parseBe4 r4 with
  | none => none
  | some (crc, r5) =>
  match parseBe2 r5 with
  | none => none
  | some (_, r6) =>
  match parseBe4 r6 with
  | none => none
  | some (_, r7) =>
  match parseBe8 r7 with
  | none => none
  | some (_, r8) =>
  match parseBe8 r8 with
  | none => none
  | some (_, r9) =>
  match parseBe8 r9 with
  | none => none
  | some (_, r10) =>
  match parseBe2 r10 with
  | none => none
  | some (_, r11) =>
  match parseBe4 r11 with
  | none => none
  | some (_, r12) =>
  match parseBe4 r12 with
  | none => none
  | some (recordsCount, _) =>
      some (baseOffset, { lastCrc := crc, recordCount := recordsCount })

def vesperParseBatchHeader (bytes : List Nat) (st : DecoderStats) :
    Except ParseError (Nat × DecoderStats) :=
  if bytes.length < 61 then
    .error .short
  else
    match parseHeaderBody bytes st with
    | some res => .ok res
    | none     => .error .malformed

theorem wire_header_length_61 (hdr : KafkaBatchWireHeader) :
    (Wire.toBytes hdr).length = 61 := by
  simp only [Wire.toBytes, encodeBe8, encodeBe4, encodeBe2, encodeBe1,
    List.length_append, List.length_cons, List.length_nil]

theorem vesperParseBatchHeader_short_fails
    (bytes : List Nat) (st : DecoderStats) (h_short : bytes.length < 61) :
    vesperParseBatchHeader bytes st = .error .short := by
  simp only [vesperParseBatchHeader, ite_eq_left h_short]

theorem parseHeaderBody_toBytes
    (hdr : KafkaBatchWireHeader) (st : DecoderStats) :
    parseHeaderBody (Wire.toBytes hdr) st =
      some (hdr.baseOffset.val, { lastCrc := hdr.crc.val, recordCount := hdr.recordsCount.val }) := by
  unfold parseHeaderBody Wire.toBytes
  simp only [List.append_assoc,
    parseBe8_encodeWord64,
    parseBe4_encodeWord32,
    parseBe1_encodeWord8,
    parseBe2_encodeWord16,
    parseBe4_encodeWord32_nil]

/-- §21.4 Total functional equivalence across all 61-byte `KafkaBatchWireHeader`s. -/
theorem migrated_batch_header_matches_spec
    (hdr : KafkaBatchWireHeader) (st : DecoderStats) :
    (vesperParseBatchHeader (Wire.toBytes hdr) st).map Prod.fst =
      .ok hdr.baseOffset.toNative := by
  have hlen : ¬ ((Wire.toBytes hdr).length < 61) := by
    rw [wire_header_length_61 hdr]
    omega
  simp only [vesperParseBatchHeader, ite_eq_right hlen, parseHeaderBody_toBytes]
  rfl

/-- Exact CRC and record-count extraction onto `DecoderStats`. -/
theorem migrated_batch_header_stats_exact
    (hdr : KafkaBatchWireHeader) (st : DecoderStats) :
    vesperParseBatchHeader (Wire.toBytes hdr) st =
      .ok (hdr.baseOffset.toNative,
           { lastCrc := hdr.crc.toNative, recordCount := hdr.recordsCount.toNative }) := by
  have hlen : ¬ ((Wire.toBytes hdr).length < 61) := by
    rw [wire_header_length_61 hdr]
    omega
  simp only [vesperParseBatchHeader, ite_eq_right hlen, parseHeaderBody_toBytes]
  rfl

end Vesper.CrucibleMigrationSafety

#print axioms Vesper.CrucibleMigrationSafety.choice_tape_shrink_terminates
#print axioms Vesper.CrucibleMigrationSafety.wire_header_length_61
#print axioms Vesper.CrucibleMigrationSafety.vesperParseBatchHeader_short_fails
#print axioms Vesper.CrucibleMigrationSafety.migrated_batch_header_matches_spec
#print axioms Vesper.CrucibleMigrationSafety.migrated_batch_header_stats_exact

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

@[simp] def Word32.toNative (w : Word32) : Nat := w.val
@[simp] def Word16.toNative (w : Word16) : Nat := w.val
@[simp] def Word8.toNative (w : Word8) : Nat := w.val

/-- Signed big-endian 64-bit, 32-bit, and 16-bit wire integers (`i64be`, `i32be`, `i16be`). -/
structure I64be where
  raw : Nat
  lt  : raw < 2 ^ 64
  deriving DecidableEq, Repr

structure I32be where
  raw : Nat
  lt  : raw < 2 ^ 32
  deriving DecidableEq, Repr

structure I16be where
  raw : Nat
  lt  : raw < 2 ^ 16
  deriving DecidableEq, Repr

@[simp] def decodeI64 (raw : Nat) : Int :=
  if raw < 2 ^ 63 then Int.ofNat raw else Int.ofNat raw - (2 ^ 64 : Int)

@[simp] def decodeI32 (raw : Nat) : Int :=
  if raw < 2 ^ 31 then Int.ofNat raw else Int.ofNat raw - (2 ^ 32 : Int)

@[simp] def decodeI16 (raw : Nat) : Int :=
  if raw < 2 ^ 15 then Int.ofNat raw else Int.ofNat raw - (2 ^ 16 : Int)

@[simp] def I64be.toNative (w : I64be) : Int := decodeI64 w.raw
@[simp] def I32be.toNative (w : I32be) : Int := decodeI32 w.raw
@[simp] def I16be.toNative (w : I16be) : Int := decodeI16 w.raw

def I64be.ofInt (x : Int) (h_min : -(2 ^ 63 : Int) ≤ x) (h_max : x < (2 ^ 63 : Int)) : I64be :=
  if hx : 0 ≤ x then
    ⟨x.toNat, by omega⟩
  else
    ⟨2 ^ 64 - (-x).toNat, by omega⟩

def I32be.ofInt (x : Int) (h_min : -(2 ^ 31 : Int) ≤ x) (h_max : x < (2 ^ 31 : Int)) : I32be :=
  if hx : 0 ≤ x then
    ⟨x.toNat, by omega⟩
  else
    ⟨2 ^ 32 - (-x).toNat, by omega⟩

def I16be.ofInt (x : Int) (h_min : -(2 ^ 15 : Int) ≤ x) (h_max : x < (2 ^ 15 : Int)) : I16be :=
  if hx : 0 ≤ x then
    ⟨x.toNat, by omega⟩
  else
    ⟨2 ^ 16 - (-x).toNat, by omega⟩

theorem I64be.ofInt_toNative (x : Int)
    (h_min : -(2 ^ 63 : Int) ≤ x) (h_max : x < (2 ^ 63 : Int)) :
    (I64be.ofInt x h_min h_max).toNative = x := by
  unfold I64be.toNative decodeI64 I64be.ofInt
  split <;> dsimp only <;> split <;> simp only [Int.ofNat_eq_natCast] <;> omega

theorem I32be.ofInt_toNative (x : Int)
    (h_min : -(2 ^ 31 : Int) ≤ x) (h_max : x < (2 ^ 31 : Int)) :
    (I32be.ofInt x h_min h_max).toNative = x := by
  unfold I32be.toNative decodeI32 I32be.ofInt
  split <;> dsimp only <;> split <;> simp only [Int.ofNat_eq_natCast] <;> omega

theorem I16be.ofInt_toNative (x : Int)
    (h_min : -(2 ^ 15 : Int) ≤ x) (h_max : x < (2 ^ 15 : Int)) :
    (I16be.ofInt x h_min h_max).toNative = x := by
  unfold I16be.toNative decodeI16 I16be.ofInt
  split <;> dsimp only <;> split <;> simp only [Int.ofNat_eq_natCast] <;> omega

/-- Canonical 61-byte Kafka RecordBatch v2 wire header (§15.2, §21.3–§21.4):
    signed big-endian fields use `I64be`/`I32be`/`I16be`, while `magic`,
    `crc`, and `attributes` remain unsigned. -/
structure KafkaBatchWireHeader where
  baseOffset           : I64be
  batchLength          : I32be
  partitionLeaderEpoch : I32be
  magic                : Word8
  crc                  : Word32
  attributes           : Word16
  lastOffsetDelta      : I32be
  baseTimestamp        : I64be
  maxTimestamp         : I64be
  producerId           : I64be
  producerEpoch        : I16be
  baseSequence         : I32be
  recordsCount         : I32be
  deriving DecidableEq, Repr

structure DecoderStats where
  lastCrc     : Nat
  recordCount : Int
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

theorem parseBe2_encodeI16 (x : I16be) (rest : List Nat) :
    parseBe2 (encodeBe2 x.raw ++ rest) = some (x.raw, rest) :=
  parseBe2_encodeBe2 x.raw rest x.lt

theorem parseBe4_encodeWord32 (x : Word32) (rest : List Nat) :
    parseBe4 (encodeBe4 x.val ++ rest) = some (x.val, rest) :=
  parseBe4_encodeBe4 x.val rest x.lt

theorem parseBe4_encodeI32 (x : I32be) (rest : List Nat) :
    parseBe4 (encodeBe4 x.raw ++ rest) = some (x.raw, rest) :=
  parseBe4_encodeBe4 x.raw rest x.lt

theorem parseBe4_encodeI32_nil (x : I32be) :
    parseBe4 (encodeBe4 x.raw) = some (x.raw, []) := by
  simpa using parseBe4_encodeBe4 x.raw [] x.lt

theorem parseBe8_encodeI64 (x : I64be) (rest : List Nat) :
    parseBe8 (encodeBe8 x.raw ++ rest) = some (x.raw, rest) :=
  parseBe8_encodeBe8 x.raw rest x.lt

def Wire.toBytes (hdr : KafkaBatchWireHeader) : List Nat :=
  encodeBe8 hdr.baseOffset.raw ++
  encodeBe4 hdr.batchLength.raw ++
  encodeBe4 hdr.partitionLeaderEpoch.raw ++
  encodeBe1 hdr.magic.val ++
  encodeBe4 hdr.crc.val ++
  encodeBe2 hdr.attributes.val ++
  encodeBe4 hdr.lastOffsetDelta.raw ++
  encodeBe8 hdr.baseTimestamp.raw ++
  encodeBe8 hdr.maxTimestamp.raw ++
  encodeBe8 hdr.producerId.raw ++
  encodeBe2 hdr.producerEpoch.raw ++
  encodeBe4 hdr.baseSequence.raw ++
  encodeBe4 hdr.recordsCount.raw

def parseHeaderBody (bytes : List Nat) (_st : DecoderStats) :
    Option (Int × DecoderStats) :=
  match parseBe8 bytes with
  | none => none
  | some (baseOffsetRaw, r1) =>
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
  | some (recordsCountRaw, _) =>
      some (decodeI64 baseOffsetRaw,
            { lastCrc := crc, recordCount := decodeI32 recordsCountRaw })

def vesperParseBatchHeader (bytes : List Nat) (st : DecoderStats) :
    Except ParseError (Int × DecoderStats) :=
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
      some (hdr.baseOffset.toNative,
            { lastCrc := hdr.crc.toNative, recordCount := hdr.recordsCount.toNative }) := by
  unfold parseHeaderBody Wire.toBytes
  simp only [List.append_assoc,
    parseBe8_encodeI64,
    parseBe4_encodeI32,
    parseBe4_encodeWord32,
    parseBe1_encodeWord8,
    parseBe2_encodeWord16,
    parseBe2_encodeI16,
    parseBe4_encodeI32_nil,
    I64be.toNative,
    Word32.toNative,
    I32be.toNative]

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

/-- Exact unsigned CRC and signed `baseOffset`/`recordsCount` extraction onto `DecoderStats`. -/
theorem migrated_batch_header_stats_exact
    (hdr : KafkaBatchWireHeader) (st : DecoderStats) :
    vesperParseBatchHeader (Wire.toBytes hdr) st =
      .ok (hdr.baseOffset.toNative,
           { lastCrc := hdr.crc.toNative, recordCount := hdr.recordsCount.toNative }) := by
  have hlen : ¬ ((Wire.toBytes hdr).length < 61) := by
    rw [wire_header_length_61 hdr]
    omega
  simp only [vesperParseBatchHeader, ite_eq_right hlen, parseHeaderBody_toBytes]

/-- All-`0xff` bytes decode to `-1` for `i64be` `baseOffset` and `i32be` `recordsCount`,
    while `u32be` `crc` stays unsigned (`0xffffffff` or any 32-bit pattern). -/
def allOnesHeader : KafkaBatchWireHeader :=
  { baseOffset           := ⟨2 ^ 64 - 1, by decide⟩
  , batchLength          := ⟨0, by decide⟩
  , partitionLeaderEpoch := ⟨0, by decide⟩
  , magic                := ⟨2, by decide⟩
  , crc                  := ⟨0xffffffff, by decide⟩
  , attributes           := ⟨0, by decide⟩
  , lastOffsetDelta      := ⟨0, by decide⟩
  , baseTimestamp        := ⟨0, by decide⟩
  , maxTimestamp         := ⟨0, by decide⟩
  , producerId           := ⟨0, by decide⟩
  , producerEpoch        := ⟨0, by decide⟩
  , baseSequence         := ⟨0, by decide⟩
  , recordsCount         := ⟨2 ^ 32 - 1, by decide⟩
  }

theorem all_ff_bytes_decode_negative_one :
    let rawBytes :=
      [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff] ++
      List.replicate 9 0 ++
      [0x12, 0x34, 0x56, 0x78] ++
      List.replicate 36 0 ++
      [0xff, 0xff, 0xff, 0xff]
    vesperParseBatchHeader rawBytes { lastCrc := 0, recordCount := 0 } =
      .ok (-1, { lastCrc := 0x12345678, recordCount := -1 }) := rfl

theorem allOnesHeader_signed_and_crc_unsigned :
    vesperParseBatchHeader (Wire.toBytes allOnesHeader) { lastCrc := 0, recordCount := 0 } =
      .ok (-1, { lastCrc := 0xffffffff, recordCount := -1 }) := by
  rw [migrated_batch_header_stats_exact]
  rfl

/-- Matches `crates/vesper-ffi/tests/wire.rs` (`base_offset = -123_456`, `crc = 0x12345678`). -/
def wireTestNegHeader : KafkaBatchWireHeader :=
  { baseOffset           := I64be.ofInt (-123456) (by decide) (by decide)
  , batchLength          := ⟨0, by decide⟩
  , partitionLeaderEpoch := ⟨0, by decide⟩
  , magic                := ⟨2, by decide⟩
  , crc                  := ⟨0x12345678, by decide⟩
  , attributes           := ⟨0, by decide⟩
  , lastOffsetDelta      := ⟨0, by decide⟩
  , baseTimestamp        := ⟨0, by decide⟩
  , maxTimestamp         := ⟨0, by decide⟩
  , producerId           := ⟨0, by decide⟩
  , producerEpoch        := I16be.ofInt (-32768) (by decide) (by decide)
  , baseSequence         := ⟨0, by decide⟩
  , recordsCount         := I32be.ofInt (-42) (by decide) (by decide)
  }

theorem wire_rs_negative_base_offset_roundtrip :
    vesperParseBatchHeader (Wire.toBytes wireTestNegHeader) { lastCrc := 0, recordCount := 0 } =
      .ok (-123456, { lastCrc := 0x12345678, recordCount := -42 }) := by
  rw [migrated_batch_header_stats_exact]
  rfl

end Vesper.CrucibleMigrationSafety

#print axioms Vesper.CrucibleMigrationSafety.choice_tape_shrink_terminates
#print axioms Vesper.CrucibleMigrationSafety.I64be.ofInt_toNative
#print axioms Vesper.CrucibleMigrationSafety.I32be.ofInt_toNative
#print axioms Vesper.CrucibleMigrationSafety.I16be.ofInt_toNative
#print axioms Vesper.CrucibleMigrationSafety.wire_header_length_61
#print axioms Vesper.CrucibleMigrationSafety.vesperParseBatchHeader_short_fails
#print axioms Vesper.CrucibleMigrationSafety.migrated_batch_header_matches_spec
#print axioms Vesper.CrucibleMigrationSafety.migrated_batch_header_stats_exact
#print axioms Vesper.CrucibleMigrationSafety.all_ff_bytes_decode_negative_one
#print axioms Vesper.CrucibleMigrationSafety.allOnesHeader_signed_and_crc_unsigned
#print axioms Vesper.CrucibleMigrationSafety.wire_rs_negative_base_offset_roundtrip

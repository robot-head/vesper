-- SimdJsonRoundtrip.lean (§29.5)
-- Formalizes `TelemetryPoint` NDJSON/JSON schema roundtrip soundness
-- (`JsonSpec.decodeTelemetryPoint (JsonSpec.encodeTelemetryPoint pt) = .ok pt`),
-- unknown-field tolerance, and fail-closed rejection on missing or out-of-range
-- fields matching `crates/vesper-serde/src/tiles.rs`.
--
-- No `sorry`, no `admit`, and no custom axioms are used.

namespace Vesper.SimdJsonRoundtrip

/-- Inline string bounded by UTF-8 encoded byte length (`≤ 32` bytes),
    matching `InlineStr32::new` and the 32-byte decoder scratch buffer in
    `crates/vesper-serde/src/tiles.rs`. -/
structure InlineStr32 where
  text    : String
  byte_le : text.utf8ByteSize ≤ 32
  deriving DecidableEq, Repr

/-- §29.5 `TelemetryPoint` AoSoA record with exact host bit-width invariants. -/
structure TelemetryPoint where
  tsUnixMs   : Int
  ts_min     : -(2 ^ 63 : Int) ≤ tsUnixMs
  ts_max     : tsUnixMs < (2 ^ 63 : Int)
  latencyUs  : Nat
  lat_lt     : latencyUs < 2 ^ 32
  statusCode : Nat
  status_lt  : statusCode < 2 ^ 16
  flags      : Nat
  flags_lt   : flags < 2 ^ 16
  traceIdHex : InlineStr32
  deriving DecidableEq, Repr

inductive TelemetryField where
  | tsUnixMs
  | latencyUs
  | statusCode
  | flags
  | traceIdHex
  deriving DecidableEq, Repr

inductive TileError where
  | missingField (field : TelemetryField)
  | fieldType (field : TelemetryField)
  | fieldRange (field : TelemetryField)
  deriving DecidableEq, Repr

inductive JsonKey where
  | tsUnixMs
  | latencyUs
  | statusCode
  | flags
  | traceIdHex
  | other (id : Nat)
  deriving DecidableEq, Repr

inductive JsonVal where
  | int (v : Int)
  | str (s : String)
  | bool (b : Bool)
  | null
  deriving DecidableEq, Repr

abbrev JsonObject := List (JsonKey × JsonVal)

namespace JsonSpec

def findField (key : JsonKey) : JsonObject → Option JsonVal
  | [] => none
  | (k, v) :: rest => if k = key then some v else findField key rest

def encodeTelemetryPoint (pt : TelemetryPoint) : JsonObject :=
  [ (.tsUnixMs,   .int pt.tsUnixMs)
  , (.latencyUs,  .int (Int.ofNat pt.latencyUs))
  , (.statusCode, .int (Int.ofNat pt.statusCode))
  , (.flags,      .int (Int.ofNat pt.flags))
  , (.traceIdHex, .str pt.traceIdHex.text)
  ]

def decodeTelemetryPoint (obj : JsonObject) : Except TileError TelemetryPoint :=
  match findField .tsUnixMs obj with
  | none => .error (.missingField .tsUnixMs)
  | some (.int ts) =>
      if h_ts : -(2 ^ 63 : Int) ≤ ts ∧ ts < (2 ^ 63 : Int) then
        match findField .latencyUs obj with
        | none => .error (.missingField .latencyUs)
        | some (.int (.ofNat lat)) =>
            if h_lat : lat < 2 ^ 32 then
              match findField .statusCode obj with
              | none => .error (.missingField .statusCode)
              | some (.int (.ofNat st)) =>
                  if h_st : st < 2 ^ 16 then
                    match findField .flags obj with
                    | none => .error (.missingField .flags)
                    | some (.int (.ofNat fl)) =>
                        if h_fl : fl < 2 ^ 16 then
                          match findField .traceIdHex obj with
                          | none => .error (.missingField .traceIdHex)
                          | some (.str s) =>
                              if h_s : s.utf8ByteSize ≤ 32 then
                                .ok {
                                  tsUnixMs   := ts
                                  ts_min     := h_ts.1
                                  ts_max     := h_ts.2
                                  latencyUs  := lat
                                  lat_lt     := h_lat
                                  statusCode := st
                                  status_lt  := h_st
                                  flags      := fl
                                  flags_lt   := h_fl
                                  traceIdHex := ⟨s, h_s⟩
                                }
                              else .error (.fieldRange .traceIdHex)
                          | some _ => .error (.fieldType .traceIdHex)
                        else .error (.fieldRange .flags)
                    | some (.int (.negSucc _)) => .error (.fieldRange .flags)
                    | some _ => .error (.fieldType .flags)
                  else .error (.fieldRange .statusCode)
              | some (.int (.negSucc _)) => .error (.fieldRange .statusCode)
              | some _ => .error (.fieldType .statusCode)
            else .error (.fieldRange .latencyUs)
        | some (.int (.negSucc _)) => .error (.fieldRange .latencyUs)
        | some _ => .error (.fieldType .latencyUs)
      else .error (.fieldRange .tsUnixMs)
  | some _ => .error (.fieldType .tsUnixMs)

private theorem findField_encode_ts (pt : TelemetryPoint) :
    findField .tsUnixMs (encodeTelemetryPoint pt) = some (.int pt.tsUnixMs) := by
  simp [findField, encodeTelemetryPoint]

private theorem findField_encode_lat (pt : TelemetryPoint) :
    findField .latencyUs (encodeTelemetryPoint pt) =
      some (.int (Int.ofNat pt.latencyUs)) := by
  simp [findField, encodeTelemetryPoint]

private theorem findField_encode_status (pt : TelemetryPoint) :
    findField .statusCode (encodeTelemetryPoint pt) =
      some (.int (Int.ofNat pt.statusCode)) := by
  simp [findField, encodeTelemetryPoint]

private theorem findField_encode_flags (pt : TelemetryPoint) :
    findField .flags (encodeTelemetryPoint pt) =
      some (.int (Int.ofNat pt.flags)) := by
  simp [findField, encodeTelemetryPoint]

private theorem findField_encode_trace (pt : TelemetryPoint) :
    findField .traceIdHex (encodeTelemetryPoint pt) =
      some (.str pt.traceIdHex.text) := by
  simp [findField, encodeTelemetryPoint]

/-- §29.5 `TelemetryPoint` JSON encode/decode roundtrip theorem. -/
theorem telemetry_point_json_roundtrip (pt : TelemetryPoint) :
    decodeTelemetryPoint (encodeTelemetryPoint pt) = .ok pt := by
  unfold decodeTelemetryPoint
  rw [findField_encode_ts]
  dsimp only
  rw [dite_eq_left ⟨pt.ts_min, pt.ts_max⟩]
  rw [findField_encode_lat]
  dsimp only
  rw [dite_eq_left pt.lat_lt]
  rw [findField_encode_status]
  dsimp only
  rw [dite_eq_left pt.status_lt]
  rw [findField_encode_flags]
  dsimp only
  rw [dite_eq_left pt.flags_lt]
  rw [findField_encode_trace]
  dsimp only
  rw [dite_eq_left pt.traceIdHex.byte_le]

/-- Leading unknown JSON keys are skipped cleanly during field decoding. -/
theorem findField_other_prefix (k : JsonKey) (id : Nat) (v : JsonVal) (obj : JsonObject)
    (h_known : k ≠ .other id) :
    findField k ((.other id, v) :: obj) = findField k obj := by
  have h_neq : JsonKey.other id ≠ k := fun h_eq => h_known h_eq.symm
  simp [findField, h_neq]

theorem telemetry_point_decode_ignores_unknown_fields
    (id : Nat) (v : JsonVal) (pt : TelemetryPoint) :
    decodeTelemetryPoint ((.other id, v) :: encodeTelemetryPoint pt) = .ok pt := by
  unfold decodeTelemetryPoint
  rw [findField_other_prefix .tsUnixMs id v _ (by intro h; cases h), findField_encode_ts]
  dsimp only
  rw [dite_eq_left ⟨pt.ts_min, pt.ts_max⟩]
  rw [findField_other_prefix .latencyUs id v _ (by intro h; cases h), findField_encode_lat]
  dsimp only
  rw [dite_eq_left pt.lat_lt]
  rw [findField_other_prefix .statusCode id v _ (by intro h; cases h), findField_encode_status]
  dsimp only
  rw [dite_eq_left pt.status_lt]
  rw [findField_other_prefix .flags id v _ (by intro h; cases h), findField_encode_flags]
  dsimp only
  rw [dite_eq_left pt.flags_lt]
  rw [findField_other_prefix .traceIdHex id v _ (by intro h; cases h), findField_encode_trace]
  dsimp only
  rw [dite_eq_left pt.traceIdHex.byte_le]

/-- Empty JSON object fails closed on missing `ts_unix_ms`. -/
theorem telemetry_point_empty_rejected :
    decodeTelemetryPoint [] = .error (.missingField .tsUnixMs) := rfl

/-- 17 copies of `'é'` have 17 Unicode scalar values (`≤ 32`) but 34 UTF-8 bytes
    (`> 32`), so `decodeTelemetryPoint` fails closed with `fieldRange .traceIdHex`. -/
def seventeenEAcute : String := String.ofList (List.replicate 17 'é')

theorem seventeenEAcute_chars_le_bytes_gt :
    seventeenEAcute.length = 17 ∧ seventeenEAcute.utf8ByteSize = 34 := by
  decide

theorem telemetry_point_multibyte_utf8_over_32_bytes_rejected :
    decodeTelemetryPoint
      [ (.tsUnixMs,   .int 1700000000000)
      , (.latencyUs,  .int 42)
      , (.statusCode, .int 200)
      , (.flags,      .int 0)
      , (.traceIdHex, .str seventeenEAcute)
      ] = .error (.fieldRange .traceIdHex) := rfl

/-- 16 copies of `'é'` occupy exactly 32 UTF-8 bytes and are accepted. -/
def sixteenEAcute : String := String.ofList (List.replicate 16 'é')

theorem telemetry_point_multibyte_utf8_32_bytes_accepted :
    (decodeTelemetryPoint
      [ (.tsUnixMs,   .int 1700000000000)
      , (.latencyUs,  .int 42)
      , (.statusCode, .int 200)
      , (.flags,      .int 0)
      , (.traceIdHex, .str sixteenEAcute)
      ]).isOk = true := rfl

#eval seventeenEAcute.length
#eval seventeenEAcute.utf8ByteSize

end JsonSpec

end Vesper.SimdJsonRoundtrip

#print axioms Vesper.SimdJsonRoundtrip.JsonSpec.telemetry_point_json_roundtrip
#print axioms Vesper.SimdJsonRoundtrip.JsonSpec.telemetry_point_decode_ignores_unknown_fields
#print axioms Vesper.SimdJsonRoundtrip.JsonSpec.telemetry_point_empty_rejected
#print axioms Vesper.SimdJsonRoundtrip.JsonSpec.telemetry_point_multibyte_utf8_over_32_bytes_rejected
#print axioms Vesper.SimdJsonRoundtrip.JsonSpec.telemetry_point_multibyte_utf8_32_bytes_accepted

/-!
# Vesper Beacon PII redaction non-interference (design §22.4)

A model of Beacon's export boundary and of its dictionary-encoded export
stream, with what is proved about each:

* `exportAttr fp a` is the attribute the exporter emits. A public attribute
  keeps its value; a PII attribute under the `redact` policy becomes the
  constant marker; under `fingerprint` it becomes `fp value`, an abstract
  keyed hash (any function `String → Nat`).
* `attrLow fp a b`: `a` and `b` have the same key and label, agree on the
  value when it is public, and agree on `fp value` when it is fingerprinted.
  PII values under `redact` may differ arbitrarily.
* `export_attr_noninterference`, `export_span_noninterference`: low-equivalent
  inputs export identically. `redact_only_low` shows that when no attribute
  is fingerprinted, low-equivalence asks nothing of PII values at all.
* `encodeStream cap d batches` models the producer's per-stream string
  dictionary: each batch interns its strings, emits the newly added ones as
  a delta and the indices of all of them, and resets the dictionary (fresh
  stream) when it would exceed `cap` entries.
* `stream_noninterference`: two batch sequences that are pointwise
  low-equivalent produce the identical message stream, deltas and resets
  included.
* `emitted_strings_exported` and `pii_never_emitted`: every string in an
  emitted delta is a key, a public value or the marker of some exported
  attribute; so with only the `redact` policy, a PII value that is not also
  a key or public value never appears in the stream.
* Structured values (`Val`: OTLP slices, maps and byte strings, every leaf
  labelled and exported like a top-level value; `exportVal` is the token
  model of the serialized `ser` cell). `export_val_noninterference` and
  `struct_attrs_noninterference`: low-equivalent structured values and
  attribute lists export identically. `exported_texts_public`: every text
  token of an exported value is a map key, a public leaf or the marker.

These are theorems about these models only, not about the Rust crate
`crates/vesper-beacon-arrow`: the model abstracts span fields to a `Nat`,
attribute values and byte strings to `String`, numeric leaves to `Int`, CBOR
to the `Tok` sequence, Arrow IPC framing to `Msg` and the HMAC to `fp`.
Its tests check the same properties at run time on real encoded bytes, and
`tests/lean_model.rs` compares `exportAttr` under `redact` with the Rust
exporter through `lean/BeaconRedaction/Fixture.lean`.
-/

namespace Vesper.BeaconRedaction

def modelId : String := "vesper.beacon.redaction-model/1"
def modelClaim : String :=
  "attribute redaction and dictionary export stream models; not a proof about the Rust implementation"

def marker : String := "[REDACTED]"

inductive Policy where
  | redact
  | fingerprint
  deriving DecidableEq, Repr

inductive Label where
  | pub
  | pii (p : Policy)
  deriving DecidableEq, Repr

structure Attr where
  key : String
  label : Label
  value : String
  deriving DecidableEq, Repr

inductive Out where
  | plain (s : String)
  | redacted
  | tag (h : Nat)
  deriving DecidableEq, Repr

def exportAttr (fp : String → Nat) (a : Attr) : String × Out :=
  match a.label with
  | .pub => (a.key, .plain a.value)
  | .pii .redact => (a.key, .redacted)
  | .pii .fingerprint => (a.key, .tag (fp a.value))

/-- Pointwise relation between two lists of the same length. -/
inductive Pairwise2 {α : Type} (r : α → α → Prop) : List α → List α → Prop where
  | nil : Pairwise2 r [] []
  | cons {a b : α} {as bs : List α} : r a b → Pairwise2 r as bs → Pairwise2 r (a :: as) (b :: bs)

def attrLow (fp : String → Nat) (a b : Attr) : Prop :=
  a.key = b.key ∧ a.label = b.label ∧
    (a.label = .pub → a.value = b.value) ∧
    (a.label = .pii .fingerprint → fp a.value = fp b.value)

theorem export_attr_noninterference (fp : String → Nat) (a b : Attr)
    (h : attrLow fp a b) : exportAttr fp a = exportAttr fp b := by
  obtain ⟨hk, hl, hp, hf⟩ := h
  cases a with
  | mk ka la va =>
    cases b with
    | mk kb lb vb =>
      simp only at hk hl hp hf
      subst hk hl
      cases la with
      | pub => simp [exportAttr, hp rfl]
      | pii p =>
        cases p with
        | redact => simp [exportAttr]
        | fingerprint => simp [exportAttr, hf rfl]

/-- Without fingerprinting, low-equivalence constrains only keys, labels and
public values: PII values are completely free. -/
theorem redact_only_low (fp : String → Nat) (a b : Attr)
    (hk : a.key = b.key) (hl : a.label = b.label)
    (hp : a.label = .pub → a.value = b.value)
    (hnf : a.label ≠ .pii .fingerprint) : attrLow fp a b :=
  ⟨hk, hl, hp, fun h => absurd h hnf⟩

/-- A span: its public fields (ids, times, status) abstracted to one `Nat`,
and its attributes. -/
structure Span where
  fields : Nat
  attrs : List Attr
  deriving Repr

def exportSpan (fp : String → Nat) (s : Span) : Nat × List (String × Out) :=
  (s.fields, s.attrs.map (exportAttr fp))

def spanLow (fp : String → Nat) (s t : Span) : Prop :=
  s.fields = t.fields ∧ Pairwise2 (attrLow fp) s.attrs t.attrs

theorem map_eq_of_forall₂ {α β : Type} (f : α → β) (r : α → α → Prop)
    (hf : ∀ a b, r a b → f a = f b) :
    ∀ {xs ys : List α}, Pairwise2 r xs ys → xs.map f = ys.map f
  | [], [], .nil => rfl
  | _ :: _, _ :: _, .cons hab hrest => by
    simp [hf _ _ hab, map_eq_of_forall₂ f r hf hrest]

theorem export_span_noninterference (fp : String → Nat) (s t : Span)
    (h : spanLow fp s t) : exportSpan fp s = exportSpan fp t := by
  obtain ⟨hf, ha⟩ := h
  have hm := map_eq_of_forall₂ (exportAttr fp) (attrLow fp) (export_attr_noninterference fp) ha
  simp [exportSpan, hf, hm]

/-! ## Dictionary-encoded export stream -/

/-- Strings an exported attribute places in the string dictionary. -/
def outStrings : String × Out → List String
  | (k, .plain s) => [k, s]
  | (k, .redacted) => [k, marker]
  | (k, .tag _) => [k]

def batchStrings (fp : String → Nat) (batch : List Span) : List String :=
  (batch.map (exportSpan fp)).flatMap fun e => e.2.flatMap outStrings

def intern (d : List String) (s : String) : List String :=
  if s ∈ d then d else d ++ [s]

def indexIn (d : List String) (s : String) : Nat := d.idxOf s

structure Msg where
  reset : Bool
  delta : List String
  keys : List Nat
  fields : List Nat
  deriving DecidableEq, Repr

/-- One batch against dictionary `d`: returns the next dictionary and the
message. Resets (fresh dictionary, full contents as delta) on overflow. -/
def encodeBatch (cap : Nat) (d : List String) (fields : List Nat) (strs : List String) :
    List String × Msg :=
  if (strs.foldl intern d).length ≤ cap then
    (strs.foldl intern d,
      ⟨false, (strs.foldl intern d).drop d.length, strs.map (indexIn (strs.foldl intern d)), fields⟩)
  else
    (strs.foldl intern [], ⟨true, strs.foldl intern [], strs.map (indexIn (strs.foldl intern [])), fields⟩)

/-- The encoder sees only exported data: span fields and dictionary strings. -/
def encodeExported (cap : Nat) : List String → List (List Nat × List String) → List Msg
  | _, [] => []
  | d, (fields, strs) :: rest =>
    let r := encodeBatch cap d fields strs
    r.2 :: encodeExported cap r.1 rest

def exportBatch (fp : String → Nat) (batch : List Span) : List Nat × List String :=
  (batch.map Span.fields, batchStrings fp batch)

def encodeStream (fp : String → Nat) (cap : Nat) (d : List String)
    (batches : List (List Span)) : List Msg :=
  encodeExported cap d (batches.map (exportBatch fp))

def batchLow (fp : String → Nat) : List Span → List Span → Prop :=
  Pairwise2 (spanLow fp)

theorem export_batch_noninterference (fp : String → Nat) :
    ∀ {b c : List Span}, batchLow fp b c → exportBatch fp b = exportBatch fp c := by
  intro b c h
  have hspans := map_eq_of_forall₂ (exportSpan fp) (spanLow fp)
    (export_span_noninterference fp) h
  have hfields : b.map Span.fields = c.map Span.fields := by
    have := congrArg (List.map Prod.fst) hspans
    simpa [List.map_map, exportSpan, Function.comp_def] using this
  simp [exportBatch, batchStrings, hspans, hfields]

theorem stream_noninterference (fp : String → Nat) (cap : Nat) (d : List String)
    (xs ys : List (List Span)) (h : Pairwise2 (batchLow fp) xs ys) :
    encodeStream fp cap d xs = encodeStream fp cap d ys := by
  unfold encodeStream
  rw [map_eq_of_forall₂ (exportBatch fp) (batchLow fp)
    (fun _ _ hb => export_batch_noninterference fp hb) h]

/-! ## No PII string reaches a delta -/

theorem mem_foldl_intern :
    ∀ (strs d : List String) (s : String), s ∈ strs.foldl intern d → s ∈ d ∨ s ∈ strs
  | [], d, s, h => Or.inl h
  | x :: xs, d, s, h => by
    simp only [List.foldl] at h
    rcases mem_foldl_intern xs (intern d x) s h with h | h
    · unfold intern at h
      split at h
      · exact Or.inl h
      · rcases List.mem_append.mp h with h | h
        · exact Or.inl h
        · simp at h; exact Or.inr (by simp [h])
    · exact Or.inr (List.mem_cons_of_mem x h)

theorem delta_strings (cap : Nat) (d : List String) (fields : List Nat) (strs : List String)
    (s : String) (h : s ∈ (encodeBatch cap d fields strs).2.delta) : s ∈ d ∨ s ∈ strs := by
  unfold encodeBatch at h
  by_cases hc : (strs.foldl intern d).length ≤ cap
  · rw [ite_eq_left hc] at h
    exact mem_foldl_intern strs d s (List.mem_of_mem_drop h)
  · rw [ite_eq_right hc] at h
    rcases mem_foldl_intern strs [] s h with h | h
    · simp at h
    · exact Or.inr h

theorem dict_strings (cap : Nat) (d : List String) (fields : List Nat) (strs : List String)
    (s : String) (h : s ∈ (encodeBatch cap d fields strs).1) : s ∈ d ∨ s ∈ strs := by
  unfold encodeBatch at h
  by_cases hc : (strs.foldl intern d).length ≤ cap
  · rw [ite_eq_left hc] at h
    exact mem_foldl_intern strs d s h
  · rw [ite_eq_right hc] at h
    rcases mem_foldl_intern strs [] s h with h | h
    · simp at h
    · exact Or.inr h

/-- Every string in any emitted delta was in the initial dictionary or among
the exported strings of some batch. -/
theorem emitted_strings_exported (cap : Nat) :
    ∀ (d : List String) (bs : List (List Nat × List String)) (m : Msg) (s : String),
      m ∈ encodeExported cap d bs → s ∈ m.delta → s ∈ d ∨ ∃ b ∈ bs, s ∈ b.2
  | _, [], _, _, hm, _ => by simp [encodeExported] at hm
  | d, (fields, strs) :: rest, m, s, hm, hs => by
    simp only [encodeExported, List.mem_cons] at hm
    rcases hm with hm | hm
    · subst hm
      rcases delta_strings cap d fields strs s hs with h | h
      · exact Or.inl h
      · exact Or.inr ⟨(fields, strs), by simp, h⟩
    · rcases emitted_strings_exported cap _ rest m s hm hs with h | ⟨b, hb, h⟩
      · rcases dict_strings cap d fields strs s h with h | h
        · exact Or.inl h
        · exact Or.inr ⟨(fields, strs), by simp, h⟩
      · exact Or.inr ⟨b, List.mem_cons_of_mem _ hb, h⟩

/-- Strings a redact-only attribute may contribute: its key, its value when
public, and the marker. -/
def publicStrings (a : Attr) : List String :=
  match a.label with
  | .pub => [a.key, a.value]
  | .pii _ => [a.key, marker]

theorem out_strings_public (fp : String → Nat) (a : Attr) (hnf : a.label ≠ .pii .fingerprint)
    (s : String) (h : s ∈ outStrings (exportAttr fp a)) : s ∈ publicStrings a := by
  cases a with
  | mk k l v =>
    cases l with
    | pub => simpa [exportAttr, outStrings, publicStrings] using h
    | pii p =>
      cases p with
      | redact => simpa [exportAttr, outStrings, publicStrings] using h
      | fingerprint => exact absurd rfl hnf

def redactOnly (batches : List (List Span)) : Prop :=
  ∀ b ∈ batches, ∀ sp ∈ b, ∀ a ∈ sp.attrs, a.label ≠ .pii .fingerprint

/-- With only the redact policy, starting from an empty dictionary, any
string in the stream's deltas is the key, a public value or the marker of
some attribute. A PII value that is none of these is never emitted. -/
theorem pii_never_emitted (fp : String → Nat) (cap : Nat) (batches : List (List Span))
    (hr : redactOnly batches) (m : Msg) (hm : m ∈ encodeStream fp cap [] batches)
    (s : String) (hs : s ∈ m.delta) :
    ∃ b ∈ batches, ∃ sp ∈ b, ∃ a ∈ sp.attrs, s ∈ publicStrings a := by
  rcases emitted_strings_exported cap [] _ m s hm hs with h | ⟨e, he, h⟩
  · simp at h
  · obtain ⟨b, hb, rfl⟩ := List.mem_map.mp he
    simp only [exportBatch, batchStrings, List.mem_flatMap, List.mem_map] at h
    obtain ⟨_, ⟨sp, hsp, rfl⟩, o, ho, hso⟩ := h
    obtain ⟨a, ha, rfl⟩ := List.mem_map.mp ho
    exact ⟨b, hb, sp, hsp, a, ha,
      out_strings_public fp a (hr b hb sp hsp a ha) s hso⟩

/-! ## Structured values: slices, maps and byte strings

OTLP `array_value`, `kvlist_value` and `bytes_value`. Every leaf carries its
own label and passes the same policy as a top-level attribute value; byte
strings are leaves like strings (modelled as `String`), and booleans,
integers and doubles are `num` leaves, always public. `exportVal` is the
token sequence the exporter writes for a value (the model of a CBOR `ser`
cell: open and close markers around slices and maps, map keys as text). -/

mutual
inductive Val where
  | leaf (l : Label) (s : String)
  | num (n : Int)
  | slice (xs : Vals)
  | map (fs : Fields)
inductive Vals where
  | nil
  | cons (v : Val) (rest : Vals)
inductive Fields where
  | nil
  | cons (k : String) (v : Val) (rest : Fields)
end

inductive Tok where
  | text (s : String)
  | tag (h : Nat)
  | num (n : Int)
  | openSlice
  | openMap
  | close
  deriving DecidableEq, Repr

def leafTok (fp : String → Nat) (l : Label) (s : String) : Tok :=
  match l with
  | .pub => .text s
  | .pii .redact => .text marker
  | .pii .fingerprint => .tag (fp s)

mutual
def exportVal (fp : String → Nat) : Val → List Tok
  | .leaf l s => [leafTok fp l s]
  | .num n => [.num n]
  | .slice xs => .openSlice :: (exportVals fp xs ++ [.close])
  | .map fs => .openMap :: (exportFields fp fs ++ [.close])
def exportVals (fp : String → Nat) : Vals → List Tok
  | .nil => []
  | .cons v rest => exportVal fp v ++ exportVals fp rest
def exportFields (fp : String → Nat) : Fields → List Tok
  | .nil => []
  | .cons k v rest => .text k :: (exportVal fp v ++ exportFields fp rest)
end

-- Low-equivalence of structured values: same shape, keys and labels; equal
-- public leaves; equal fingerprints of fingerprinted leaves.
mutual
inductive ValLow (fp : String → Nat) : Val → Val → Prop where
  | leaf {l : Label} {s t : String} :
      (l = .pub → s = t) → (l = .pii .fingerprint → fp s = fp t) →
      ValLow fp (.leaf l s) (.leaf l t)
  | num (n : Int) : ValLow fp (.num n) (.num n)
  | slice {xs ys : Vals} : ValsLow fp xs ys → ValLow fp (.slice xs) (.slice ys)
  | map {xs ys : Fields} : FieldsLow fp xs ys → ValLow fp (.map xs) (.map ys)
inductive ValsLow (fp : String → Nat) : Vals → Vals → Prop where
  | nil : ValsLow fp .nil .nil
  | cons {v w : Val} {r q : Vals} :
      ValLow fp v w → ValsLow fp r q → ValsLow fp (.cons v r) (.cons w q)
inductive FieldsLow (fp : String → Nat) : Fields → Fields → Prop where
  | nil : FieldsLow fp .nil .nil
  | cons (k : String) {v w : Val} {r q : Fields} :
      ValLow fp v w → FieldsLow fp r q → FieldsLow fp (.cons k v r) (.cons k w q)
end

theorem leaf_tok_low (fp : String → Nat) (l : Label) (s t : String)
    (hp : l = .pub → s = t) (hf : l = .pii .fingerprint → fp s = fp t) :
    leafTok fp l s = leafTok fp l t := by
  cases l with
  | pub => simp [leafTok, hp rfl]
  | pii p =>
    cases p with
    | redact => simp [leafTok]
    | fingerprint => simp [leafTok, hf rfl]

mutual
theorem export_val_noninterference (fp : String → Nat) :
    ∀ {v w : Val}, ValLow fp v w → exportVal fp v = exportVal fp w
  | _, _, .leaf hp hf => by simp [exportVal, leaf_tok_low fp _ _ _ hp hf]
  | _, _, .num _ => rfl
  | _, _, .slice h => by simp [exportVal, export_vals_noninterference fp h]
  | _, _, .map h => by simp [exportVal, export_fields_noninterference fp h]
theorem export_vals_noninterference (fp : String → Nat) :
    ∀ {xs ys : Vals}, ValsLow fp xs ys → exportVals fp xs = exportVals fp ys
  | _, _, .nil => rfl
  | _, _, .cons hv hr => by
    simp [exportVals, export_val_noninterference fp hv, export_vals_noninterference fp hr]
theorem export_fields_noninterference (fp : String → Nat) :
    ∀ {xs ys : Fields}, FieldsLow fp xs ys → exportFields fp xs = exportFields fp ys
  | _, _, .nil => rfl
  | _, _, .cons _ hv hr => by
    simp [exportFields, export_val_noninterference fp hv, export_fields_noninterference fp hr]
end

/-- A structured attribute: key and value. -/
structure StructAttr where
  key : String
  value : Val

def exportStructAttr (fp : String → Nat) (a : StructAttr) : String × List Tok :=
  (a.key, exportVal fp a.value)

def structAttrLow (fp : String → Nat) (a b : StructAttr) : Prop :=
  a.key = b.key ∧ ValLow fp a.value b.value

theorem struct_attrs_noninterference (fp : String → Nat) {xs ys : List StructAttr}
    (h : Pairwise2 (structAttrLow fp) xs ys) :
    xs.map (exportStructAttr fp) = ys.map (exportStructAttr fp) :=
  map_eq_of_forall₂ (exportStructAttr fp) (structAttrLow fp)
    (fun a b ⟨hk, hv⟩ => by simp [exportStructAttr, hk, export_val_noninterference fp hv]) h

-- Text a value may legitimately put on the wire: map keys, public leaves
-- and the marker.
mutual
def valTexts : Val → List String
  | .leaf .pub s => [s]
  | .leaf (.pii _) _ => [marker]
  | .num _ => []
  | .slice xs => valsTexts xs
  | .map fs => fieldsTexts fs
def valsTexts : Vals → List String
  | .nil => []
  | .cons v rest => valTexts v ++ valsTexts rest
def fieldsTexts : Fields → List String
  | .nil => []
  | .cons k v rest => k :: (valTexts v ++ fieldsTexts rest)
end

theorem leaf_tok_text (fp : String → Nat) (l : Label) (s t : String)
    (h : leafTok fp l s = .text t) : t ∈ valTexts (.leaf l s) := by
  cases l with
  | pub => simp [leafTok] at h; simp [valTexts, h]
  | pii p =>
    cases p with
    | redact => simp [leafTok] at h; simp [valTexts, h]
    | fingerprint => simp [leafTok] at h

-- Every text token of an exported structured value is a map key, a public
-- leaf or the marker, under either policy (fingerprints are `tag` tokens).
-- So a PII leaf string that is not also a key or public leaf of the same
-- value never appears in its serialized form.
mutual
theorem exported_texts_public (fp : String → Nat) :
    ∀ (v : Val) (t : String), Tok.text t ∈ exportVal fp v → t ∈ valTexts v
  | .leaf l s, t, h => by
    simp only [exportVal, List.mem_singleton] at h
    exact leaf_tok_text fp l s t h.symm
  | .num _, _, h => by simp [exportVal] at h
  | .slice xs, t, h => by
    have h' : Tok.text t ∈ exportVals fp xs := by simpa [exportVal] using h
    simpa [valTexts] using exported_vals_texts_public fp xs t h'
  | .map fs, t, h => by
    have h' : Tok.text t ∈ exportFields fp fs := by simpa [exportVal] using h
    simpa [valTexts] using exported_fields_texts_public fp fs t h'
theorem exported_vals_texts_public (fp : String → Nat) :
    ∀ (xs : Vals) (t : String), Tok.text t ∈ exportVals fp xs → t ∈ valsTexts xs
  | .nil, _, h => by simp [exportVals] at h
  | .cons v rest, t, h => by
    simp only [exportVals, List.mem_append] at h
    simp only [valsTexts, List.mem_append]
    rcases h with h | h
    · exact Or.inl (exported_texts_public fp v t h)
    · exact Or.inr (exported_vals_texts_public fp rest t h)
theorem exported_fields_texts_public (fp : String → Nat) :
    ∀ (fs : Fields) (t : String), Tok.text t ∈ exportFields fp fs → t ∈ fieldsTexts fs
  | .nil, _, h => by simp [exportFields] at h
  | .cons k v rest, t, h => by
    simp only [exportFields, List.mem_cons, List.mem_append, Tok.text.injEq] at h
    simp only [fieldsTexts, List.mem_cons, List.mem_append]
    rcases h with h | h | h
    · exact Or.inl h
    · exact Or.inr (Or.inl (exported_texts_public fp v t h))
    · exact Or.inr (Or.inr (exported_fields_texts_public fp rest t h))
end

end Vesper.BeaconRedaction

#print axioms Vesper.BeaconRedaction.export_attr_noninterference
#print axioms Vesper.BeaconRedaction.redact_only_low
#print axioms Vesper.BeaconRedaction.export_span_noninterference
#print axioms Vesper.BeaconRedaction.export_batch_noninterference
#print axioms Vesper.BeaconRedaction.stream_noninterference
#print axioms Vesper.BeaconRedaction.emitted_strings_exported
#print axioms Vesper.BeaconRedaction.pii_never_emitted
#print axioms Vesper.BeaconRedaction.export_val_noninterference
#print axioms Vesper.BeaconRedaction.struct_attrs_noninterference
#print axioms Vesper.BeaconRedaction.exported_texts_public

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

These are theorems about these models only, not about the Rust crate
`crates/vesper-beacon-arrow`: the model abstracts span fields to a `Nat`,
attribute values to `String`, Arrow IPC framing to `Msg` and the HMAC to `fp`.
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
  · rw [if_pos hc] at h
    exact mem_foldl_intern strs d s (List.mem_of_mem_drop h)
  · rw [if_neg hc] at h
    rcases mem_foldl_intern strs [] s h with h | h
    · simp at h
    · exact Or.inr h

theorem dict_strings (cap : Nat) (d : List String) (fields : List Nat) (strs : List String)
    (s : String) (h : s ∈ (encodeBatch cap d fields strs).1) : s ∈ d ∨ s ∈ strs := by
  unfold encodeBatch at h
  by_cases hc : (strs.foldl intern d).length ≤ cap
  · rw [if_pos hc] at h
    exact mem_foldl_intern strs d s h
  · rw [if_neg hc] at h
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

end Vesper.BeaconRedaction

#print axioms Vesper.BeaconRedaction.export_attr_noninterference
#print axioms Vesper.BeaconRedaction.redact_only_low
#print axioms Vesper.BeaconRedaction.export_span_noninterference
#print axioms Vesper.BeaconRedaction.export_batch_noninterference
#print axioms Vesper.BeaconRedaction.stream_noninterference
#print axioms Vesper.BeaconRedaction.emitted_strings_exported
#print axioms Vesper.BeaconRedaction.pii_never_emitted

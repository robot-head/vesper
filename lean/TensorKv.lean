/-
  Vesper Tensor (design §28.4): paged K/V block conservation and
  grouped-query attention index bounds.

  Everything here is about the Lean model below: a pool of 16-token pages,
  sequences that take a page exactly when their length is a multiple of 16,
  rollback to a shorter length, and release. It mirrors the host protocol of
  crates/vesper-tensor/src/kv.rs (KvPool::begin / append / rollback / release)
  and the GQA mapping of src/shape.rs, but it is NOT a theorem about the Rust
  code: the crate's tests compare the Rust pool with a transliteration of
  `step` on generated operation sequences, which is evidence on those inputs
  only.

  Proved (no `sorry`, no new axioms):
  * `step_conserves`, `run_conserves`: every operation, successful or not,
    keeps `free + active` (free pages plus pages held by sequences) fixed.
  * `step_wf`, `run_wf`: every held sequence keeps exactly
    `pagesFor tokens = ceil(tokens / 16)` pages.
  * `release_frees_all`: after `release id` the sequence holds no pages.
  * `stream_completion_conserves_kv_pages`: the §28.3 streaming loop (begin,
    prefill, decode until EOS, client disconnect or out-of-pages, release)
    conserves pages and leaves the sequence with none.
  * `gqa_kv_head_index_in_bounds` and `gqa_flat_index_in_bounds`: query head
    `qh` reads KV head `qh / (qHeads / kvHeads) < kvHeads`, and its element
    offsets stay inside the `[kvHeads, headDim]` row.
  * `page_index_in_bounds`: a stored position's page index is below the
    sequence's page count.
-/

namespace TensorKv

def modelId : String := "vesper.tensor.kv-model/1"

def pageTokens : Nat := 16

/-- Pages a sequence of `t` tokens holds: `ceil(t / 16)`. -/
def pagesFor (t : Nat) : Nat := (t + 15) / 16

structure Seq where
  id : Nat
  tokens : Nat
  pages : Nat
deriving Repr, DecidableEq

structure Pool where
  free : Nat
  seqs : List Seq
deriving Repr

def held : List Seq → Nat
  | [] => 0
  | s :: rest => s.pages + held rest

def Pool.active (p : Pool) : Nat := held p.seqs

def Pool.total (p : Pool) : Nat := p.free + p.active

/-- Pages held under `id` (all sequences with that id). -/
def pagesOf (id : Nat) : List Seq → Nat
  | [] => 0
  | s :: rest => (if s.id = id then s.pages else 0) + pagesOf id rest

def find? (id : Nat) : List Seq → Option Seq
  | [] => none
  | s :: rest => if s.id = id then some s else find? id rest

def replaceFirst (id : Nat) (s' : Seq) : List Seq → List Seq
  | [] => []
  | s :: rest => if s.id = id then s' :: rest else s :: replaceFirst id s' rest

def dropId (id : Nat) : List Seq → List Seq
  | [] => []
  | s :: rest => if s.id = id then dropId id rest else s :: dropId id rest

inductive Op where
  | begin (id : Nat)
  | append (id : Nat)
  | rollback (id : Nat) (keep : Nat)
  | release (id : Nat)
deriving Repr

/-- One pool operation; the flag reports success. A failed operation
    returns the pool unchanged (as the Rust pool does). -/
def step (p : Pool) : Op → Pool × Bool
  | .begin id =>
    match find? id p.seqs with
    | some _ => (p, false)
    | none => ({ p with seqs := ⟨id, 0, 0⟩ :: p.seqs }, true)
  | .append id =>
    match find? id p.seqs with
    | none => (p, false)
    | some s =>
      if s.tokens % pageTokens = 0 then
        if p.free = 0 then (p, false)
        else ({ free := p.free - 1,
                seqs := replaceFirst id ⟨id, s.tokens + 1, s.pages + 1⟩ p.seqs }, true)
      else ({ p with seqs := replaceFirst id ⟨id, s.tokens + 1, s.pages⟩ p.seqs }, true)
  | .rollback id keep =>
    match find? id p.seqs with
    | none => (p, false)
    | some s =>
      if keep ≤ s.tokens ∧ pagesFor keep ≤ s.pages then
        ({ free := p.free + (s.pages - pagesFor keep),
           seqs := replaceFirst id ⟨id, keep, pagesFor keep⟩ p.seqs }, true)
      else (p, false)
  | .release id =>
    ({ free := p.free + pagesOf id p.seqs, seqs := dropId id p.seqs }, true)

def run (p : Pool) : List Op → Pool
  | [] => p
  | op :: rest => run (step p op).1 rest

/-! ## List lemmas -/

theorem find_id {id : Nat} : ∀ {l : List Seq} {s : Seq}, find? id l = some s → s.id = id
  | [], _, h => by simp [find?] at h
  | x :: rest, s, h => by
    unfold find? at h
    by_cases hx : x.id = id
    · simp [hx] at h; subst h; exact hx
    · simp [hx] at h; exact find_id h

theorem held_replace {id : Nat} {s' : Seq} :
    ∀ {l : List Seq} {s : Seq}, find? id l = some s →
      held (replaceFirst id s' l) + s.pages = held l + s'.pages
  | [], _, h => by simp [find?] at h
  | x :: rest, s, h => by
    unfold find? at h
    by_cases hx : x.id = id
    · simp [hx] at h; subst h; simp [replaceFirst, hx, held]; omega
    · simp [hx] at h
      have ih := held_replace (s' := s') h
      simp [replaceFirst, hx, held]; omega

theorem held_drop (id : Nat) :
    ∀ l : List Seq, held (dropId id l) + pagesOf id l = held l
  | [] => by simp [dropId, pagesOf, held]
  | x :: rest => by
    have ih := held_drop id rest
    by_cases hx : x.id = id
    · simp [dropId, pagesOf, held, hx]; omega
    · simp [dropId, pagesOf, held, hx]; omega

theorem pagesOf_drop (id : Nat) : ∀ l : List Seq, pagesOf id (dropId id l) = 0
  | [] => by simp [dropId, pagesOf]
  | x :: rest => by
    by_cases hx : x.id = id
    · simp [dropId, hx, pagesOf_drop id rest]
    · simp [dropId, pagesOf, hx, pagesOf_drop id rest]

theorem mem_replace {id : Nat} {s' : Seq} :
    ∀ {l : List Seq} {x : Seq}, x ∈ replaceFirst id s' l → x = s' ∨ x ∈ l
  | [], _, h => by simp [replaceFirst] at h
  | y :: rest, x, h => by
    unfold replaceFirst at h
    by_cases hy : y.id = id
    · simp [hy] at h
      rcases h with h | h
      · exact Or.inl h
      · exact Or.inr (List.mem_cons_of_mem _ h)
    · simp [hy] at h
      rcases h with h | h
      · exact Or.inr (h ▸ List.mem_cons_self)
      · rcases mem_replace h with h' | h'
        · exact Or.inl h'
        · exact Or.inr (List.mem_cons_of_mem _ h')

theorem mem_drop {id : Nat} : ∀ {l : List Seq} {x : Seq}, x ∈ dropId id l → x ∈ l
  | [], _, h => by simp [dropId] at h
  | y :: rest, x, h => by
    unfold dropId at h
    by_cases hy : y.id = id
    · simp [hy] at h; exact List.mem_cons_of_mem _ (mem_drop h)
    · simp [hy] at h
      rcases h with h | h
      · exact h ▸ List.mem_cons_self
      · exact List.mem_cons_of_mem _ (mem_drop h)

theorem find_mem {id : Nat} : ∀ {l : List Seq} {s : Seq}, find? id l = some s → s ∈ l
  | [], _, h => by simp [find?] at h
  | x :: rest, s, h => by
    unfold find? at h
    by_cases hx : x.id = id
    · simp [hx] at h; subst h; exact List.mem_cons_self
    · simp [hx] at h; exact List.mem_cons_of_mem _ (find_mem h)

/-! ## Conservation -/

theorem step_conserves (p : Pool) (op : Op) : (step p op).1.total = p.total := by
  cases op with
  | begin id =>
    cases hf : find? id p.seqs with
    | some _ => simp [step, hf]
    | none => simp [step, hf, Pool.total, Pool.active, held]
  | append id =>
    cases hf : find? id p.seqs with
    | none => simp [step, hf]
    | some s =>
      have h1 := held_replace (s' := ⟨id, s.tokens + 1, s.pages + 1⟩) hf
      have h2 := held_replace (s' := ⟨id, s.tokens + 1, s.pages⟩) hf
      by_cases hm : s.tokens % pageTokens = 0
      · by_cases hz : p.free = 0
        · simp [step, hf, hm, hz]
        · simp [step, hf, hm, hz, Pool.total, Pool.active] at h1 ⊢; omega
      · simp [step, hf, hm, Pool.total, Pool.active] at h2 ⊢; omega
  | rollback id keep =>
    cases hf : find? id p.seqs with
    | none => simp [step, hf]
    | some s =>
      have h := held_replace (s' := ⟨id, keep, pagesFor keep⟩) hf
      by_cases hk : keep ≤ s.tokens ∧ pagesFor keep ≤ s.pages
      · simp [step, hf, hk, Pool.total, Pool.active] at h ⊢; omega
      · simp [step, hf, hk]
  | release id =>
    have h := held_drop id p.seqs
    simp [step, Pool.total, Pool.active]; omega

theorem run_conserves : ∀ (p : Pool) (ops : List Op), (run p ops).total = p.total
  | _, [] => rfl
  | p, op :: rest => by
    simp only [run]; rw [run_conserves, step_conserves]

/-! ## Page accounting invariant -/

def WF (p : Pool) : Prop := ∀ s ∈ p.seqs, s.pages = pagesFor s.tokens

theorem pagesFor_succ_page (t : Nat) (h : t % 16 = 0) : pagesFor (t + 1) = pagesFor t + 1 := by
  unfold pagesFor; omega

theorem pagesFor_succ_mid (t : Nat) (h : ¬ t % 16 = 0) : pagesFor (t + 1) = pagesFor t := by
  unfold pagesFor; omega

theorem pagesFor_mono {a b : Nat} (h : a ≤ b) : pagesFor a ≤ pagesFor b := by
  unfold pagesFor; omega

theorem step_wf (p : Pool) (op : Op) (hw : WF p) : WF (step p op).1 := by
  cases op with
  | begin id =>
    cases hf : find? id p.seqs with
    | some _ => simpa [step, hf] using hw
    | none =>
      simp only [step, hf]
      intro x hx
      simp at hx
      rcases hx with hx | hx
      · subst hx; simp [pagesFor]
      · exact hw x hx
  | append id =>
    cases hf : find? id p.seqs with
    | none => simpa [step, hf] using hw
    | some s =>
      have hs := hw s (find_mem hf)
      by_cases hm : s.tokens % pageTokens = 0
      · by_cases hz : p.free = 0
        · simpa [step, hf, hm, hz] using hw
        · simp only [step, hf, hm, hz, if_true, if_false]
          intro x hx
          rcases mem_replace hx with h | h
          · subst h
            have hm' : s.tokens % 16 = 0 := hm
            show s.pages + 1 = pagesFor (s.tokens + 1)
            rw [pagesFor_succ_page _ hm', hs]
          · exact hw x h
      · simp only [step, hf, hm, if_false]
        intro x hx
        rcases mem_replace hx with h | h
        · subst h
          have hm' : ¬ s.tokens % 16 = 0 := hm
          show s.pages = pagesFor (s.tokens + 1)
          rw [pagesFor_succ_mid _ hm', hs]
        · exact hw x h
  | rollback id keep =>
    cases hf : find? id p.seqs with
    | none => simpa [step, hf] using hw
    | some s =>
      by_cases hk : keep ≤ s.tokens ∧ pagesFor keep ≤ s.pages
      · simp only [step, hf, hk, and_self, if_true]
        intro x hx
        rcases mem_replace hx with h | h
        · subst h; rfl
        · exact hw x h
      · simpa [step, hf, hk] using hw
  | release id =>
    intro x hx
    exact hw x (mem_drop hx)

theorem run_wf : ∀ (p : Pool) (ops : List Op), WF p → WF (run p ops)
  | _, [], h => h
  | p, op :: rest, h => run_wf _ rest (step_wf p op h)

/-- Under the invariant a rollback to a shorter length never fails its page
    guard: the model's guard is only defensive. -/
theorem rollback_guard_holds (s : Seq) (keep : Nat) (hs : s.pages = pagesFor s.tokens)
    (hk : keep ≤ s.tokens) : pagesFor keep ≤ s.pages := by
  rw [hs]; exact pagesFor_mono hk

theorem release_frees_all (p : Pool) (id : Nat) :
    pagesOf id (step p (.release id)).1.seqs = 0 := by
  simp [step, pagesOf_drop]

/-! ## The §28.3 streaming loop -/

inductive Outcome where
  | token
  | eos
  | disconnect
deriving Repr

def release (id : Nat) (p : Pool) : Pool := (step p (.release id)).1

/-- Decode until EOS, disconnect, out-of-pages or the end of the events;
    every exit releases the sequence. -/
def decodeLoop (id : Nat) : List Outcome → Pool → Pool
  | [], p => release id p
  | .eos :: _, p => release id p
  | .disconnect :: _, p => release id p
  | .token :: rest, p =>
    match step p (.append id) with
    | (p', true) => decodeLoop id rest p'
    | (p', false) => release id p'

/-- Prefill `n` prompt tokens; out-of-pages releases and reports failure. -/
def prefillOrRelease (id : Nat) : Nat → Pool → Pool × Bool
  | 0, p => (p, true)
  | n + 1, p =>
    match step p (.append id) with
    | (p', true) => prefillOrRelease id n p'
    | (p', false) => (release id p', false)

def streamCompletion (id prompt : Nat) (events : List Outcome) (p : Pool) : Pool :=
  match step p (.begin id) with
  | (p1, false) => p1
  | (p1, true) =>
    match prefillOrRelease id prompt p1 with
    | (p2, true) => decodeLoop id events p2
    | (p2, false) => p2

theorem release_conserves (id : Nat) (p : Pool) : (release id p).total = p.total :=
  step_conserves p _

theorem release_zero (id : Nat) (p : Pool) : pagesOf id (release id p).seqs = 0 :=
  release_frees_all p id

theorem decodeLoop_ok (id : Nat) :
    ∀ (events : List Outcome) (p : Pool),
      (decodeLoop id events p).total = p.total ∧ pagesOf id (decodeLoop id events p).seqs = 0
  | [], p => ⟨release_conserves id p, release_zero id p⟩
  | .eos :: _, p => ⟨release_conserves id p, release_zero id p⟩
  | .disconnect :: _, p => ⟨release_conserves id p, release_zero id p⟩
  | .token :: rest, p => by
    have hc := step_conserves p (.append id)
    simp only [decodeLoop]
    split
    · rename_i p' heq
      rw [heq] at hc
      have ih := decodeLoop_ok id rest p'
      exact ⟨ih.1.trans hc, ih.2⟩
    · rename_i p' heq
      rw [heq] at hc
      exact ⟨(release_conserves id p').trans hc, release_zero id p'⟩

theorem prefill_ok (id : Nat) :
    ∀ (n : Nat) (p : Pool),
      (prefillOrRelease id n p).1.total = p.total ∧
      ((prefillOrRelease id n p).2 = false → pagesOf id (prefillOrRelease id n p).1.seqs = 0)
  | 0, p => ⟨rfl, by simp [prefillOrRelease]⟩
  | n + 1, p => by
    have hc := step_conserves p (.append id)
    simp only [prefillOrRelease]
    split
    · rename_i p' heq
      rw [heq] at hc
      have ih := prefill_ok id n p'
      exact ⟨ih.1.trans hc, ih.2⟩
    · rename_i p' heq
      rw [heq] at hc
      exact ⟨(release_conserves id p').trans hc, fun _ => release_zero id p'⟩

theorem begin_refused_unchanged (p p1 : Pool) (id : Nat)
    (h : step p (.begin id) = (p1, false)) : p1 = p := by
  cases hf : find? id p.seqs with
  | some _ => simp [step, hf] at h; exact h.symm
  | none => simp [step, hf] at h

/-- §28.4 theorem 1, about this model: whatever happens (EOS, client
    disconnect, out-of-pages during prefill or decode, or `begin` refused),
    the pool's pages are conserved and the request holds none afterwards. -/
theorem stream_completion_conserves_kv_pages (p : Pool) (id prompt : Nat)
    (events : List Outcome) (fresh : pagesOf id p.seqs = 0) :
    let p' := streamCompletion id prompt events p
    p'.free + p'.active = p.free + p.active ∧ pagesOf id p'.seqs = 0 := by
  show (streamCompletion id prompt events p).total = p.total ∧ _
  have hb := step_conserves p (.begin id)
  simp only [streamCompletion]
  split
  · rename_i p1 heq
    rw [heq] at hb
    rw [begin_refused_unchanged p p1 id heq]
    exact ⟨rfl, fresh⟩
  · rename_i p1 heq
    rw [heq] at hb
    have hp := prefill_ok id prompt p1
    split
    · rename_i p2 heq2
      rw [heq2] at hp
      have hd := decodeLoop_ok id events p2
      exact ⟨hd.1.trans (hp.1.trans hb), hd.2⟩
    · rename_i p2 heq2
      rw [heq2] at hp
      exact ⟨hp.1.trans hb, hp.2 rfl⟩

/-! ## Index bounds -/

set_option linter.unusedVariables false in
/-- §28.4 theorem 2: grouped-query head mapping. (`h_pos` is implied by
    the other two hypotheses but kept to match the design's statement.) -/
theorem gqa_kv_head_index_in_bounds (qHeads kvHeads qh : Nat)
    (h_pos : kvHeads > 0) (h_div : qHeads % kvHeads = 0) (h_qh : qh < qHeads) :
    let groupSize := qHeads / kvHeads
    qh / groupSize < kvHeads := by
  intro groupSize
  have hq : qHeads = kvHeads * groupSize := by
    have := Nat.div_add_mod qHeads kvHeads
    simp [h_div] at this
    exact this.symm
  have hg : 0 < groupSize := by
    rcases Nat.eq_zero_or_pos groupSize with h0 | h0
    · rw [h0] at hq; omega
    · exact h0
  rw [Nat.div_lt_iff_lt_mul hg]
  rw [hq] at h_qh
  exact h_qh

/-- Element `i < headDim` of KV head `kvh < kvHeads` lies inside the
    `[kvHeads, headDim]` token row. -/
theorem gqa_flat_index_in_bounds (kvHeads headDim kvh i : Nat)
    (hk : kvh < kvHeads) (hi : i < headDim) : kvh * headDim + i < kvHeads * headDim := by
  have : (kvh + 1) * headDim ≤ kvHeads * headDim := Nat.mul_le_mul_right _ hk
  rw [Nat.succ_mul] at this
  omega

/-- A stored position's page is one of the sequence's pages. -/
theorem page_index_in_bounds (pos tokens : Nat) (h : pos < tokens) :
    pos / pageTokens < pagesFor tokens := by
  unfold pagesFor pageTokens; omega

end TensorKv

#print axioms TensorKv.step_conserves
#print axioms TensorKv.run_conserves
#print axioms TensorKv.step_wf
#print axioms TensorKv.run_wf
#print axioms TensorKv.rollback_guard_holds
#print axioms TensorKv.release_frees_all
#print axioms TensorKv.begin_refused_unchanged
#print axioms TensorKv.stream_completion_conserves_kv_pages
#print axioms TensorKv.gqa_kv_head_index_in_bounds
#print axioms TensorKv.gqa_flat_index_in_bounds
#print axioms TensorKv.page_index_in_bounds

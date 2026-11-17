/-
  Vesper Silicon (§26): a model of RISC-V AIA delivery (IMSIC interrupt
  files, APLIC MSI forwarding, `eidelivery`, guest interrupt files and the
  hypervisor CSRs `hgeie`/`hgeip`/`hstatus.VGEIN`/`vstopei`), of
  `dma-ranges` translation, and of PCI `msi-map` requester-ID routing,
  with theorems about the model.

  What is proved, and about what:

  * `File`: an IMSIC interrupt file with `eidelivery`, `eithreshold`, and
    the `eip`/`eie` arrays as `Nat → Bool`. `top_spec` says `top` is the
    least eligible identity (pending, enabled, below a nonzero threshold);
    `top_none` says no identity is eligible when `top` is 0;
    `claim_clears_top` / `claim_keeps_others` say a `*topei` claim clears
    exactly that identity's pending bit; `msi_makes_pending` and
    `msi_asserts` say an MSI with an enabled identity below the threshold
    makes an enabled file assert; `external_file_is_silent` and
    `topei_ignores_delivery` are the two halves of the `eidelivery`
    0x40000000 rule (AIA §3.8.1); `warl_guest_never_external` says a
    guest file (no 0x40000000 option) never takes that value.
  * `hartLevel`: the hart's external interrupt at one level, choosing
    between the file and the PLIC/APLIC by `eidelivery`:
    `level_file_on`, `level_external`, `level_off`.
  * `msiPage`: the APLIC MSI page number of AIA §4.9.1 with group, hart
    and guest fields; `msiPage_injective` says different (group, hart,
    guest) triples within the field widths give different pages, and
    `qemu_s_level_drops_hart` exhibits the collision QEMU 8.2.2's reading
    of LHXW (from `smsiaddrcfgh`, where it is zero) produces.
  * `forward`: an MSI-mode APLIC domain forwarding its pending and enabled
    sources. `forward_pending` (each forwarded source ends not pending,
    every other keeps its bit), `forward_count` (one MSI per forwarded
    source) and `forward_targets` (each MSI carries its source's target).
  * `Hyp`: a hart with guest files `1..geilen`, `hgeie` and `vgein`.
    `hgeip_iff`, `vseip_iff`, `sgeip_needs_hgeie`, `hgeie_bit0`,
    `vs_access_iff` (vsiselect/vsireg/vstopei are reachable exactly when
    VGEIN names a guest file) and `vsclaim_isolated` (a VS-level claim
    changes only the selected guest file, never the hart's own file or
    another guest's).
  * `dma-ranges`: `toCpu` over a window list. `toCpu_sound` (a result lies
    in some window's CPU range at the same offset), `toCpu_none` (no
    window holds the region), `roundtrip` (one window: bus to CPU and
    back), and `compose_sound` (a window composed of two bus levels, as
    `DeviceTree::dma_windows` intersects them, translates as the two
    levels do one after the other).
  * `msi-map`: `mapId` as Linux's `of_map_id` matches entries;
    `mapId_sound`, `mapId_none` and `mapId_mask_idem`.
  * A concrete trace: `hypTrace` runs the hypervisor script of
    `crates/vesper-silicon/tests/aia_qemu.rs` on this model, and
    `hypTrace_matches_qemu` checks by evaluation that it gives the values
    QEMU 8.2.2 printed for that script (recorded in that test as
    `HYPERVISOR_EXPECTED`).

  These are theorems about these models only. They are not proofs about the
  Rust code in crates/vesper-silicon (checked by its tests against QEMU) or
  about QEMU or any hardware.
-/

namespace Vesper.Silicon.Aia

/-! ## First-match search -/

/-- The least `i` in `[k, k + n)` with `p i`. -/
def firstFrom (p : Nat → Bool) : Nat → Nat → Option Nat
  | 0, _ => none
  | n + 1, k => if p k then some k else firstFrom p n (k + 1)

theorem firstFrom_some (p : Nat → Bool) :
    ∀ n k i, firstFrom p n k = some i →
      p i = true ∧ k ≤ i ∧ i < k + n ∧ ∀ j, k ≤ j → j < i → p j = false := by
  intro n
  induction n with
  | zero => intro k i h; simp [firstFrom] at h
  | succ n ih =>
    intro k i h
    simp only [firstFrom] at h
    split at h
    · rename_i hk
      cases h
      exact ⟨hk, Nat.le_refl _, by omega, fun j h1 h2 => by omega⟩
    · rename_i hk
      obtain ⟨h1, h2, h3, h4⟩ := ih (k + 1) i h
      refine ⟨h1, by omega, by omega, fun j hj1 hj2 => ?_⟩
      by_cases hjk : j = k
      · subst hjk; simpa using hk
      · exact h4 j (by omega) hj2

theorem firstFrom_none (p : Nat → Bool) :
    ∀ n k, firstFrom p n k = none → ∀ j, k ≤ j → j < k + n → p j = false := by
  intro n
  induction n with
  | zero => intro k _ j h1 h2; omega
  | succ n ih =>
    intro k h j h1 h2
    simp only [firstFrom] at h
    split at h
    · simp at h
    · rename_i hk
      by_cases hjk : j = k
      · subst hjk; simpa using hk
      · exact ih (k + 1) h j (by omega) (by omega)

/-! ## IMSIC interrupt files -/

def EXT : Nat := 0x40000000

structure File where
  ids : Nat
  /-- Whether `eidelivery` supports 0x40000000. -/
  extOk : Bool
  delivery : Nat
  threshold : Nat
  eip : Nat → Bool
  eie : Nat → Bool

def upd (f : Nat → Bool) (i : Nat) (b : Bool) : Nat → Bool :=
  fun j => if j = i then b else f j

def File.eligible (f : File) (i : Nat) : Bool :=
  f.eip i && f.eie i && (f.threshold == 0 || decide (i < f.threshold))

/-- The `*topei` identity: the least eligible identity in `1..ids`, or 0. -/
def File.top (f : File) : Nat := (firstFrom f.eligible f.ids 1).getD 0

def File.asserted (f : File) : Bool := f.delivery == 1 && f.top != 0

/-- `seteipnum_le`: identities outside `1..ids` are ignored. -/
def File.msi (f : File) (id : Nat) : File :=
  if 1 ≤ id ∧ id ≤ f.ids then { f with eip := upd f.eip id true } else f

/-- `csrrw rd, *topei, x0`. -/
def File.claim (f : File) : File :=
  if f.top = 0 then f else { f with eip := upd f.eip f.top false }

/-- `eidelivery` is WARL: 0x40000000 exactly when supported, else bit 0. -/
def File.setDelivery (f : File) (v : Nat) : File :=
  { f with delivery := if f.extOk && v == EXT then EXT else v % 2 }

theorem top_spec (f : File) (h : f.top ≠ 0) :
    f.eligible f.top = true ∧ 1 ≤ f.top ∧ f.top ≤ f.ids ∧
      ∀ j, 1 ≤ j → j < f.top → f.eligible j = false := by
  unfold File.top at h ⊢
  cases hf : firstFrom f.eligible f.ids 1 with
  | none => simp [hf] at h
  | some i =>
    obtain ⟨h1, h2, h3, h4⟩ := firstFrom_some _ _ _ _ hf
    exact ⟨h1, h2, by simp; omega, h4⟩

theorem top_none (f : File) (h : f.top = 0) :
    ∀ j, 1 ≤ j → j ≤ f.ids → f.eligible j = false := by
  intro j h1 h2
  unfold File.top at h
  cases hf : firstFrom f.eligible f.ids 1 with
  | none => exact firstFrom_none _ _ _ hf j h1 (by omega)
  | some i =>
    obtain ⟨_, h2', _, _⟩ := firstFrom_some _ _ _ _ hf
    simp [hf] at h; omega

theorem claim_clears_top (f : File) (h : f.top ≠ 0) : (f.claim).eip f.top = false := by
  simp [File.claim, h, upd]

theorem claim_keeps_others (f : File) (j : Nat) (h : j ≠ f.top) :
    (f.claim).eip j = f.eip j ∧ (f.claim).eie = f.eie := by
  unfold File.claim
  split
  · exact ⟨rfl, rfl⟩
  · simp [upd, h]

theorem msi_makes_pending (f : File) (id : Nat) (h1 : 1 ≤ id) (h2 : id ≤ f.ids) :
    (f.msi id).eip id = true := by
  simp [File.msi, h1, h2, upd]

/-- An MSI with an enabled identity below the threshold makes a file with
    `eidelivery` = 1 assert. -/
theorem msi_asserts (f : File) (id : Nat) (h1 : 1 ≤ id) (h2 : id ≤ f.ids)
    (he : f.eie id = true) (ht : f.threshold = 0 ∨ id < f.threshold)
    (hd : f.delivery = 1) : (f.msi id).asserted = true := by
  have hel : (f.msi id).eligible id = true := by
    have hp := msi_makes_pending f id h1 h2
    have hie : (f.msi id).eie = f.eie := by simp [File.msi, h1, h2]
    have hth : (f.msi id).threshold = f.threshold := by simp [File.msi, h1, h2]
    simp only [File.eligible, hp, hie, hth, he, Bool.true_and]
    rcases ht with ht | ht <;> simp [ht]
  have hids : (f.msi id).ids = f.ids := by simp [File.msi, h1, h2]
  have hdel : (f.msi id).delivery = 1 := by simp [File.msi, h1, h2, hd]
  simp only [File.asserted, hdel, beq_self_eq_true, Bool.true_and, bne_iff_ne, ne_eq]
  intro h0
  have := top_none _ h0 id h1 (by omega)
  rw [hel] at this
  exact Bool.noConfusion this

/-- `eidelivery` = 0x40000000: the file acts as though it were 0. -/
theorem external_file_is_silent (f : File) (h : f.delivery = EXT) : f.asserted = false := by
  simp [File.asserted, h, EXT]

/-- `eidelivery` has no effect on `*topei` (§3.8.1). -/
theorem topei_ignores_delivery (f : File) (d : Nat) : ({ f with delivery := d }).top = f.top :=
  rfl

theorem warl_values (f : File) (v : Nat) :
    (f.setDelivery v).delivery = 0 ∨ (f.setDelivery v).delivery = 1 ∨
      (f.setDelivery v).delivery = EXT := by
  unfold File.setDelivery
  split
  · exact Or.inr (Or.inr rfl)
  · dsimp only
    have := Nat.mod_two_eq_zero_or_one v
    omega

theorem warl_guest_never_external (f : File) (v : Nat) (h : f.extOk = false) :
    (f.setDelivery v).delivery ≠ EXT := by
  have hd : (f.setDelivery v).delivery = v % 2 := by simp [File.setDelivery, h]
  rw [hd]
  have := Nat.mod_lt v (by decide : 2 > 0)
  unfold EXT; omega

/-! ## The hart's external interrupt at one level -/

/-- With an interrupt file at this level, `eidelivery` chooses the file (1)
    or the PLIC/APLIC output `wired` (0x40000000); without one, `wired`. -/
def hartLevel (f : Option File) (wired : Bool) : Bool :=
  match f with
  | none => wired
  | some f => f.asserted || (f.delivery == EXT && wired)

theorem level_file_on (f : File) (w : Bool) (h : f.delivery = 1) :
    hartLevel (some f) w = f.asserted := by
  simp [hartLevel, h, EXT]

theorem level_external (f : File) (w : Bool) (h : f.delivery = EXT) :
    hartLevel (some f) w = w := by
  simp [hartLevel, h, external_file_is_silent f h]

theorem level_off (f : File) (w : Bool) (h : f.delivery = 0) : hartLevel (some f) w = false := by
  simp [hartLevel, File.asserted, h, EXT]

/-! ## APLIC MSI addresses (§4.9.1) -/

/-- The MSI page number: `base + group·2^(hhxs+12) + hart·2^lhxs + guest`
    written with the field sizes `G = 2^(hhxs+12)` and `S = 2^lhxs`. The
    `base` (the PPN) has zeros in those fields. -/
def msiPage (base G S group hart guest : Nat) : Nat := base + group * G + hart * S + guest

theorem split_unique (S a b c d : Nat) (hS : 0 < S) (hb : b < S) (hd : d < S)
    (h : a * S + b = c * S + d) : a = c ∧ b = d := by
  have e1 : (S * a + b) / S = a := by rw [Nat.mul_add_div hS, Nat.div_eq_of_lt hb]; simp
  have e2 : (S * c + d) / S = c := by rw [Nat.mul_add_div hS, Nat.div_eq_of_lt hd]; simp
  have e3 : (S * a + b) % S = b := by rw [Nat.mul_add_mod, Nat.mod_eq_of_lt hb]
  have e4 : (S * c + d) % S = d := by rw [Nat.mul_add_mod, Nat.mod_eq_of_lt hd]
  rw [Nat.mul_comm a S, Nat.mul_comm c S] at h
  constructor
  · rw [← e1, ← e2, h]
  · rw [← e3, ← e4, h]

/-- Within the field widths (guest below `S`, the hart field below `G`),
    different (group, hart, guest) triples give different pages. -/
theorem msiPage_injective (base G S g1 h1 x1 g2 h2 x2 : Nat) (hS : 0 < S)
    (hx1 : x1 < S) (hx2 : x2 < S) (hh1 : h1 * S + x1 < G) (hh2 : h2 * S + x2 < G)
    (h : msiPage base G S g1 h1 x1 = msiPage base G S g2 h2 x2) :
    g1 = g2 ∧ h1 = h2 ∧ x1 = x2 := by
  unfold msiPage at h
  have h' : g1 * G + (h1 * S + x1) = g2 * G + (h2 * S + x2) := by omega
  have hG : 0 < G := by omega
  obtain ⟨hg, hr⟩ := split_unique G g1 (h1 * S + x1) g2 (h2 * S + x2) hG hh1 hh2 h'
  obtain ⟨hh, hx⟩ := split_unique S h1 x1 h2 x2 hS hx1 hx2 hr
  exact ⟨hg, hh, hx⟩

/-- QEMU 8.2.2 takes an S-level domain's LHXW from `smsiaddrcfgh`, where it
    is zero, so the hart field is masked to width 0: hart 1, guest 2 and
    hart 0, guest 2 share a page (with `S = 4`, two guest-index bits). -/
def qemuHart (lhxw hart : Nat) : Nat := hart % 2 ^ lhxw

theorem qemu_s_level_drops_hart :
    msiPage 0x28000 (2 ^ 24) 4 0 (qemuHart 0 1) 2 = msiPage 0x28000 (2 ^ 24) 4 0 0 2 ∧
      msiPage 0x28000 (2 ^ 24) 4 0 (qemuHart 2 1) 2 ≠ msiPage 0x28000 (2 ^ 24) 4 0 0 2 := by
  decide

/-! ## APLIC forwarding in MSI mode -/

structure Src where
  pending : Bool
  enabled : Bool
  /-- The page `target` names and the identity (EIID). -/
  page : Nat
  eiid : Nat

structure Msi where
  page : Nat
  data : Nat

/-- One forwarding pass (domaincfg.IE = 1, DM = 1): every pending and
    enabled source sends its MSI and its pending bit clears. -/
def forward : List Src → List Src × List Msi
  | [] => ([], [])
  | s :: ss =>
    let (rest, msis) := forward ss
    if s.pending && s.enabled then
      ({ s with pending := false } :: rest, ⟨s.page, s.eiid⟩ :: msis)
    else (s :: rest, msis)

theorem forward_pending (l : List Src) :
    (forward l).1.map Src.pending = l.map (fun s => s.pending && !s.enabled) := by
  induction l with
  | nil => rfl
  | cons s ss ih =>
    simp only [forward]
    split <;> rename_i h <;> simp [ih] <;> cases hp : s.pending <;> cases he : s.enabled <;>
      simp_all

theorem forward_count (l : List Src) :
    (forward l).2.length = (l.filter (fun s => s.pending && s.enabled)).length := by
  induction l with
  | nil => rfl
  | cons s ss ih =>
    simp only [forward]
    split <;> rename_i h <;> simp [h, ih]

theorem forward_targets (l : List Src) :
    (forward l).2 = (l.filter (fun s => s.pending && s.enabled)).map (fun s => ⟨s.page, s.eiid⟩) := by
  induction l with
  | nil => rfl
  | cons s ss ih =>
    simp only [forward]
    split <;> rename_i h <;> simp [h, ih]

/-! ## Guest interrupt files and the hypervisor CSRs -/

structure Hyp where
  /-- `files 0` is the hart's own S-level file; `1..geilen` are guests. -/
  files : Nat → File
  geilen : Nat
  hgeie : Nat → Bool
  vgein : Nat

def Hyp.validGuest (h : Hyp) (g : Nat) : Bool := decide (1 ≤ g ∧ g ≤ h.geilen)

/-- `hgeip` bit `g`. -/
def Hyp.hgeip (h : Hyp) (g : Nat) : Bool := h.validGuest g && (h.files g).asserted

/-- A write of `hgeie`: bits `geilen:1` are writable. -/
def Hyp.setHgeie (h : Hyp) (v : Nat → Bool) : Hyp :=
  { h with hgeie := fun g => h.validGuest g && v g }

def Hyp.sgeip (h : Hyp) : Bool := (List.range (h.geilen + 1)).any (fun g => h.hgeip g && h.hgeie g)

def Hyp.vseip (h : Hyp) : Bool := h.hgeip h.vgein

/-- `vsiselect`/`vsireg`/`vstopei` reach guest file VGEIN, or trap. -/
def Hyp.vsFile (h : Hyp) : Option File := if h.validGuest h.vgein then some (h.files h.vgein) else none

def Hyp.vsclaim (h : Hyp) : Option Hyp :=
  if h.validGuest h.vgein then
    some { h with files := fun g => if g = h.vgein then (h.files g).claim else h.files g }
  else none

theorem hgeip_iff (h : Hyp) (g : Nat) :
    h.hgeip g = true ↔ 1 ≤ g ∧ g ≤ h.geilen ∧ (h.files g).asserted = true := by
  simp [Hyp.hgeip, Hyp.validGuest, and_assoc]

theorem vseip_iff (h : Hyp) :
    h.vseip = true ↔ (h.vsFile.map File.asserted) = some true := by
  unfold Hyp.vseip Hyp.vsFile Hyp.hgeip
  cases h.validGuest h.vgein <;> simp

theorem hgeie_bit0 (h : Hyp) (v : Nat → Bool) : (h.setHgeie v).hgeie 0 = false := by
  simp [Hyp.setHgeie, Hyp.validGuest]

theorem sgeip_needs_hgeie (h : Hyp) (hz : ∀ g, h.hgeie g = false) : h.sgeip = false := by
  simp [Hyp.sgeip, hz]

theorem vs_access_iff (h : Hyp) :
    h.vsFile.isSome = true ↔ 1 ≤ h.vgein ∧ h.vgein ≤ h.geilen := by
  unfold Hyp.vsFile Hyp.validGuest
  by_cases c : 1 ≤ h.vgein ∧ h.vgein ≤ h.geilen <;> simp [c]

/-- A VS-level claim changes only the guest file VGEIN selects: the hart's
    own file (0) and every other guest's are untouched. -/
theorem vsclaim_isolated (h h' : Hyp) (hc : h.vsclaim = some h') (g : Nat) (hg : g ≠ h.vgein) :
    h'.files g = h.files g := by
  unfold Hyp.vsclaim at hc
  split at hc
  · cases hc; simp [hg]
  · simp at hc

theorem vsclaim_own_file (h h' : Hyp) (hc : h.vsclaim = some h') : h'.files 0 = h.files 0 := by
  have hv : h.validGuest h.vgein = true := by
    unfold Hyp.vsclaim at hc; split at hc <;> simp_all
  have : 0 ≠ h.vgein := by simp [Hyp.validGuest] at hv; omega
  exact vsclaim_isolated h h' hc 0 this

/-! ## `dma-ranges` translation -/

structure Win where
  dma : Nat
  cpu : Nat
  size : Nat

def Win.holds (w : Win) (a len : Nat) : Bool := decide (w.dma ≤ a ∧ a + len ≤ w.dma + w.size)

/-- Bus address `[a, a + len)` to CPU: the first window holding it. -/
def toCpu : List Win → Nat → Nat → Option Nat
  | [], _, _ => none
  | w :: ws, a, len => if w.holds a len then some (w.cpu + (a - w.dma)) else toCpu ws a len

def toBus (w : Win) (c : Nat) : Nat := w.dma + (c - w.cpu)

theorem toCpu_sound (ws : List Win) (a len c : Nat) (h : toCpu ws a len = some c) :
    ∃ w ∈ ws, w.dma ≤ a ∧ a + len ≤ w.dma + w.size ∧ c = w.cpu + (a - w.dma) ∧
      w.cpu ≤ c ∧ c + len ≤ w.cpu + w.size := by
  induction ws with
  | nil => simp [toCpu] at h
  | cons w ws ih =>
    simp only [toCpu] at h
    split at h
    · rename_i hw
      simp [Win.holds] at hw
      cases h
      exact ⟨w, by simp, hw.1, hw.2, rfl, by omega, by omega⟩
    · obtain ⟨w', hm, rest⟩ := ih h
      exact ⟨w', by simp [hm], rest⟩

theorem toCpu_none (ws : List Win) (a len : Nat) (h : toCpu ws a len = none) :
    ∀ w ∈ ws, ¬ (w.dma ≤ a ∧ a + len ≤ w.dma + w.size) := by
  induction ws with
  | nil => simp
  | cons w ws ih =>
    simp only [toCpu] at h
    split at h
    · simp at h
    · rename_i hw
      intro w' hm
      simp at hm
      rcases hm with rfl | hm
      · simpa [Win.holds] using hw
      · exact ih h w' hm

theorem roundtrip (w : Win) (a len : Nat) (h : w.holds a len = true) :
    toCpu [w] a len = some (w.cpu + (a - w.dma)) ∧ toBus w (w.cpu + (a - w.dma)) = a := by
  simp [Win.holds] at h
  simp [toCpu, Win.holds, h, toBus]

/-- Two bus levels: the device-side window `w` (bus address to the middle
    bus) and the parent level's entry `e` (middle bus to CPU), composed by
    intersecting `w`'s output with `e`'s input as `DeviceTree::dma_windows`
    does. -/
def compose (w e : Win) : Option Win :=
  let lo := max w.cpu e.dma
  let hi := min (w.cpu + w.size) (e.dma + e.size)
  if lo < hi then some ⟨w.dma + (lo - w.cpu), e.cpu + (lo - e.dma), hi - lo⟩ else none

theorem compose_sound (w e w' : Win) (a len : Nat) (hc : compose w e = some w')
    (h : w'.holds a len = true) :
    w.holds a len = true ∧ e.holds (w.cpu + (a - w.dma)) len = true ∧
      e.cpu + (w.cpu + (a - w.dma) - e.dma) = w'.cpu + (a - w'.dma) := by
  dsimp only [compose] at hc
  split at hc
  · rename_i hlt
    cases hc
    simp only [Win.holds, decide_eq_true_eq] at h ⊢
    refine ⟨?_, ?_, ?_⟩ <;> omega
  · simp at hc

/-! ## PCI `msi-map` (Linux `of_map_id`) -/

structure MapEntry where
  ridBase : Nat
  ctl : Nat
  msiBase : Nat
  len : Nat

def mapId : List MapEntry → Nat → Option (Nat × Nat)
  | [], _ => none
  | e :: es, m =>
    if e.ridBase ≤ m ∧ m < e.ridBase + e.len then some (e.ctl, e.msiBase + (m - e.ridBase))
    else mapId es m

def route (es : List MapEntry) (mask rid : Nat) : Option (Nat × Nat) := mapId es (rid &&& mask)

theorem mapId_sound (es : List MapEntry) (m c id : Nat) (h : mapId es m = some (c, id)) :
    ∃ e ∈ es, e.ctl = c ∧ e.ridBase ≤ m ∧ m < e.ridBase + e.len ∧
      id = e.msiBase + (m - e.ridBase) ∧ id < e.msiBase + e.len := by
  induction es with
  | nil => simp [mapId] at h
  | cons e es ih =>
    simp only [mapId] at h
    split at h
    · rename_i hm
      cases h
      exact ⟨e, by simp, rfl, hm.1, hm.2, rfl, by omega⟩
    · obtain ⟨e', he, rest⟩ := ih h
      exact ⟨e', by simp [he], rest⟩

theorem mapId_none (es : List MapEntry) (m : Nat) (h : mapId es m = none) :
    ∀ e ∈ es, ¬ (e.ridBase ≤ m ∧ m < e.ridBase + e.len) := by
  induction es with
  | nil => simp
  | cons e es ih =>
    simp only [mapId] at h
    split at h
    · simp at h
    · rename_i hm
      intro e' he
      simp at he
      rcases he with rfl | he
      · exact hm
      · exact ih h e' he

/-- Masking twice is masking once: requester IDs that differ only outside
    `msi-map-mask` route identically. -/
theorem mapId_mask_idem (es : List MapEntry) (mask rid : Nat) :
    route es mask (rid &&& mask) = route es mask rid := by
  simp [route, Nat.and_assoc, Nat.and_self]

/-! ## The hypervisor script of `tests/aia_qemu.rs` on this model

  Hart 0 of `virt,aia=aplic-imsic,aia-guests=2 -smp 4`: its S-level file
  (page 0) and guest files 1 and 2 (pages 1 and 2) of 255 identities.
  `FAULT` stands for the illegal-instruction trap (mcause 2) the probe
  prints. -/

def FAULT : Nat := 0xfa17000000000002

inductive Op where
  | hgeieW (v : Nat) | hgeieR | hgeip | hmip | vgein (n : Nat)
  | vsiW (isel v : Nat) | vsiR (isel : Nat) | vstopei | vsclaim
  | msi (page id : Nat) | siR (isel : Nat) | stopei

def emptyFile : File := ⟨255, false, 0, 0, fun _ => false, fun _ => false⟩

def hyp0 : Hyp := ⟨fun _ => emptyFile, 2, fun _ => false, 0⟩

/-- The `eip`/`eie` word `k` (RV64: even selectors only). -/
def word (b : Nat → Bool) (k : Nat) : Nat :=
  (List.range 64).foldl (fun acc i => acc + if b (64 * k + i) then 2 ^ i else 0) 0

def setWord (f : File) (eie : Bool) (k v : Nat) : File :=
  let g : Nat → Bool := fun j =>
    if 64 * k ≤ j ∧ j < 64 * k + 64 ∧ j ≠ 0 then Nat.testBit v (j - 64 * k)
    else if eie then f.eie j else f.eip j
  if eie then { f with eie := g } else { f with eip := g }

def iread (f : File) (isel : Nat) : Option Nat :=
  if isel = 0x70 then some f.delivery
  else if isel = 0x72 then some f.threshold
  else if 0x80 ≤ isel ∧ isel < 0xc0 ∧ isel % 2 = 0 then some (word f.eip ((isel - 0x80) / 2))
  else if 0xc0 ≤ isel ∧ isel < 0x100 ∧ isel % 2 = 0 then some (word f.eie ((isel - 0xc0) / 2))
  else none

def iwrite (f : File) (isel v : Nat) : Option File :=
  if isel = 0x70 then some (f.setDelivery v)
  else if isel = 0x72 then some { f with threshold := v % 256 }
  else if 0x80 ≤ isel ∧ isel < 0xc0 ∧ isel % 2 = 0 then some (setWord f false ((isel - 0x80) / 2) v)
  else if 0xc0 ≤ isel ∧ isel < 0x100 ∧ isel % 2 = 0 then some (setWord f true ((isel - 0xc0) / 2) v)
  else none

def topei (f : File) : Nat := f.top * 65536 + f.top

def setFile (h : Hyp) (g : Nat) (f : File) : Hyp :=
  { h with files := fun j => if j = g then f else h.files j }

def bits (p : Nat → Bool) (n : Nat) : Nat :=
  (List.range n).foldl (fun acc i => acc + if p i then 2 ^ i else 0) 0

/-- One op: the new state and the value printed, if any. -/
def step (h : Hyp) : Op → Hyp × Option Nat
  | .hgeieW v => (h.setHgeie (Nat.testBit v), none)
  | .hgeieR => (h, some (bits h.hgeie 64))
  | .hgeip => (h, some (bits h.hgeip 64))
  | .hmip => (h, some ((if h.sgeip then 0x1000 else 0) + (if h.vseip then 0x400 else 0)))
  | .vgein n => ({ h with vgein := n }, none)
  | .vsiW isel v =>
    match h.vsFile with
    | none => (h, some FAULT)
    | some f =>
      match iwrite f isel v with
      | none => (h, some FAULT)
      | some f' => (setFile h h.vgein f', none)
  | .vsiR isel =>
    match h.vsFile with
    | none => (h, some FAULT)
    | some f => (h, some ((iread f isel).getD FAULT))
  | .vstopei =>
    match h.vsFile with
    | none => (h, some FAULT)
    | some f => (h, some (topei f))
  | .vsclaim =>
    match h.vsFile with
    | none => (h, some FAULT)
    | some f => (setFile h h.vgein f.claim, some (topei f))
  | .msi page id => if page ≤ h.geilen then (setFile h page ((h.files page).msi id), none) else (h, none)
  | .siR isel => (h, some ((iread (h.files 0) isel).getD FAULT))
  | .stopei => (h, some (topei (h.files 0)))

def run (h : Hyp) : List Op → List Nat
  | [] => []
  | op :: ops =>
    let (h', out) := step h op
    out.toList ++ run h' ops

def hypScript : List Op :=
  [ .hgeieR, .hgeieW 0xffff, .hgeieR, .hgeip, .hmip,
    .vgein 1, .vsiW 0x70 1, .vsiR 0x70, .vsiW 0xc0 (2 ^ 5), .vsiR 0xc0, .vstopei,
    .msi 1 5, .vstopei, .hgeip, .hmip, .vsiR 0x80,
    .vsiW 0x72 5, .vstopei, .hgeip, .hmip, .vsiW 0x72 6, .hgeip, .vsiW 0x72 0,
    .hgeieW 0, .hmip, .hgeieW 6, .hmip, .vsclaim, .vstopei, .hgeip, .hmip,
    .vgein 0, .vsiR 0x70, .vstopei, .vsclaim, .vsiW 0x70 1, .hmip,
    .vgein 3, .vsiR 0x70, .vstopei,
    .vgein 2, .vsiR 0x70, .msi 2 7, .hgeip, .vsiW 0xc0 (2 ^ 7), .hgeip, .vsiW 0x70 1, .hgeip,
    .hmip, .vstopei,
    .vgein 1, .hmip, .vstopei,
    .siR 0x80, .stopei,
    .vgein 2, .vsiR 0x81, .vsiR 0x60, .vsclaim, .hgeip, .hmip ]

def hypTrace : List Nat := run hyp0 hypScript

/-- QEMU 8.2.2's output for this script (`HYPERVISOR_EXPECTED`). -/
def qemuHyp : List Nat :=
  [0, 6, 0, 0, 1, 0x20, 0, 0x50005, 2, 0x1400, 0x20, 0, 0, 0, 2, 0x400, 0x1400, 0x50005, 0, 0, 0,
   FAULT, FAULT, FAULT, FAULT, 0, FAULT, FAULT, 0, 0, 0, 4, 0x1400, 0x70007, 0x1000, 0, 0, 0,
   FAULT, FAULT, 0x70007, 0, 0]

theorem hypTrace_matches_qemu : hypTrace = qemuHyp := by decide +kernel

end Vesper.Silicon.Aia

# Mechanized metatheory: places, the Place Capability Graph, second-class borrows and Zero-Pin (#455)

Wave eight, lane 6 (design §3-§5 and the Zero-Pin corollary of §6, Appendix A).
Base: `origin/main` at `1094e649c95e4d81eef4e705d13882ab1339542d` (a descendant
of the required baseline `ea1ed29`). Branch `claude/project-thread-4oxh21`.

## What was built

* `lean/MetatheoryPlaces.lean` (new, about 3,100 lines, Lean 4 core only, no
  imports, no Mathlib, namespace `Vesper.Metatheory.Places`). A separate
  calculus, close to the Stage 3 Place IR that the PCG checker works on, with
  its type-safety, borrow-soundness, linear-exhaustion and Zero-Pin proofs:
  189 theorems, every one audited with `#print axioms`. Kernel report:
  47 theorems use no axiom, the rest only `propext`, `Quot.sound` and
  `Classical.choice`; no `sorry`, no new axiom.
* `tests/metatheory_places.rs` (new root integration test, Bazel target
  `//:test/metatheory_places`; the root package's test data already contains
  `lean/`, so no BUILD change). It always checks source hygiene (no `sorry`,
  `admit`, `axiom`, `unsafe`, `partial`, `native_decide`, `opaque`,
  `implemented_by`/`extern`, no imports, at least 150 theorems, no duplicate
  names, every theorem audited, the 30 headline theorems present). With
  `VESPER_LEAN` set it compiles a private regular-file copy (Bazel runfiles are
  symlinks), requires a clean compile with no warnings, and parses the kernel
  axiom report: every declared theorem is audited exactly once and depends
  only on `propext`, `Classical.choice` and `Quot.sound`. It follows
  `tests/metatheory_closures.rs`.
* This report.

No other paths were touched. There is no root patch.

## The calculus

| Spec item | In the model |
| --- | --- |
| Modes `copy ⊑ affine ⊑ linear` (§4) | `Mode`, `Mode.Le`, `Mode.join` |
| [K-Prim], [K-Adt] (no `@@` overrides) | `Ty.kind`: `nat` copy, `buf` affine, `hdl` linear, `pair` the join of its fields |
| Mutable owned locals and places `x.π` (§3 `PlaceTree`, §5) | a frame's `own : List Val`; `Place = root.path` with `Root.own x` or `Root.bor b`; field-tree paths `List Bool` |
| Second-class borrows `x :_m^μ τ`, `μ ∈ {shr, mut}` (§5 Γ) | borrow slots of a frame: `in`/`inout` callee parameters only. There is no reference type, no value holds a pointer, and no statement stores a borrow |
| The Place Capability Graph | `Pcg`: per owned local a capability tree `Sh` over its field tree (`full t`, `moved t`, or split into the two fields), per slot its mode and type. Statements are typed flow-sensitively, `TyS P G s G'` |
| discard(Γ) | `Sh.dropOK`, `Done`: a frame may only exit when every initialized leaf is at most affine |
| Context split ⊛ for owned values | moves: `move p` turns a place's capability to `moved`; a moved place cannot be read, moved, borrowed or passed again |
| [T-Var] through a borrow | `copy p` reads a copy-kind value through an owned root or any slot |
| Writes | `assign p := o` over a droppable old value (owned place or under a `mut` slot); `swap p q` exchanges two places of the same type (this is how a linear value under an `inout` is replaced) |
| [T-Match] arm agreement / PCG join (A.1 item 5) | `ifz`: both arms start from one PCG and must end in the same PCG |
| [T-CallProj] with `in`/`inout`/`sink` and reborrows `ν ≥ μ` | `call f ss bs`: `sink` arguments are owned initialized places and are moved; `in` arguments any initialized place; `inout` arguments an owned place or a `mut` slot; all argument pairs disjoint unless both are `in` (`AllCompat`). A reborrow of a caller slot is flattened, so every slot names an owned local of an older frame |
| [T-Abs] premise "no shr/mut borrows in Γ" | `spawn f ss` starts a fiber whose entry function has no borrow parameters: a fiber receives owned values only |
| [T-Let] Resumption Frame Condition for a linear effect | `yield` (an `Async`/`Storage` suspension, `res = linear`) admits every live binding, linear ones included |
| Linear obligations, Theorem 2 | `mint` (operand) and `consume`, a store of live handle ids |
| Stack/frame model, suspended continuations and their state | a configuration is a pool of fibers; a fiber is a stack of frames (owned locals, slots, remaining code), a base address and a suspended flag. `yield` suspends a fiber with its whole stack; `resume` restarts it, after relocating it to an arbitrary new base |

A borrow slot holds a fiber-relative location (owning frame position, local,
path). The machine semantics never reads a fiber's base address.

## Theorems

All statements are about the Lean model above, not about the Rust compiler,
its checker or the native fiber ABI.

* **Type safety** (Theorem 1 for this calculus): `preservation`,
  `preservation_steps`, `progress` and `type_safety`. The invariant `CfgOK`
  says: the program is well typed; in every fiber each frame's values agree
  with its PCG, its slots are sound and its remaining code is typed to a
  discardable exit; the stack is borrow-sound; and each live handle is held by
  exactly one owned place while no dead handle is held anywhere. Progress: a
  typed configuration has finished (every stack empty) or steps; `consume`
  always finds a live handle, a branch a number, a call its callee.
* **Borrow soundness** on every reachable state (`BorrowSound`, part of the
  invariant): if two distinct slots overlap and one is `mut`, they are in
  different frames, the older one is `mut` and its place contains the younger
  one's (a reborrow chain whose older end is frozen while the younger frame
  runs). Corollaries: `mut_borrows_exclusive`, `same_frame_mut_disjoint` (two
  live `mut` borrows of one frame never overlap), `shr_never_aliases_mut`, and
  `running_frame_unaliased` (no slot anywhere names a place of the running
  frame). **No borrow outlives its place**: `no_dangling_borrow`, every slot
  of the frame at position `i` names an existing owned local of a frame at
  position `pos < i` of the same fiber, initialized at the slot's type.
* **Theorem 2 (total linear exhaustion)**: `linear_held_exactly_once` (at every
  reachable point the number of copies of handle `r` held in owned places is 1
  if it is live, else 0), and `total_linear_exhaustion`: when a run of a
  well-typed program from `initCfg` finishes, no minted handle is still live.
  Since consuming needs a live handle and kills it, each minted handle was
  consumed exactly once.
* **Zero-Pin corollary**: `zero_pin_no_self_reference`, a suspended fiber's
  stack contains no pointer into itself (values have no pointer at all, and
  every slot of frame `i` points into a strictly older frame of the same
  fiber, never into its own frame, a younger one or another fiber), so the
  stack is position independent. `fstep_reloc`: stepping commutes with
  relocating a fiber to a new base. `zero_pin_relocation`: relocating any
  fiber keeps the whole configuration typed, borrow-sound and in agreement
  with the store. Resumption itself relocates (`Step.resume` takes any base),
  so every theorem above already covers pooled, moved fibers.
* Worked examples (`Examples`): a seven-function program (`inout` write,
  swapping a linear value through an `inout`, an `in` read, two disjoint
  `inout x.{0, 1}` projections, a suspension with a live linear field, a
  spawned fiber taking a handle by `sink`) is typed (`prog_ok`) and every
  finished run of it exhausts its handles (`main_exhausts`). Rejected:
  overlapping `inout` arguments, a `sink` overlapping a borrow, spawning a
  function with a borrow parameter, moving out of a borrow, writing through an
  `in` borrow, overwriting a linear value, a function exit with a live handle,
  and branch arms that disagree on consuming a linear value.

## Which §3-§5 rules are covered

Covered: the `in`/`inout`/`sink` pass modes and `PlaceTree` field projections
of §3; the mode lattice, [K-Prim] and [K-Adt] without `@@` overrides of §4;
discard(Γ), second-class `shr`/`mut` borrows in Γ, ownership transfer for
affine/linear values (as PCG moves), [T-Var] through borrows, [T-CallProj]
with disjointness, reborrows `ν ≥ μ` and `sink` consumption, the uniform-arm
condition of [T-Match] for a two-way branch, the no-borrow premise of [T-Abs]
for fiber entries, and the Resumption Frame Condition for linear (`res =
linear`) suspensions of §5. From §6: Theorem 1 and Theorem 2 for this
calculus and the Zero-Pin corollary.

Not covered here (they are in `lean/MetatheoryFull.lean` and
`lean/Metatheory/Closures.lean`, or remain gaps): [K-Var], [K-Fn], `@@`
field-modality overrides, [T-SubMode], [T-Cross], first-class closures and
the capture premise of [T-Abs], [T-Let] with abortable effects (`Fail`,
`res = affine`), checked arithmetic aborts, effect handlers with
continuations as values, [T-Match] over general ADTs, and SemVer narrowing.

## Decisions

* A separate, flow-sensitive statement calculus instead of extending the
  substitution calculi: mutable places and borrow slots need a store and a
  stack, and the PCG checker (§7 Stage 3, vesper-mir-pcg) is flow-sensitive
  over Place IR. The file has no imports and its own namespace, so lane 5's
  calculus can import it later; lane 5's files were not touched.
* Borrows exist only as callee parameters, for the duration of a call. Loans
  are therefore implicit in the stack: the lender is suspended while the
  borrower runs, which is why borrow soundness is stated as a reborrow-chain
  property over the whole stack.
* Reborrows are flattened at call time, so a slot names an owned local
  directly. A `swap` statement replaces linear values under an `inout`
  (nothing can be moved out of a borrow).
* Zero-Pin is formalized with fiber-relative slots: the substantive part is
  the invariant that every slot points strictly down its own fiber's stack
  (which is what makes that encoding total); relocation commutation then
  follows. This is a model of the language rule, not of the native
  stack-switching ABI or `fiber-abi` stack images.
* Functions return through `inout` parameters (no return values); loops are
  not modelled (no termination claim is needed for safety).

## Checks

* `lean lean/MetatheoryPlaces.lean` with the Lean 4.24.0 release `.zip`
  (elan is blocked in the cloud): clean, no warnings, about 9 s.
* `VESPER_LEAN=…/lean-4.24.0-linux/bin/lean cargo test --locked --test metatheory_places`:
  2 passed (hygiene; compile plus axiom report of all 189 theorems).
* `cargo fmt --all -- --check`: clean.
* `cargo clippy --locked --test metatheory_places -- -D warnings` (Rust 1.97 locally): clean.
* The full `cargo test --workspace` and workspace Clippy were left to CI (Bazel
  `//...` on Rust 1.100, macOS and Windows): the change adds one Lean file, one
  root test and this report, and touches no Rust library code.

## Remaining gaps

* The items listed as not covered above.
* `scripts/ci/check-lean-models.sh` is not this lane's path; the Rust test
  already compiles and audits the file in CI when `VESPER_LEAN` is set.
* No connection to the Rust PCG checker: nothing here is a theorem about
  `src/` or any crate.

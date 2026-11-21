# Native code after callbacks (SH5 lane 7)

Claim #1330, the three gaps SH4 lane 4 left
([self-host-callback-registers.md](self-host-callback-registers.md), #1317,
follow-up of #1271). Stacked on #1317 (head `7e4f47e`); #1317 merged as
`a8828f2` and the branch merged origin/main `b0c3dd1`. Native optimize path
only: the unoptimized path, the verified backend and the Stage 1 lock are
unchanged.

## What changed

### 1. Small enums are words (`src/native_aggregates.rs`)

`scalar_enums` rewrites a graph before the native backend lowers it. An
enum whose fields are all integer or `bool` words, at most four words with
its tag (`Option[u64]`, `Result[u64, u32]`, `enum Pick { A(u64), B(u32,
bool), C }`), becomes its words: the tag (the variant index, the same value
memory holds) and one word per field of every variant.

- A constructor becomes its tag constant and its field values (zero
  constants for the other variants' words).
- A block parameter becomes one parameter per word, a local one local per
  word (so `register_locals` keeps them in registers), a `let` binds each
  word and a whole read reads each word.
- A `match` tests the tag (integer literal arms, the last arm a wildcard),
  and an arm's field binding binds the field's word.
- A call of a function the native path specializes into its caller (a
  `with_ref`-style method with a lent callback, or any effectful function)
  passes the words of its result to its caller's join as block parameters
  instead of an address: the caller asks for words, the specialized-call
  path gives the join one parameter per word, and the callee's `return`
  jumps there with its words. A `return` of a value the callee keeps in
  storage (an early `return`, a value that passed through a call), and a
  call the native path does not specialize after all, read the words back
  from storage (`enum_words`), so either side may keep storage.

A value qualifies only when every definition is one of those (constructor,
whole read of a qualifying local, specialized call, block parameter whose
every incoming edge passes a qualifying value) and every use is one of those
(binding, block argument, `match`, field binding, `return` to a join that
takes words). Anything else (a field path, a write, a place argument, a
closure or handler that names the local, a call argument, a non-specialized
`return`) keeps the old lowering for the whole connected set. Graphs with
`Box` values or FIP slots keep the old lowering (their drop plans index
instructions). A set of values that nothing takes apart (no `match`, no
field binding) is left alone.

### 2. Invariant receiver headers across callbacks (`src/bounds_hoist.rs`, `src/bounds_hoist/native.rs`, `src/native.rs`)

`with_ref` is specialized into its caller's loop as a nested graph, and that
graph read `self.vals`, `self.slots`, `self.hashes` and `self.keys` (each
handle, then its length and data words) on every call.

- An aggregate `in` parameter of the function being emitted that its graph
  never rebinds (and that no `handle` in the graph can see) is an invariant
  root: nothing can change its memory for the whole call (§5, `in` is a
  shared borrow). A whole read of an invariant root is an invariant pointer,
  and a specialized callee's `in` parameter bound to an invariant pointer is
  an invariant root of the callee's graph.
- `stable_views` (the entry hoists of `in` parameter fields) no longer gives
  up on a graph that contains a closure, an indirect call or a method call
  (`::`): under an `in` borrow nothing can write, move or resize the field,
  whatever code runs. Only a `handle` (which shares the graph's names) and,
  for whole `inout` and `in` sequence parameters, the old rule remain.
- A nested graph keeps its entry hoists whose root is an invariant root, and
  reads them with `seq_invariant_field_view`: the handle word and its header
  words are read-only, non-trapping, movable loads, which Cranelift moves
  out of the caller's loop.

### 3. A cost rule for `in` homes on impure calls (`src/native.rs`, `register_locals`)

A real (not specialized) call with an effect row homed every place it
borrows. Its `in` scalar places are values (nothing writes them back), so
their home is a cost question. `keeps_in_home` keeps the home only for a
name lent `in` inside a loop that also computes with it (loop-weighted
loans of at least one loop iteration's weight and other reads or writes).
Every other such name is a register.

Rules measured (callgrind Ir per pass, against the old rule):

| Rule | Aegis snp | tdx | sgx | policy | Silicon dtb-parse | dtb-platform | Grammar pb | der |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| No home for any `in` loan (#1317's experiment) | −4.79% | −8.28% | −6.71% | +0.89% | +0.33% | +0.21% | −7.56% | −13.25% |
| Home unless other uses outweigh loans | −3.60% | −5.91% | −5.41% | −0.09% | +0.72% | +0.46% | −1.58% | −2.95% |
| Home for any loan inside a loop | −4.79% | −8.28% | −6.71% | +0.95% | 0.00% | 0.00% | −2.66% | −6.82% |
| Register budget of 4 per function | +0.99% | −0.48% | −0.38% | +0.90% | +0.88% | +0.57% | −2.58% | −2.82% |
| Home only when no other use | −1.81% | −4.25% | −3.42% | −0.05% | −0.03% | −0.02% | −1.59% | −10.59% |
| **Home for a loop loan with other uses (shipped)** | **−4.79%** | **−8.28%** | **−6.71%** | **−0.09%** | **−0.03%** | **−0.02%** | **−5.35%** | **−11.10%** |

Aegis `policy`'s +0.9% is one function (`check_policy`, 261 blocks) and a
threshold, not a per-name cost: dropping the homes of any one of its three
groups of names (one name read in its loops, three loop names, four
straight-line names) saves 0.06-0.09%, and dropping any two costs 0.9%,
which is where Cranelift's register allocator starts spilling in the loop.

### flat_callbacks

The `flat_callbacks` drivers of `tests/closure_modes_collections.rs`, built
as executables and run under callgrind (Ir of the driver, origin/main
against this branch):

| Part | Callback form, main → branch | Callback form against the element-view form |
|---|--:|--:|
| `FlatTable` | −1.33% | +0.37% (was +1.70% after #1317) |
| `CacheBTree` | −4.07% (the view form −3.59%) | +0.18% (was +0.57%) |
| `IdxMap` (`look_o`/`look_c`) | −7.55% / −8.92% | −0.19% |

All three parts are within the 0.5% bar.

### Ports

Every port object was rebuilt with origin/main (`b0c3dd1`) and with this
branch (`vesper build --lib --target x86_64-unknown-linux-gnu`). 29 of the 47
units change; chorus, forge, highlight, lean-syntax, loom-audit,
loom-calculus, loom-solver, pact, the port build helper, pilot
arrays/gauge/kit, prism connection/router/target, quarry, semver and
warp-bbr are byte-identical. For each changed port with a benchmark, the
ABBA example (or callgrind test) of the port arm was run under callgrind at
two pass counts; the figure is Ir per pass (or per batch), main against this
branch.

Ports that move (|change| at least 0.1%):

| Port | Workload | main | branch | Change |
|---|---|--:|--:|--:|
| Aegis | snp | 34,710 | 33,049 | −4.79% |
| Aegis | tdx | 42,995 | 39,434 | −8.28% |
| Aegis | sgx | 23,309 | 21,745 | −6.71% |
| Grammar | pb | 113,594 | 107,512 | −5.35% |
| Grammar | der | 93,850 | 83,433 | −11.10% |
| Kinetic | colorer | 7,821,847 | 7,334,772 | −6.23% |
| Kinetic | multi-colorer | 5,136,510 | 4,910,790 | −4.39% |
| Kinetic | pile-lbvh | 109,315,375 | 106,471,555 | −2.60% |
| Kinetic | lbvh | 25,805,253 | 25,580,223 | −0.87% |
| Kinetic | scene | 90,100,934 | 89,969,464 | −0.15% |
| eBPF | decode | 5,320,041 | 5,040,505 | −5.25% |
| eBPF | interp | 26,058,890 | 25,037,908 | −3.92% |
| eBPF | ext | 967,274 | 949,474 | −1.84% |
| eBPF | verify | 647,398 | 638,849 | −1.32% |
| eBPF | scalar | 8,743,365 | 8,734,128 | −0.11% |
| Silicon | aia-intc | 4,134,423 | 3,960,839 | −4.20% |
| Silicon | imsic | 105,607 | 105,344 | −0.25% |
| Lint | corpus | 114,461,874 | 110,500,362 | −3.46% |
| Lint | policy | 29,775,298 | 29,745,280 | −0.10% |
| Serde | parse_simd | 34,160,233 | 33,350,229 | −2.37% |
| Serde | tiles | 115,232,182 | 113,686,048 | −1.34% |
| Serde | parse_scalar | 39,352,836 | 39,076,825 | −0.70% |
| Warp | frames | 8,288,963 | 8,179,395 | −1.32% |
| Warp | ack | 8,877,516 | 8,789,197 | −0.99% |
| Warp | transfer | 3,695,483 | 3,665,504 | −0.81% |
| Warp | recovery | 6,726,417 | 6,715,153 | −0.17% |
| Simpulse | all four workloads | 63,448,683 | 63,094,488 | −0.56% |
| prism-h3 | decode | 33,970,360 | 33,822,984 | −0.43% |
| prism-h3 | encode | 15,720,023 | 15,690,638 | −0.19% |
| Prism | all four workloads | 3,062,487 | 3,052,087 | −0.34% |

Within ±0.1% (the largest increases are Diagnostics `prepare` +0.09%,
eBPF `core` +0.09%, fmt `corpus` +0.03% and Aura +0.02%): Aegis `policy`
−0.09%, Silicon `dtb-parse` −0.03%, `dtb-platform` −0.02%, `aplic-direct`
−0.06%, Alloc, Aura, Bench, Crucible, Diagnostics, eBPF `concrete`/`btf`,
Flux, fmt, Ignis, Kinetic `morton`/`fastmath`/`crmath`/`gjk`/`sap`,
prism-h3 `frames`/`messages`, Resonance, Serde `f64_decode`/`f64_encode`,
Span, Warp `buffers`, warp-tcp and warp-wheel. No port regresses by more
than 0.2%.

Not measured: Beacon, the port pilot fixtures, std-driver and Tensor change
objects but have no port benchmark (Tensor's `throughput` example does not
link the port).

## Tests

`tests/native_after_callbacks.rs`:

- `option_results_stay_words`: the `flat_callbacks` shape and its
  hand-written `Option` form keep no 16-byte slot when optimized (and keep
  it unoptimized); all three forms agree.
- `callback_receivers_are_invariant`: the optimized callback loop reads its
  receiver with read-only loads, like the element-view form; the
  unoptimized path has none.
- `exits_and_stored_results_reach_the_join`: an early `return` of the
  result, a result that passes through a call, and a caller that passes the
  result on whole.
- `multi_variant_enums_stay_words`: an enum with two payload variants
  (`u32` and `bool` fields), wildcard patterns and a catch-all arm, joined
  through `if`/`else if` and `match` arms and locals, into a `Result`: the
  optimized function keeps no stack slot.
- `raising_callbacks_and_whole_results`: a callback that raises out of a
  lookup whose result is words, caught by a handler around the loop.

Every test is differential: the evaluator, the JIT with the optimizer on and
off (with the sequence double-free counter), x86-64 and AArch64 objects and
an executable.

Run locally, all passing: `native_after_callbacks`, `callback_registers`,
`closure_modes`, `closure_modes_collections`, `metatheory_closures`,
`language`, `seq`, `borrow_paths`, `borrowed_box_match`, `dyn_box`,
`dyn_effects`, `dyn_fn`, and 29 `native_*` suites (`aggregates`,
`ascii_in_place`, `batch`, `block_param_homes`, `bounds_facts`,
`byte_classes`, `drop_glue`, `empty_seq`, `export_handle_seqs`,
`fail_exit_drops`, `fail_status`, `handler_loops`, `handlers`,
`hoist_calls`, `hoist_headers`, `in_params`, `inactive_payload`, `inline`,
`inline_cost`, `inline_inout_slice`, `inline_reach`, `seq_access`,
`seq_drop`, `short_scans`, `simd_bytes`, `slice_headers`,
`specialized_sink`, `sroa`, `write_in_place`). The full suite runs in CI.
`cargo +1.100.0 clippy --all-targets -p vesper` is clean.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `src/ir_opt*`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. Every
new pass runs only on the native optimize path (`operands_only`): the
scalar-enum rewrite, the invariant roots (`return_to.is_none() &&
operands_only`, inherited by nested graphs only from invariant pointers) and
`header_hoists`/`stable_views` (called only from `src/native.rs` on that
path). The verified pipeline does not reach them. The `bootstrap-stage1`
workflow is run on the branch (see the PR).

## Compiler gaps

1. **Enum words stop at a boundary that is not a specialized call.** A
   function compiled once that returns `Option[u64]` writes it through its
   result pointer, and its caller's `let` and `match` read it from storage
   (no word ABI for small enum results). Same for an enum passed to a call,
   stored in a field, or compared with `==`:

   ```vesper
   fn find(in xs: Slice[u64], k: u64) -> Option[u64] { .. }   // compiled once
   let r = find(in xs, k);        // r lives in memory
   ```

   Fix: return small enums in registers in the export and internal ABIs
   (the way `register-status Fail` results already do), then let
   `scalar_enums` accept such calls like specialized ones.
2. **Only integer and `bool` fields, at most four words.** `Option[f64]`,
   `Option[(u64, u64)]` and `Option[State]` (an aggregate payload) keep
   storage. Floats need a zero constant of the right width in the rewrite
   (`Literal::Float` lowers by the span's type); aggregate payloads need the
   split-local machinery (`split_words`) for variant payloads.
3. **The callback form is still 4 Ir per lookup over the element view.**
   The join after the specialized call branches on the tag again (the arms
   already knew the variant): a jump-threading or case-of-known-constructor
   pass on the native IR would remove it. With the view form that is the
   rest of the 0.37% (`FlatTable`).
4. **Invariant roots are whole `in` parameters.** A lookup through a field
   (`self.table.with_ref(..)` of `in self`) or a `let`-bound borrow is not
   an invariant root, so its headers are read on each call:

   ```vesper
   fn f(in s: Wrapper, n: u64) -> u64 {
       for k in 0..n { acc = mix(acc, or0(s.table.with_ref(k, |in v| weight(in v)))); }
   }
   ```

   Fix: track invariant pointers through `in` field reads of an invariant
   root (`Read { path: [..] }` of an aggregate field).
5. **The `in`-home rule is empirical.** It encodes where Cranelift's
   register allocator starts spilling in Aegis `check_policy` and Silicon
   `dtb_parse`, not a model of register pressure. A pressure estimate per
   loop (names live across calls in the loop against callee-saved
   registers) would replace it.

## What is left

- The acceptance bar is met: every `flat_callbacks` part is within 0.5% of
  the view form (`FlatTable` +0.37%, `CacheBTree` +0.18%, `IdxMap`
  −0.19%), and no port regresses by more than 0.2% Ir (largest increase
  +0.09%), while Aegis, Grammar, Kinetic, eBPF, Silicon `aia-intc`, Lint,
  Serde and Warp drop 1-11% on their callback and enum-heavy workloads.
- Gaps 1 to 5 above.

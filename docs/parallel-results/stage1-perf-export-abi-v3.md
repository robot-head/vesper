# Stage 1 perf lane 4: export ABI v3 (#934)

Baseline: `main` at 943d901. Branch `claude/project-thread-j4qaow`, PR #940.

## What changed

- **`vesper.native.export-abi/3`.** A library opts in with the source
  directive `// @export abi 3` (`src/native_export/directives.rs`; any other
  `abi` value, or naming it twice, is an error). The object is the v2 object
  byte for byte (`tests/native_export_v3.rs`
  `v3_objects_carry_the_v2_runtime`). Only the Rust side changes.
- **Mirrors.** The binding (`src/native_export/binding_v2.rs`) emits
  `pub mod repr` with one `#[repr(C)]` mirror per copy struct, in the Vesper
  word layout:

  | Vesper field | Mirror cell |
  | --- | --- |
  | `i64`, `u64`, `isize`, `usize`, `f64` | the plain Rust type |
  | narrower integers, `bool`, `Unit`, field-less enums | `vesper_port_abi::Word<T>` (canonical word, private, written only by `new`/`set`) |
  | a nested copy struct | its mirror |
  | anything else (`Option`, `Result`, tuples, enums with payloads) | `vesper_port_abi::Words<T, N>`, converted on access |

  Each mirror gets compile-time `size_of`/`align_of`/`offset_of!` asserts for
  every field, `unsafe impl vesper_port_abi::Mirror`, and
  `from_value`/`to_value`.
- **In place.** `in`/`inout` parameters of a mirrored struct are
  `&repr::T`/`&mut repr::T`, passed as a pointer to the caller's storage.
  `in`/`inout Slice[T]` of one are `&[repr::T]`/`&mut [repr::T]`, lent by
  `with_mirror_slice[_mut]` with no heap copy. A mirrored result is returned
  as `repr::T`. `sink` takes `repr::T` by value and passes a copy (Vesper may
  reuse a consumed value's storage). Vesper cannot write an `in` parameter
  (`E0307`), and `tests/gauge.rs` checks that an `in` mirror and an `in`
  slice of mirrors are unchanged after calls that copy and modify them.
- **Converted.** Copy enums and tuples keep the v2 conversion. The interface
  records it per type (`"mirror": {"kind": "converted", "reason": ..}`), and
  every in-place parameter carries `"in_place": true`.
- **Aliasing.** An `inout` mirror and a slice over the same storage cannot
  reach one call: the wrapper takes `&mut T` and `&[T]`, and the borrow
  checker refuses the pair (a `compile_fail` doctest in `vesper-port-abi`).
- **v1/v2 unchanged.** The committed pilot (v1), kit (v2) and BBR (v2)
  bindings still match what the compiler writes, and a v2 interface has no v3
  keys (`v2_libraries_are_unchanged`).
- **Pilot.** New v3 library `gauge` (`vesper/gauge.ves`, `src/gauge.rs` with
  a Rust build in `src/gauge/reference.rs`, `port/gauge.rs`, `tests/gauge.rs`,
  Bazel `:gauge_vesper` and `:port_gauge_test`). `Meter` and `Samples` hold
  the mirrors in the Vesper build and the Rust values in the Rust build, the
  pattern a port follows. The tests cover every cell at its width's edges,
  canonical words written by Vesper, Rust writes seen by Vesper, slices read
  and updated in place (same buffer pointer), and zero allocations per call.
- **Docs.** `docs/maintenance/porting-a-crate.md` gains "Export ABI v3: state
  lent in place". The manuscript §16.6 gains one paragraph on v3 (spec change,
  flagged in the PR); the PDF is not rebuilt here because `website/**` is
  outside this lane.

## Measurements

Setup: cloud x86-64 container, 4 cores, release profile, ports compiled by
this branch's `vesper` (debug build; the object does not depend on how the
compiler itself was built).

### BBR (`crates/vesper-warp-bbr/examples/ack_path_abba.rs`)

Before (v2 binding as on `main`), callgrind over warm-up plus 20,000 steps:
251.4M instructions, of which 70.0M (27.8%) are `Abi::put`/`Abi::get` of
`Bbrv3`, `RateSample` and the event structs, and 5.1M (2.0%) `memcpy`. This
confirms the plan's "about 25%".

After: a local experiment, not pushed. The same object, a v3 binding of the
same source (`// @export abi 3` prepended), and the steady-state loop of
`ack_path_abba` driven on `repr::Bbrv3` with the `on_ack` call protocol of
`src/bbr/forward.rs`, with sent-packet slots stored as mirrors. Callgrind:
163.1M instructions (-35%). The only `Abi` frame left with a measurable cost is the `Result<PacketDeliveryState, BbrFault>` result of `record_send` (0.3%); the `Bbrv3` conversions run once, at construction.

ABBA, 20 epochs of 200,000 steps, two runs each, Rust build as arm A:

| Vesper arm | 95% interval of the median ratio (run 1) | (run 2) |
| --- | --- | --- |
| v2 (as on `main`) | 11.3x - 15.6x | 11.8x - 14.8x |
| v3 mirror (experiment) | 7.7x - 9.8x | 9.2x - 10.8x |

The wall-time gain (about 13x to about 9x) is smaller than the instruction
gain, because the conversion code is simple, highly parallel loads and
stores. What remains is Vesper code itself (`vesper_function_*`,
`windowed_max_update`, `record_send`): inlining and Fail lowering (lanes 1
and 5).

To adopt v3, BBR's Vesper build has to keep `repr::Bbrv3` as its state, and
its 19 field getters must read mirror cells (`.get()` on `Word` fields). That
is the follow-up on the port; this lane does not change BBR.

### Quarry (`crates/vesper-quarry/examples/planner_abba.rs`, PR #930)

Quarry's six entry points take `&[u64]`/`&[u8]` word streams, which v2
already lends in place, and return one handle `Q`. No copy struct crosses as
a parameter, so v3 changes no wrapper: with `// @export abi 3` on the PR
branch (merged with this branch, locally) the binding only gains the `repr`
module, and the wrapper functions are identical. Its 15-20% encode/decode is
the Rust-side word-stream codec in `src/port.rs`, a format choice of the port.

| Port arm (9 epochs, 200 passes) | median regression |
| --- | --- |
| v2 (PR branch as is) | +36670 bp (4.7x) |
| v3 directive | +33136 bp (4.3x) |

The difference is noise: both arms run the same machine code. Moving
Quarry's codec to slices of mirrored copy structs (plan nodes as
`Slice[Node]` instead of prefix word streams) would let v3 remove it; that is
a redesign of the port's boundary, left to the port.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `src/ir_opt.rs`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed
(`git diff --stat origin/main`). The verified backend has no export ABI, and
the v3 object equals the v2 object, so no native code changed either.

Evidence:

- The `bootstrap-stage1` workflow on this branch (run 44169342354, head
  8f5d1a8) passed: two clean builds, the ratchet against the committed lock,
  and the strict cross-backend gate ("committed lock is strict; the fresh
  lock must be too", 11 programs).
- Locally, `bootstrap/run.sh stage1 --reproduce --outcome` on this branch and
  on `origin/main` (same host, Rust 1.100.0) give identical fresh locks for
  all 11 programs: every `programs.verified.artifact` and every
  `cranelift_artifact` digest is equal, and so is `frontend_sources`. Only
  `seed_binary` differs, because the seed compiler binary includes this
  branch's code.
- On this cloud host both runs report the same 22 stale fields against the
  committed lock (the Cranelift and verified digests of 10 programs, all but
  `lexer-byte-classes`). That is a host difference that `main` shows too, not
  this change; on the GitHub runner the comparison is clean.

## What is left

- Ports adopt v3: BBR keeps `repr::Bbrv3` as its Vesper-build state (the
  experiment above shows the gain); Quarry would need mirrored node slices
  instead of word streams.
- Tuples and copy enums still convert. A tuple could get a named mirror; an
  enum would need a validity-checked view.
- `ChunkSeq[T]` of mirrored structs (by-value sequences, results) still
  converts element by element; `in`/`inout` slices are the in-place form.
- Mirrored structs inside handle fields are read through the v2 view.

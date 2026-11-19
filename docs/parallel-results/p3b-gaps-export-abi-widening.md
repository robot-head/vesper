# P3b gaps lane 2: export ABI widening (#1087)

Base: `9305b24` (origin/main, the merged wave plan #1098). Prompt:
[`docs/agent-prompts/p3b-gaps/02-export-abi-widening.md`](../agent-prompts/p3b-gaps/02-export-abi-widening.md).

The four boundary gaps from the P3a/P3b port reports are closed in the
binding generator, the drop glue and `vesper-port-abi`. Native code
generation is unchanged except for the drop glue of the new handle field
forms: a v2 or v3 object that compiled before is byte for byte the same
(`v3_objects_carry_the_v2_runtime`, and the lent-handle test compares the v2
and v3 objects of the silicon repro).

## What changed

| Gap | Before | After |
|---|---|---|
| 1. `f32` (resonance gap 5, kinetic gap 4) | `pub fn f(x: f32) -> f32` planned, but the binding declared `fn vesper_t_f(x: i64) -> i64` and `f32` had no `Abi`, so the binding did not compile. Ports passed `u32` bits. | `f32` parameters and results are `f32` in the `extern "C"` declarations, so they travel in float registers as the Cranelift signature already did (`xmm`/`s` registers on System V, Windows x64 and AAPCS64). `vesper-port-abi` implements `Abi` for `f32` (the low half of its word, the high half ignored on read). `Option[f32]`, `Fail` payloads, mirror fields (`Word<f32>`), `Slice[f32]` and `ChunkSeq[f32]` follow. v1, v2 and v3 bindings all do this. |
| 2. `in` handles copied (silicon gap 1) | `let mut arg_a = a.words;` then a pointer to the copy, per call. | v3 bindings pass `a.words.as_ptr().cast_mut()`: the caller's own words, as `in` mirrors already are. The interface marks the parameter `in_place`. `inout` and by-value handles and every v2 binding are unchanged. |
| 3. Sequences of handles (silicon gap 3) | `ChunkSeq[Imsic]` in a handle field: "only sequences and slices of copy elements cross the boundary". | A handle type's field may be a `ChunkSeq` of non-copy elements: other handle types, `String`s, boxes, `Option`s of those, and sequences of them. The binding's field view is `vesper_port_abi::OpaqueSeq` (its length only). The drop glue walks the elements (one loop per field, the element's own glue for each) before it frees the buffer, and skips a 0 handle (an emptied slot). Such a sequence never crosses on its own: as a parameter or result it is refused with "has non-copy elements (handles, boxes or strings); such a sequence crosses only inside a handle type". Slices of non-copy elements stay refused. |
| 4. `Box[dyn Trait]` (resonance gap 4) | "type dyn P has no fixed word layout". | A new interface type `AbiType::Object { name }` is the pointee of a `Box` inside a handle. Its field view is `vesper_port_abi::OpaqueObject` (whether the slot is full). The drop glue loads the block's vtable, calls its drop word on the value at offset 8 when it is not 0, and frees the block, as `emit_drop_boxed` does in native code. A null box (moved from) drops nothing. |

Files: `src/native_export.rs`, `src/native_export/{binding,binding_v2,glue}.rs`,
`crates/vesper-port-abi/src/{lib,seq}.rs`. The pilot gains a fourth
library, `crates/vesper-port-pilot/vesper/widen.ves` (Vesper build only,
committed binding `port/widen.rs`, Bazel target `:widen_vesper`, test
`tests/widen.rs`). One expected message in `tests/native_export_v2.rs`
follows the new refusal text.

The five committed v3 bindings that take `in` handles were regenerated with
`VESPER_PORT_BLESS=1`: Aura, Crucible, Ignis, serde and silicon (only the
`in` handle lines change: 108 insertions, 218 deletions). Quarry's binding
did not change. No port's default build or Rust code changed.

### Manuscript (flagged)

§16.6 (the export section; the prompt calls it §13) and one sentence of §7.3:

- §16.6 values: scalars pass by value, "`f64` as a double and `f32` as a
  single-precision float, both in the platform's float registers; inside an
  aggregate or a sequence buffer an `f32` is the low half of its word, and
  the high half is unspecified".
- §16.6 ABI v3: "An `in` parameter of a handle type is likewise a pointer to
  the caller's own handle words."
- §16.6, new paragraph: handle fields may hold a `ChunkSeq` of elements that
  own memory and boxed objects `Box[dyn Trait]`; the handle stays opaque,
  dropping it drops every element and each boxed value through its vtable;
  such a sequence never crosses on its own.
- §7.3: "the export ABI carries [boxed objects] only inside a handle type
  (§16.6)" replaces "the export ABI refuses boxed objects".

## Tests

- `tests/native_export_f32.rs`: the evaluator and the JIT (optimize on and
  off) agree with IEEE single results for `scale`, `halve`, `total`, `ramp`
  and the `Pt` mirror; the binding declarations; v1 and v2 libraries.
- `tests/native_export_lent_handles.rs`: `in_place` marking, the v3 binding
  text, the silicon repro without the directive (v2 keeps its copy) and
  with it, identical objects, and an evaluator/JIT run of 1,000 lent reads.
- `tests/native_export_handle_seqs.rs`: the interface (`Seq { Named }`,
  `Box { Object }`), the binding views, both report repros accepted, the
  refusals (as a parameter, result, `Option` result, slice), and
  evaluator/JIT runs of fabrics and graphs, with the JIT's box allocations
  and frees equal.
- `crates/vesper-port-pilot/tests/widen.rs`, linked against the object:
  every form end to end, including `NaN`, `-0.0`, overflow to infinity and
  subnormals; and the library's `live_allocations()` back to its starting
  value after dropping banks, fabrics (with their banks and names) and
  graphs (with their boxed `Gain`/`Delay` objects and the `Delay` lines),
  repeated 100-200 times.
- Every target builds the widen object (x86_64 and aarch64 Linux, aarch64
  macOS, x86_64 Windows MSVC); the pilot test runs on all four CI platforms.

Run locally: `native_export_{f32,lent_handles,handle_seqs,v2,v3,cli}`,
`vesper-port-harness`, `vesper-port-pilot` (default and `vesper-port`);
`cargo +1.100.0 clippy`. The full suite, the Bazel targets and the port
shards ran in CI.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The
bootstrap-stage1 workflow result for the branch is in the PR.

## Measurements

Release builds, Intel Xeon @ 2.10 GHz cloud host (4 vCPUs), one session.
Wall clock on this host moves by up to 30% between runs, so instruction
counts are the firmer result.

### silicon: `in` handles lent

The silicon object is the same in both arms (one build of
`crates/vesper-silicon/vesper`); only the binding differs. "Before" is the
binding of `origin/main`, linked with `VESPER_PORT_OBJECT`; "after" is the
regenerated binding. `examples/abba.rs compare 11 20`, arms run before,
after, after, before. Instructions are callgrind `Ir` of `abba one port
<workload> 20` minus `... 0`, per run.

| Workload | Port Ir/run before | after | change | Wall ratio to Rust before (2 runs) | after (2 runs) |
|---|--:|--:|--:|--:|--:|
| `aia-intc` | 3,976,507 | 3,959,132 | -0.44% | 1.21x, 1.21x | 1.28x, 1.27x |
| `aplic-direct` | 1,255,639 | 1,252,567 | -0.24% | 2.06x, 2.08x | 2.15x, 2.18x |
| `imsic` | 100,383 | 99,360 | -1.02% | 4.41x, 4.45x | 4.53x, 4.54x |

The copies are gone (46 `let mut arg_* = *.words;` lines in silicon's
binding before, 0 after), but they were small: `Aplic` is 12 words on main
(the 27-word, 23% `memcpy` figure in the silicon report predates the
fabric port, whose callers now live in the library). The instruction
counts drop by 0.2-1.0%. The wall ratios are 3-5% higher in both "after"
runs, inside this host's noise, and the instruction counts do not explain
it; it is reported as measured.

`ChunkSeq[Imsic]` instead of banks: **not measured**. The export side now
accepts it, but Vesper code cannot use a sequence of non-copy structs in
place: `f.files[i].read(x)` and `bank_read(in f.files[i], x)` are E1522
("cannot copy 'Bank' out of a sequence because it is not copy; use pop"),
and `inout f.files[i]` is E0300. Only boxed objects (`Box[dyn P]`) may be
lent from a sequence element. A rewrite would have to `pop` and `push`
elements, which is not the bank layout's cost model. This is a new gap
(below).

### f32

Before: the repro's binding did not compile. After: it runs (pilot tests).
No per-call measurement: the wrapper is the same move in both forms
(`f32::to_bits` and a GPR before, an `xmm` register now). A `&[f32]` is
copied at the boundary, because an `f32` takes eight bytes in a Vesper
sequence buffer (`seq::elem_bytes`). Ports that lend `&[f32]` as
`&[u32]` with `bytemuck` (Resonance) lend in place today and should keep
doing so for slices until `f32` packs at 4 bytes; scalars and mirror fields
can move to `f32` now.

### Resonance, Kinetic, Tensor, eBPF

Not re-measured. Resonance (#1082) and Kinetic (#1078) are on open port-lane
PRs, so their committed ABBA drivers are not on main, and the plan forbids
editing their crates while those PRs are open. A Resonance rewrite would
keep a `Graph`-like handle (`procs: ChunkSeq[Box[dyn DspProcessor]]` plus
the slab) across `render_graph` calls instead of compiling the processors
per call; the pilot's `Graph` shows the shape works and frees everything.
Tensor and eBPF are v2 libraries (their committed bindings say
`export-abi/2`), so `in` handle lending reaches them only if they opt into
`// @export abi 3`; their objects and bindings are unchanged by this lane.
Tensor's `f32` data crosses as `u32` bits in slices, which stays the
cheaper form until `f32` packs at 4 bytes (below).

## What is left

- **Element borrows of non-copy sequence elements** (new): Vesper code can
  lend only boxed-object elements from a sequence; `in`/`inout` borrows of
  a struct or handle element (`f(in s[i])`, `s[i].method()`) are E1522 /
  E0300. Without it, a `ChunkSeq[Handle]` field can be built, popped and
  dropped, but not used in place, so silicon keeps its banks.
- **`f32` sequence stride**: `ChunkSeq[f32]`/`Slice[f32]` store an `f32`
  per word, so `&[f32]` is copied at the boundary, and lane 1's slice loads
  assume a 4-byte `f32` stride. Packing `f32` at 4 bytes like `u32` is a
  compiler change (`seq::elem_bytes` and the sequence lowering), not a
  binding one.
- Element access from Rust to sequences of handles and boxed objects stays
  out of scope (`OpaqueSeq` gives the length only).
- Found while writing the pilot: in an exported function, `let gone =
  f.names.pop();` on a `ChunkSeq[String]` leaks the popped `String` (one
  live allocation per call). The pilot avoids it.
- Found while writing the pilot: an optimized function with a loop that
  calls through `s[i]` of a `ChunkSeq[Box[dyn P]]` reached through an
  `inout` struct is refused natively ("Box values in a function with loop
  ... not reclaimed"), while `--no-opt` builds it. The pilot recurses
  instead. #1076 (loop-owned boxes) merged while this PR was open; the
  pilot was not rewritten back to a loop.

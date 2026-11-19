# Port P3b lane 3: opaque Bridge handles for `.vsys` imports (F9)

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1065 (the import-handle half of wave-nine lane 34, #611, which had no
PR when this lane started). Base: `main` at `8212ddf`. Lane prompt:
`docs/agent-prompts/port-p3b/03-bridge-handles.md` (plan PR #1072).

## Summary

A `.vsys` library can now declare opaque foreign objects, and Vesper holds
them as linear or affine values that only the library's imports create, use
and finalize:

```text
sys library Aead (target = "@TARGET@", abi = "@ABI@", link = "static",
                  name = "vesper_aead", artifact = "blake3:...") {
    linear extern type Sealer;        // must reach exactly one finalizer
    affine extern type Probe;         // may be dropped
    fn aead_sealer_new(key: u64) -> ForeignVal[Option[Sealer]];
    fn aead_seal(inout s: Sealer, nocapture inout packet: CSlice[u8]) -> ForeignVal[u32];
    fn aead_open(in s: Sealer, nocapture inout packet: CSlice[u8]) -> ForeignVal[u32];
    fn aead_sealer_free(sink s: Sealer) -> ForeignVal[Unit];
}
```

```text
pub fn roundtrip(inout p: Slice[u8]) -> u32 eff [Ffi[Aead]] {
    let mut s = match Aead::aead_sealer_new(7) { Some(s) => s, None => return 1 };
    let code = Aead::aead_seal(inout s, inout p);   // in place, no copy
    Aead::aead_sealer_free(s);                      // forgetting this is E0302
    code
}
```

It works in the native backend (objects and the JIT) and in the evaluator,
with a counting test that every created sealer is finalized exactly once on
every path, including a handled `Fail` exit.

## The manuscript change (§16.3)

Two bullets after "Loom Supply-Chain Confinement" in `content.typ` specify
what §16.3's `Lz4DecompressCtx` example left open:

- **Opaque extern-type handles.** One pointer-sized word. Results are
  `ForeignVal[Option[H]]` (`NULL` is `None`) or `ForeignVal[H]` (`NULL` is
  `Err(ForeignInvalid { raw: 0 })`). Parameters are `in h: H`, `inout h: H`
  or `sink h: H`; the C callee receives the handle word in every mode, and the
  mode is the source contract. A `sink` import is a finalizer; a library with a
  `linear` type must declare one. In source `H` has no constructor, fields or
  equality, and an exported function cannot take or return it. A linear
  handle reaches exactly one `sink` on every path (`E0302`, `E0309`, `E0303`,
  `E0402` for a live handle across an unhandled abortable effect); a foreign
  call returns exactly once, so a handle may stay live across one. An affine
  handle may be dropped, which calls nothing.
- **Slices lent in place.** A `nocapture in`/`inout` `CSlice[T]` accepts a
  source `Slice[T]` when `T`'s packed stride is its C size (`u8`, `u32`,
  `u64`, `i64`, `usize`); the callee gets `(data + start * stride, len)`.

**Flagged decision:** the prompt asked that dropping an affine handle call a
declared drop import. That needs drop glue for the generated type in the
native backend's drop plan (`src/fip.rs`, `src/seq/native.rs`) and in the
evaluator, which other lanes own. The manuscript now says an affine drop calls
nothing, which matches every affine example in the manuscript (`GtkWidget`,
`VkDevice`, `VkQueue`: views the library keeps alive). An object that must be
released is `linear`. A drop import for affine types is a follow-up.

## The ABI

| `.vsys` | source | C |
| --- | --- | --- |
| `linear extern type H;` | `linear struct H { $handle: u64 }` (field unspellable) | - |
| `affine extern type H;` | `affine struct H { $handle: u64 }` | - |
| `in h: H` / `inout h: H` / `sink h: H` | `in` / `inout` / `sink` `H` | the handle word (`AbiType::Pointer`) |
| `ForeignVal[Option[H]]` | `Option[H]` | pointer; `NULL` is `None` |
| `ForeignVal[H]` | `Result[H, ForeignInvalid]` | pointer; `NULL` is `Err(raw: 0)` |
| `nocapture in/inout s: CSlice[u8]` with a `Slice[u8]` argument | `in`/`inout` `Slice[u8]` | `(data + start, len)`, no copy |

- `crates/vesper-ffi`: `ExternTypeDescriptor` (`Linearity::{Linear, Affine}`),
  `ParameterType::Handle { handle, access: HandleAccess::{In, Inout, Sink} }`,
  `ValueType::Handle { handle, nullable }`, `LibraryDescriptor::extern_types()`.
  Canonical bytes gain an `extern-types` section only when one is declared, so
  every existing library keeps its identity. The Rust-side dynamic API
  (`prepare_call`, `validate`, `check_call`) takes a handle as
  `RawArgument::Bits` and validates results to `ValidatedValue::Handle`.
  Rejected: a linear type without a `sink` import, a handle parameter without a
  mode, `out` and `nocapture` handles, `sink` of a non-handle, `Option` of a
  non-handle result, and name clashes with enums and wire structs.
- Foreign effects (`Ffi$Lib`) are now declared `linear` (one-shot): a foreign
  call returns exactly once and no source handler can abort it. That is what
  lets a linear handle stay live across the calls that use it, and it lets the
  evaluator hand foreign calls to a host.
- The export ABI refuses a handle in an exported signature with a typed
  `ExportError::Type` ("is an opaque .vsys extern-type handle"). Handles stay
  inside the Vesper library; the Rust wrapper of a port never sees one.
- The verified backend and WebAssembly refuse a World with foreign calls (a
  typed `VerifiedError` / `WasmError`; tested).

## Backends

- **Objects** (`ForeignChecked::object`, `with_lowering`, `vesper build
  --lib`): `src/ffi_lowering/native.rs` passes the handle struct's word, builds
  `Option`/`Result` results from the returned pointer, and lends a source slice
  as its own data pointer plus `start * stride`.
- **JIT** (new): `ForeignChecked::jit(entry, &[(symbol, fn pointer)])` binds
  each foreign symbol to a host function (one hunk in
  `native::register_jit_runtime_symbols`). A called symbol without a binding
  is `UnknownForeignFunction` before any code runs; `isolated(mpk)` stays
  object-only (`IsolationUnavailable`).
- **Evaluator** (new): `ForeignChecked::evaluate(entry, host)` runs `vesper
  run`'s machine and hands each foreign call to a host closure, which reads the
  arguments, writes `inout` slices (`slice_bytes`, `write_slice_bytes`) and
  builds handles (`handle_value`, `handle_word`).

## Tests (`tests/ffi_handles.rs`, 10 tests + 1 ignored timing)

The worked example `tests/fixtures/ffi_handles/aead.ves` drives a toy stateful
AEAD (a 256-byte key schedule, a keystream XOR and a 16-byte tag) through a
linear `Sealer` and an affine `Probe`: a round trip, a wrong-key open whose
`Fail[AuthError]` is handled while the sealers stay live, early returns,
`NULL` results as `None` and as `Err`, an empty never-grown packet (lent as a non-null pointer), 1,000 create/seal/free iterations, and
the measured loops.

- `jit_drives_a_stateful_rust_aead_through_linear_handles`: the Rust shim in
  the test binary, on every host with a `.vsys` target (Linux x86-64/AArch64,
  macOS AArch64, Windows x86-64); creations equal finalizations per entry.
- `evaluator_runs_handles_with_a_host_stand_in`: the same entries in the
  evaluator; the host checks no use after finalize and no double finalize.
- `objects_link_handles_with_a_c_library` (Linux x86-64): each entry's object
  linked with `aead.c` and a C host; counters printed by the host.
- `handle_misuse_is_a_typed_diagnostic`: one fixture per misuse in
  `tests/fixtures/ffi_handles/misuse/`: leak `E0302`, leak on a branch
  `E0309`, double finalize, use after finalize and copy `E0303`, comparing a
  linear handle `E0302`, comparing an affine one (bare, in an `Option`, a tuple or a struct) `E0200`, a live handle across
  an unhandled `Fail` `E0402`; constructing (struct literal or call) or
  destructuring is `FfiErrorKind::HandleMisuse`.
- `vsys_handle_declarations_are_validated`, `handle_types_belong_to_one_library` (two libraries may not share a handle name), `source_slices_only_for_packed_elements`,
  `export_libraries_keep_handles_inside`, `jit_refuses_unbound_foreign_symbols`,
  `verified_backend_and_wasm_refuse_foreign_handles`.

Existing suites run: `tests/ffi_lowering.rs` (6, including the PKU fault test)
and all `vesper-ffi`/`vesper-migrate` tests pass.

## Measurement

`measure_seal_through_handle` (ignored; `cargo test --release --test
ffi_handles -- --ignored --nocapture`), 20,000 seals of a 1,216-byte packet
(1,200 payload bytes and the tag), best of 7, JIT compile time subtracted by a
one-seal run of the same code. Cloud VM, so noisy; three runs:

| ns per seal | run 1 | run 2 | run 3 |
| --- | --- | --- | --- |
| Vesper JIT, through one handle | 3,490 | 4,225 | 3,978 |
| Vesper JIT, stateless (re-derives the schedule) | 6,411 | 8,211 | 6,014 |
| Rust, one schedule | 3,803 | 4,376 | 4,898 |
| Rust, stateless | 6,665 | 7,115 | 6,144 |

Holding the handle saves the key expansion on every packet (about 1.5-1.9x
here), and the Vesper loop around the foreign call is within noise of the
same loop in Rust: the call passes one word and the packet's own pointer, with
no marshalling. With a real AEAD the saving is the cost of the AES or ChaCha
key schedule plus any per-key precomputation (GHASH tables).

## What the Warp port needs from it

- Declare each piece of Rust state the port holds as a `linear extern type`
  in the port's `// @export vsys direct rust=<shim>` library: the
  `vesper-warp-crypto` sealer and opener, the `vesper-warp-tls` session, and a
  UMEM frame (or a frame pool, with frames as indices).
- The shim returns `Box::into_raw(..)` (or null) from the constructor and
  frees in the `sink` import. Seal and open take `inout s: Sealer` / `in o:
  Opener` and `nocapture inout packet: CSlice[u8]`, called with the port's
  `inout pkt: Slice[u8]`.
- Keep handles inside the Vesper library (exports refuse them). Code that owns
  a handle must not let an unhandled `Fail` cross it (`E0402`): handle the
  failure locally, finalize, then re-raise, or take the handle `inout` in the
  fallible helper.
- Not available yet: affine drop imports, handles inside `ChunkSeq` or boxes
  that must be finalized element by element (a `ChunkSeq[Sealer]` is linear
  and must be emptied, `ChunkSeq::finish`), handles in an isolated(mpk)
  library under the JIT, and pointer results or narrow integer parameters
  (still #611).

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The
bootstrap-stage1 workflow run on the branch is linked from the PR.

## Paths

`crates/vesper-ffi/src/sys/{mod,parser,foreign,assume}.rs`,
`crates/vesper-ffi/SYS-SPECS.md`, `src/ffi_lowering.rs`,
`src/ffi_lowering/{types,native}.rs`, the extern-handle hunk of
`src/check.rs` (equality), one hunk each in `src/native.rs` (JIT symbols) and
`src/native_export.rs` (refusal), one skip line in
`tests/fixtures/self_host/driver.rs`, `tests/ffi_handles.rs`,
`tests/fixtures/ffi_handles/**`, §16.3 of `docs/manuscript/content.typ`, the
`.vsys` paragraph of `docs/maintenance/porting-a-crate.md`, this report.

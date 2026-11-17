# Port P1 lane 1: export ABI v2 (#898)

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../manuscript/README.md) is the sole specification; use the
> [section map](../section-map.md) for earlier numbering. Confirm live ownership before acting.

`vesper build --lib` now exports sequences, slices, strings, owned and linear handles,
boxes and recursive enums, generic instances and `.vsys` imports under the
schema `vesper.native.export-abi/2`. A library that uses none of them keeps
schema `/1`, and its interface and binding are byte-identical to before
(`tests/native_export_v2.rs` compares the pilot's committed v1 binding). The
user-facing description is "The ABI", "What is exportable today" and
"Patterns for Rust APIs" in `docs/maintenance/porting-a-crate.md`.

## What landed

- `src/native_export.rs` plans v2 forms. New submodules:
  `native_export/directives.rs` (`// @export instance`, `// @export vsys`),
  `native_export/glue.rs` (the runtime entry points and one drop function per
  handle type, which walks the fields and frees sequences and boxes), and
  `native_export/binding_v2.rs` (the Rust binding for handles, views and
  methods). The v1 generator moved unchanged to `native_export/binding.rs`.
- Generic instances compile as generated monomorphic wrappers
  (`pub fn alias(..) { f[T](..) }`), because bounded generics are
  monomorphized and dropped in the front end. Symbols are
  `vesper_<lib>_<alias>`, with the default alias `f__t1__t2`.
- `.vsys` imports check through the Bridge lowering
  (`ffi_lowering::check_module`). A narrow hook,
  `ForeignChecked::with_lowering`, runs the export code generation under
  the import plan. `build --lib` now always compiles through
  `native_export::compile_module`, so the CLI and the harness share one path.
- `crates/vesper-port-abi`: `Abi::STRIDE` and `Abi::NATIVE`, and the `seq`
  module (`with_slice`, `with_slice_mut`, `with_seq`, `with_seq_mut`,
  `seq_take`, `SeqRef`, `box_words`, `SeqRuntime`).
- Live-allocation counters: `vesper_seq_live` in `src/seq/aot_runtime.rs`
  (plus a local `vesper_seq_free`) and `vesper_box_live` in
  `src/fip/aot_runtime.rs`, each its own commit. The box runtime now declares
  `exit` with a word argument, as the other runtimes do, so one module can
  link both.
- Strings (follow-up after #920): `String` and `Str` cross as the
  three-word value `{len, cap, data}`. `in Str`/`in String` lend a `&str`
  in place (`vesper_port_abi::with_str`), `inout String` lends a
  `&mut String` (`with_string_mut`), a `String` result is copied and freed
  (`string_take`), and a `String` handle field is freed by the handle's drop
  function (the local `vesper_str_release` in `src/seq/aot_runtime.rs`) and
  read through the view (`str_view`). The kit's `greet`, `word_count`,
  `shout` and `Note` handle exercise them.
- The pilot's second library, `crates/vesper-port-pilot/vesper/kit.ves`,
  exercises every form and the five patterns. `pilot.ves` stays v1.

## Acceptance

- `tests/kit.rs` (13 tests) passes against the Rust build and the Vesper
  build. The divergent build changes one thing per form, and the test of
  each form fails: `sequence_results`, `slice_parameters`, `strings`, `owned_handles`,
  `linear_handles`, `recursive_enum_handles`, `generic_instances`,
  `vsys_imports`. The pattern tests keep passing.
- `tests/kit_memory.rs` runs 10^5 cycles of every owning form, and
  `live_allocations()` returns to its start. The divergent build replaces
  `c.hits.clear()` with `c.hits = ChunkSeq::new[u64]()`, which drops a
  sequence on the Vesper side without freeing it. The test then reports
  200000 leaked allocations (header and buffer per cycle) and fails.
- Bazel: `:port_kit_test`, `:port_kit_memory_test`, and
  `:port_divergence_detected` (now three divergent binaries). Cargo:
  `scripts/test-port-harness.sh` runs the same checks on macOS and Windows.
- Still-refused forms each have a refusal test that names the function
  (`still_refused_forms_name_the_function`).

## Known gaps

- Native code does not free a sequence or string that goes out of scope, so
  a sequence, a string, or a handle owning one taken by value (`sink`) is
  refused. Strings as sequence elements and `String` errors are refused too.
  Boxes are freed. Fixing the drop elaboration for sequences would lift this.
- `Ord` on `i32` fails in the trait solver (`BRIDGE0004: unknown type i32`),
  so the pilot instantiates `largest` at `u64` and `i64`.
- `x86_64-apple-darwin` has no `.vsys` ABI in `vesper-ffi`; a library with a
  `.vsys` import is refused there.

## Commands

```sh
cargo test -p vesper --test native_export_v2 --test native_export_cli
cargo test -p vesper-port-abi -p vesper-port-harness -p vesper-port-pilot
cargo test -p vesper-port-pilot --features vesper-port
scripts/test-port-harness.sh
cargo +1.100.0 clippy --all-targets -p vesper -p vesper-port-abi -p vesper-port-pilot -- -D warnings
```

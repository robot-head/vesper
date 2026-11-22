# @vesper/codegen-numeric

Native lowering of the numeric and memory families in Vesper: self-hosting
wave SH4, lane 11 (#1285). The Rust lowering stays the default compiler; this
package is compared with it by
`crates/vesper-codegen-numeric-test/tests/self_host_codegen_numeric.rs`.

| File | Ports | Compared by |
|---|---|---|
| `src/rt.ves` | the shared `sig`/`import`/`local`/`call` helpers of the runtime generators | (used by the three below) |
| `src/regions.ves` | `src/regions/aot_runtime.rs`: the region runtime of AOT executables | runtime symbols |
| `src/tiled.ves` | `src/tiled/aot_runtime.rs`: the `TiledSeq` runtime | runtime symbols |
| `src/boxes.ves` | `src/fip/aot_runtime.rs`: `vesper_box_alloc`, `vesper_box_free` | runtime symbols |
| `src/shapes.ves` | the lane-shape decisions of `src/vector.rs` and the chunk types of `src/native_vector.rs` | lane-shape decisions |
| `src/lib.ves` | `runtime_object`: a module holding the selected runtimes | |

**Runtime symbols.** The generated runtimes are whole functions defined into
a module, so they need none of lane 1's lent builders and emit through the
existing shim. For each of three programs (regions, `TiledSeq`, `Box`), two
ELF targets and optimize on and off, the test compiles the program with the
real compiler (`native::object_with_options`) and builds the same runtimes
with `runtime_object`; every runtime function and fault message must have
the same bytes and relocations in both objects.

**Lane-shape decisions.** `shapes.ves` needs no Cranelift handle. The test
compiles it alone and compares `vector_decisions` (elements, every shape up
to 64 lanes, profiles, conversions and every intrinsic path) with the same
text built from `vesper::vector`.

**Assembly.** A package cannot import a `.vsys` library yet (shim report,
gap 4), so the test checks `@vesper/cranelift`'s source and these files as
one unit, as the shim's own tests do. The files therefore have no `use`
lines, and `vesper check` on the package directory reports the shim's
names as unknown.

# Porting a Rust crate to Vesper

This is the recipe that every crate port follows. A port keeps the Rust crate,
adds a Vesper implementation of its public API beside it, and runs the crate's
existing tests against both builds until the Vesper build is the only one left.
The survey that orders the ports is `docs/maintenance/vesper-port-survey.md`
(#869). The tools come from port wave P0 lane 6 (#878).

The worked example is `crates/vesper-port-pilot`.

## The pieces

| Piece | Where | What it does |
|---|---|---|
| `vesper build --lib` | `src/native_export.rs` | Compiles a `.ves` file to one object. Every `pub fn` becomes the C symbol `vesper_<library>_<fn>`. |
| Interface file | `--interface out.json` | Describes every symbol and type at the boundary (schema `vesper.native.export-abi/1`). |
| Rust binding | `--rust-binding out.rs` | `extern "C"` declarations plus a safe wrapper per function. |
| `vesper-port-abi` | `crates/vesper-port-abi` | The `Abi` trait the bindings use, `ArithOverflow` and `RuntimeError`. It has no dependencies. |
| `vesper-port-harness` | `crates/vesper-port-harness` | The `build.rs` helper. It compiles the port with the in-tree compiler, archives and links it, and checks the committed binding. |
| `vesper_port_object` | `tools/bazel/vesper_port.bzl` | The same compile step in Bazel, using `//:bin/vesper`. |

## The ABI

Values cross the boundary as eight-byte words, using the layouts in
`src/layout.rs`:

- A scalar is one word.
- A tuple or struct is its fields in order.
- An enum is a tag word (the variant index) followed by the variant's fields.

Calls use the platform C calling convention:

- Scalars are passed as `i64`, and `f64` is passed as `f64`.
- Aggregates are passed as a pointer to their words. The callee may overwrite
  those words, so the caller passes a scratch copy.
- An aggregate result is written through a pointer that comes before every
  other parameter.
- A function with `Fail[E1], .., Fail[En]` in its row takes one more pointer
  after its parameters: the failure record. Word 0 is the status word. The
  caller sets it to 0. On return, 0 means success, and `k` means the function
  raised the `k`-th `Fail` of the row. The error value is in the words that
  follow.

The binding maps the status word to `Result<T, E>`. With several `Fail` types,
it maps to `Result<T, <Fn>Error>`, an enum with one variant per error type,
named after the type.

What is exportable today:

- Scalars `u8 u32 u64 i64 f64 bool Unit`.
- Tuples, and non-generic `copy` structs and enums built from exportable types.
- `Option[T]`, `Result[T, E]`, `ArithOverflow` and `RuntimeError`.
- Effect rows that hold only unlabeled `Fail[E]`.

Everything else is refused, with a message that names the function:

- generic functions
- `inout` parameters
- any other effect, including capability and foreign effects
- strings, closures, region handles, boxes, sequences
- affine and linear data

Sequences and strings get added to the ABI once P0 lanes 3 and 7 land.

## Steps

1. **Claim the port.** Check open issues and PRs for the crate, then open a
   claim issue and link it from the PR.

2. **Write the Vesper source** at `<crate>/vesper/<name>.ves`. Give every
   public Rust function a `pub fn` with the same name and an equivalent
   signature, and keep helpers private. Each public Rust type that crosses the
   boundary gets a Vesper `copy` type with the same name. Field and variant
   names must match too, because the binding implements `Abi` for the crate's
   own types. Run `vesper fmt` on the file.

3. **Add the feature and the build script.** In `Cargo.toml`:

   ```toml
   [features]
   vesper-port = ["dep:vesper-port-harness"]

   [dependencies]
   vesper-port-abi = { path = "../vesper-port-abi" }

   [build-dependencies]
   vesper-port-harness = { path = "../vesper-port-harness", optional = true }
   ```

   In `build.rs`:

   ```rust
   fn main() {
       #[cfg(feature = "vesper-port")]
       vesper_port_harness::Port::new("<name>", "vesper/<name>.ves")
           .binding("src/port_binding.rs")
           .rust_types("crate")
           .build();
   }
   ```

4. **Generate the binding.** Run
   `VESPER_PORT_BLESS=1 cargo test -p <crate> --features vesper-port` once and
   commit `src/port_binding.rs`. After that, every build checks it: a change to
   the Vesper source that changes a signature fails the build until you bless
   the binding again and review the diff.

5. **Switch the public API on the feature.** Move the Rust implementation into
   a module, and re-export one implementation or the other:

   ```rust
   #[cfg(not(feature = "vesper-port"))]
   mod reference;
   #[cfg(not(feature = "vesper-port"))]
   pub use reference::*;

   #[cfg(feature = "vesper-port")]
   #[rustfmt::skip]
   mod port_binding;
   #[cfg(feature = "vesper-port")]
   pub use port_binding::*;
   ```

   Types that cross the boundary stay in the crate root, so both builds share
   them. Use `vesper_port_abi::{ArithOverflow, RuntimeError}` for Vesper's
   builtin errors.

6. **Run the tests both ways.** Run `cargo test -p <crate>` and then
   `cargo test -p <crate> --features vesper-port`. The tests under `tests/` and
   the golden fixtures use only the public API, so they run unchanged against
   both builds. When a result differs, the Vesper port is wrong, or the Rust
   behavior was never specified. Fix the port, or write the behavior down as a
   test first.

7. **Add CI.**
   - Bazel: copy the `vesper_port_object`, `rust_library` and `rust_test`
     targets from `crates/vesper-port-pilot/BUILD.bazel`, so that Linux runs the
     port tests.
   - macOS and Windows: add the crate to `scripts/test-port-harness.sh`.

## The unit-test rule

In-crate `#[cfg(test)]` unit tests often reach private functions or private
state. They cannot run against the Vesper build, because the Vesper build
replaces the whole implementation. Before you delete a crate's Rust
implementation, handle every such test in one of three ways:

- **Translate** it into an integration test under `tests/` that uses only the
  public API. Then it runs both ways.
- **Port** it into a Vesper test next to the Vesper source, if it checks a
  private helper that has a Vesper counterpart.
- **Delete** it only if it tests a Rust-only detail that the port removes, and
  say so in the PR.

Until then, keep such tests Rust-only by putting them in the module that is
compiled only without `vesper-port`. The pilot's `in_range_is_inclusive` test
in `src/reference.rs` shows this.

## Proving the harness can fail

A differential harness that cannot fail proves nothing. The pilot builds a
deliberately divergent port from its real source:

- In Cargo, `Port::edit` makes the change under the `vesper-port-divergent`
  feature.
- In Bazel, the `edits` attribute of `vesper_port_object` makes the same
  change.

CI asserts that the API tests fail on exactly the changed function and pass on
the rest:

- Bazel: `//crates/vesper-port-pilot:port_divergence_detected`.
- macOS and Windows: `scripts/test-port-harness.sh`.

Larger ports should add one such divergence check per module they port.

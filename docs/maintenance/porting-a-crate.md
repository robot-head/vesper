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
- A `ChunkSeq[T]` and a `Box[T]` are one word, a pointer. The sequence header
  it points to is `{len, cap, data}`; `data` holds `len` elements packed at
  their stride (1 byte for `u8` and `bool`, 4 for `u32`, otherwise
  the element's words).
- A `Slice[T]` is the three-word view `{header, start, len}`.
- A `String` is the three-word value `{len, cap, data}` of UTF-8 bytes,
  which is also a valid `ChunkSeq[u8]` header. A `Str` view is
  `{len, 0, data + start}`: capacity 0 marks bytes the value borrows.

Calls use the platform C calling convention:

- Scalars are passed as `i64`, and `f64` is passed as `f64`.
- Aggregates are passed as a pointer to their words. The callee may overwrite
  those words, so the caller passes a scratch copy.
- An `inout` parameter is passed as a pointer to its words, which the callee
  updates in place.
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

A library that uses only scalars, copy data and `Fail` keeps schema
`vesper.native.export-abi/1`, byte for byte. Using any form below switches
it to `vesper.native.export-abi/2` (#898), which adds:

- **Runtime entry points.** `vesper_<lib>_seq_new(capacity, stride)`,
  `vesper_<lib>_seq_free(seq)` and `vesper_<lib>_live_allocations()`. The
  binding exposes them as `RUNTIME` and `live_allocations()`; the last one is
  for leak tests.
- **Sequences and slices.** A returned `ChunkSeq[T]` becomes `Vec<T>`.
  `in Slice[T]` becomes `&[T]`, `inout Slice[T]` becomes `&mut [T]`,
  `in ChunkSeq[T]` becomes `&[T]`, and `inout ChunkSeq[T]` becomes
  `&mut Vec<T>`. Slices of the scalars whose Rust layout is the packed one
  (`u8`, `u32`, `u64`, `i64`, `usize`, `isize`, `f64`) are lent without a
  copy; other elements are copied into a packed buffer (and back, for
  `inout`).
- **Strings.** `in Str` and `in String` become `&str`, lent in place
  without a copy. `inout String` becomes `&mut String`: Vesper copies the
  bytes before its first write, and the result is copied back. A returned
  `String` becomes `String` (copied, and the Vesper buffer freed). A
  `String` field of a handle is freed by its drop function and reads as
  `&str` through the view.
- **Handles.** A non-`copy` struct or enum becomes an owned Rust type that
  holds its words. Dropping it calls `vesper_<lib>_<Type>_drop`, which frees
  every sequence and box the value owns. Functions named
  `<type>_<method>` whose first parameter is the handle become methods:
  `in` is `&self`, `inout` is `&mut self`, and `sink` consumes `self`. A
  function returning the type becomes an associated constructor. A `linear`
  type gets `#[must_use]` and no `Drop`: only a consuming call releases it.
  `as_ref()` gives a borrowed view (`<Type>Ref`) that reads fields in place;
  a sequence field reads as `vesper_port_abi::SeqRef`, and a `String`
  field as `&str`.
- **Boxes and recursive enums.** A `Box[T]` field is read through the view,
  so `enum Tree { Leaf(u64), Node(Box[Tree], Box[Tree]) }` becomes `Tree`,
  and `tree.as_ref().view()` gives `TreeView::Node(TreeRef, TreeRef)`.
- **Generic instances.** `// @export instance [name =] f[T, ..]` in the
  source, or `--instance` on the command line, exports one instance as
  `vesper_<lib>_<name>`. The default name is `f__t1__t2`, so the symbol stays
  stable as long as the spelling does.
- **`.vsys` imports.** `// @export vsys direct rust=<path>` followed by
  `// | ` lines holding a `sys library` declaration imports C functions
  (`eff [Ffi[Lib]]`) that the Rust crate itself provides at `<path>`. Write
  `target = "@TARGET@", abi = "@ABI@"`; the compiler fills them in for the
  object's target. The binding references the shim symbols, so the linker
  keeps them.

Directives are line comments that start with `// @export`. Anything else after
`@export` is an error, so a typo cannot be ignored silently.

What is exportable today:

- Scalars: every integer kind (`u8 u16 u32 u64 usize i8 i16 i32 i64 isize`), `f64`, `bool` and `Unit`.
- Tuples, and non-generic `copy` structs and enums built from exportable types.
- `Option[T]`, `Result[T, E]`, `ArithOverflow` and `RuntimeError`.
- Effect rows that hold only unlabeled `Fail[E]` and imported `Ffi[Lib]`.
- `inout` scalars and aggregates.
- `ChunkSeq[T]` results and `in`/`inout` parameters, and `Slice[T]`
  parameters, of `copy` elements.
- `String` results, `in Str`, `in String` and `inout String` parameters,
  and `String` fields of handles.
- Handle types (non-`copy` structs and enums, affine or linear), with
  sequence, box, `Option` and nested handle fields.
- Instances of generic functions.

Everything else is refused, with a message that names the function
(`tests/native_export_v2.rs` lists every case):

- generic functions without an instance
- a sequence or string, or a handle that owns one, taken by value
  (`sink`): native code does not free a dropped sequence or string yet, so
  take it `in` or `inout`
- strings as sequence or slice elements, and `String` errors
- a `Box[T]` as a parameter or result of its own (wrap it in a handle type)
- any other effect, including capability effects and unlabeled rows
- closures and region handles
- generic user types
- `.vsys` imports for targets `vesper-ffi` has no ABI for (for example
  `x86_64-apple-darwin`)

## Patterns for Rust APIs

Many Rust APIs do not map onto the forms above one to one. These five
patterns cover the common cases. Each has a pilot example in
`crates/vesper-port-pilot` (`vesper/kit.ves`, `src/kit/patterns.rs`), and
`tests/kit.rs` runs it against both builds.

**(a) Owned handles.** A Rust type with private state and methods becomes a
Vesper struct and `<type>_<method>` functions. The binding turns them into a
Rust type with methods and `Drop`, so callers do not change. Example:
`Counter` (`counter_new`, `counter_record`, `counter_hits`, ...) and the
linear `Ticket`, which must be closed.

**(b) Token slabs.** When the Rust API stores arbitrary `T` (closures, trait
objects, borrowed data), keep the values in a Rust `Vec<Option<T>>` and give
Vesper only the `u64` slot numbers. Vesper owns the logic (order, priority,
expiry), and Rust owns the payloads. Example: `TokenSlab<T>` over `Tokens`.

**(c) Sink closures become returned sequences.** A function that calls
`f(item)` for every result returns a `ChunkSeq` of the results instead, and
a thin Rust wrapper calls the closure. Example: `for_each_word` over
`word_bounds`.

**(d) A trait becomes a passed value or a command plan.** If the Rust code
asks a trait object for a value (`clock.now()`), ask it in the wrapper and
pass the answer in. Example: `is_expired`. If it tells the trait object to
act (`transport.send(..)`), Vesper returns the actions as a sequence of a
`copy` enum, and the wrapper applies them in order. Example: `broadcast` over
`plan_sends`.

**(e) Errors go through `Fail[E]`.** Declare the crate's error enum as a
`copy enum` in Vesper and raise it with `Fail::raise(e)`. With
`--rust-types`, the binding returns the crate's own Rust enum, so `?` and
`Display` keep working. Example: `mean` and `mean_or_zero`.

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
           .binding("port/binding.rs")
           .rust_types("crate")
           .build();
   }
   ```

4. **Generate the binding.** Run
   `VESPER_PORT_BLESS=1 cargo test -p <crate> --features vesper-port` once and
   commit `port/binding.rs`. Keep it outside `src/`: Bazel's rustfmt check
   formats every file under `src/` and ignores `#[rustfmt::skip]`, and the
   generated text is not rustfmt-formatted. After that, every build checks it: a change to
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
   #[path = "../port/binding.rs"]
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
   - Bazel: copy the `vesper_port_object`, `rust_library` (with
     `port/binding.rs` in `srcs`) and `rust_test` targets from `crates/vesper-port-pilot/BUILD.bazel`, so that Linux runs the
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

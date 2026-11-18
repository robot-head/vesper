# Porting a Rust crate to Vesper

This is the recipe that every crate port follows. A port keeps the Rust crate,
adds a Vesper implementation of its public API beside it, and runs the crate's
existing tests against both builds until the Vesper build is the only one left.
The survey that orders the ports is `docs/maintenance/vesper-port-survey.md`
(#869). The tools come from port wave P0 lane 6 (#878); the build modes,
package inputs and the seed-copy rule come from port wave P1 lane 2 (#899).

The worked example is `crates/vesper-port-pilot`. A crate the compiler itself
links follows "Crates the compiler links" below, shown on `crates/vesper-beacon`.

## The pieces

| Piece | Where | What it does |
|---|---|---|
| `vesper build --lib` | `src/native_export.rs` | Compiles a `.ves` file to one object. Every `pub fn` becomes the C symbol `vesper_<library>_<fn>`. |
| Interface file | `--interface out.json` | Describes every symbol and type at the boundary (schema `vesper.native.export-abi/1`). |
| Rust binding | `--rust-binding out.rs` | `extern "C"` declarations plus a safe wrapper per function. |
| `vesper-port-abi` | `crates/vesper-port-abi` | The `Abi` trait the bindings use, `ArithOverflow` and `RuntimeError`. It has no dependencies. |
| `vesper-port-harness` | `crates/vesper-port-harness` | The `build.rs` helper. It runs a `vesper` executable on the port (it does not link the compiler library), archives and links the object, sets `cfg(vesper_port)`, and checks the committed binding. |
| `vesper_package`, `vesper_port_object` | `tools/bazel/vesper_port.bzl` | The same compile step in Bazel, using `//:bin/vesper`, on a Loom package with its path dependencies or on one file. |
| `vesper-port-build` | `crates/vesper-port-build` | Fixture: a multi-file package with a path dependency, built both ways. |

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

### Export ABI v3: state lent in place

The v2 binding converts every `copy` aggregate to words and back around each
call. For a port whose state is a large `copy` struct taken `inout` (BBR's
`Bbrv3` is 70 words), that conversion is most of the call. Writing
`// @export abi 3` in the source makes the library
`vesper.native.export-abi/3` (#934). The object is the same as in v2; the
binding changes:

- **Mirrors.** Every `copy` struct gets `repr::<Type>`, a `#[repr(C)]`
  struct in the Vesper word layout. A full-word scalar field (`i64`, `u64`,
  `isize`, `usize`, `f64`) is the plain Rust type. A narrower scalar, a
  `bool`, `Unit` or a field-less enum is a `vesper_port_abi::Word<T>`: the
  canonical word, read with `.get()` and written with `.set(v)`. A nested
  `copy` struct is its own mirror. Any other copy value (an `Option`, a
  `Result`, a tuple, an enum with payloads) is a
  `vesper_port_abi::Words<T, N>`, converted on `.get()`/`.set()`. The
  binding asserts the mirror's size, alignment and every field offset at
  compile time, implements `vesper_port_abi::Mirror` for it, and adds
  `from_value(&T)` and `to_value()` to convert from and to the Rust value
  type.
- **In place.** An `in` or `inout` parameter of a mirrored struct is
  `&repr::T` or `&mut repr::T`, passed as a pointer to the caller's storage
  with no conversion and no scratch copy. `in Slice[T]` and
  `inout Slice[T]` of a mirrored struct are `&[repr::T]` and
  `&mut [repr::T]`, lent without a heap copy. A mirrored struct result is
  returned as `repr::T`. A `sink` parameter takes `repr::T` by value, and
  the binding passes a copy, since Vesper may reuse a consumed value's
  storage. Vesper never writes an `in` aggregate (`E0307`).
- **Converted.** Enums and tuples have no fixed Rust layout (tag and payload
  validity, Rust's tuple layout), so they keep the v2 conversion. The
  interface says so for each type (`"mirror": {"kind": "converted",
  "reason": ..}`), and marks every in-place parameter `"in_place": true`.
- **Aliasing.** An `inout` mirror and a borrowed slice of the same storage
  cannot reach one call: the wrapper takes `&mut T` and `&[T]`, and the
  borrow checker refuses the alias.
- **`Option` and `Result` (`// @export abi 3 options`, #991).** With the
  `options` word on the directive, an `Option[T]` or `Result[T, E]` of copy
  payloads crosses as `vesper_port_abi::OptionMirror<C>` or
  `ResultMirror<C, D>`: the tag word (`None`/`Ok` is 0, `Some`/`Err` is 1)
  and the payload cell, which is the payload's mirror, a full word, a
  `Word` or a `Words` (the same cells as struct fields). `in` and `inout`
  parameters are `&OptionMirror<..>`/`&mut OptionMirror<..>`, lent in place,
  and results are returned as the mirror, with no conversion. Build them
  with `OptionMirror::some(v)`, `::none()` or `From<Option<_>>`, and read
  them with `.as_ref()` or `.get()`. The option changes only the binding
  (the object is the same), and it is opt-in so that committed v3 bindings
  stay byte for byte. Struct fields of `Option`/`Result` type stay `Words`.
- **Sequences held by a handle.** A `SeqRef<u8>` (or of another packed
  scalar: `u32`, `u64`, `i64`, `usize`, `isize`, `f64`) is viewed in place
  with `.as_slice()`, and `.to_vec()` copies it with one `memcpy` instead
  of one read per element.

To use v3, keep the mirror as the port's storage in the Vesper build, and
the Rust value in the Rust build, behind the same API. The pilot's `gauge`
library (`src/gauge.rs`, `vesper/gauge.ves`, `tests/gauge.rs`) is the
pattern: `Meter` holds `repr::Gauge` or `Gauge`, `Samples` holds
`Vec<repr::Sample>` or `Vec<Sample>`, and the tests run against both builds.

Directives are line comments that start with `// @export`: `abi 3`
(optionally `abi 3 options`), `instance` and `vsys`. Anything else after
`@export` is an error, so a typo cannot be ignored silently. Write them in
the package's root module (`src/lib.ves`). `abi` in another file of the
package still applies, with a warning (ports placed it in the file that
sorted first while directives were read from file 0); `instance` and `vsys`
anywhere but the root module are an error (#991).

What is exportable today:

- Scalars: every integer kind (`u8 u16 u32 u64 usize i8 i16 i32 i64 isize`), `f64`, `bool` and `Unit`.
- Tuples, and non-generic `copy` structs and enums built from exportable types.
- `Option[T]`, `Result[T, E]`, `ArithOverflow` and `RuntimeError`.
- Effect rows that hold only unlabeled `Fail[E]` and imported `Ffi[Lib]`.
- `inout` scalars and aggregates.
- `ChunkSeq[T]` results and `in`/`inout`/by-value parameters, and
  `Slice[T]` parameters, of `copy` elements. A by-value sequence belongs to
  the callee, which frees it (the binding takes a `Vec<T>`).
- `String` results, `in Str`, `in String`, `inout String` and by-value
  `String` parameters (the binding takes a `String` and copies it into a
  buffer of the library, which the callee owns and may keep in a returned
  handle), and `String` fields of handles.
- Handle types (non-`copy` structs and enums, affine or linear), with
  sequence, box, `Option` and nested handle fields.
- Instances of generic functions.

Everything else is refused, with a message that names the function
(`tests/native_export_v2.rs` lists every case):

- generic functions without an instance
- a handle that owns a sequence or string, taken by value (`sink`): native
  code does not free the sequences inside a dropped handle yet, so take it
  `in` or `inout`
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

2. **Write the Vesper source** as a Loom package in `<crate>/vesper/`
   (`Loom.toml` named `@vesper/<short-name>`, root module `src/lib.ves`, more
   modules under `src/`, other packages as path dependencies). A single
   `<crate>/vesper/<name>.ves` file also works, as in the pilot. Give every
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
       vesper_port_harness::Port::new("<name>", "vesper")
           .binding("port/binding.rs")
           .rust_types("crate")
           .build();
   }
   ```

   The second argument is the package directory (or the `.ves` file). The
   harness emits `cargo:rerun-if-changed` for every package directory of the
   closure, so editing any module or dependency rebuilds the port. It also
   sets `cfg(vesper_port)` on the crate.

   **Where the compiler comes from.** The harness never links the `vesper`
   library. It takes the object from the first of:

   1. `VESPER_PORT_OBJECT` (and `VESPER_PORT_GENERATED_BINDING`): a prebuilt
      object. Bazel passes these through `build_script_env`.
   2. `VESPER_PORT_COMPILER`: a `vesper` executable, run as
      `vesper build --lib <package dir> --target $TARGET ... --rust-binding`.
      CI builds `cargo build --locked --bin vesper` once and sets this.
   3. Nothing set: the harness builds the in-tree compiler itself with
      `cargo build -p vesper --bin vesper --target <host> --target-dir <target>/vesper-port-compiler`
      (about two minutes the first time, a no-op afterwards), so a plain
      `cargo test -p <crate>` works. Set `VESPER_PORT_COMPILER` to skip it.

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
   - Bazel (Linux): declare the package with `vesper_package` (its manifest,
     `glob(["vesper/src/**/*.ves"])`, and a `vesper_package` per path
     dependency in `deps`), build it with `vesper_port_object(package = ...)`,
     and then either
     - hand the object to the crate's build script, as
       `crates/vesper-beacon/BUILD.bazel` and `crates/vesper-port-build/BUILD.bazel`
       do (`build_script_data` plus `VESPER_PORT_OBJECT` and
       `VESPER_PORT_GENERATED_BINDING` in `build_script_env`, both
       `"$(execpath ...)"`, which rules_rust makes absolute), so every
       generated target links the port;
       or
     - for an optional `vesper-port` feature, copy the `rust_library` (with
       `port/binding.rs` in `srcs`) and `rust_test` targets from
       `crates/vesper-port-pilot/BUILD.bazel`.
     Run `python3 tools/bazel/sync_build_files.py` for a new crate.
   - Cargo: append the crate's lines to the end of
     `scripts/test-port-harness.sh`. The Linux job of the Platforms workflow
     runs them, with the prebuilt compiler in `VESPER_PORT_COMPILER`. macOS
     and Windows stop after the export ABI tier at the top of the script,
     so a port's lines run there only if they test the export ABI itself.
   - Add the port's `vesper/` directory to the skip predicate in `fixtures()`
     in `tests/fixtures/self_host/driver.rs` while the self-hosted parser
     cannot parse `use` (#880).

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
  change. For a package, `edit_src` names the root-package file to edit; the
  rule copies the package closure with its layout and edits the copy. In
  Cargo, `Port::edit` on a package edits a copy of the closure in `OUT_DIR`,
  and the text must occur exactly once across the root package's `.ves`
  files. `crates/vesper-port-build` (feature `divergent`) shows both.

CI asserts that the API tests fail on exactly the changed function and pass on
the rest:

- Bazel: `//crates/vesper-port-pilot:port_divergence_detected`.
- Cargo: `scripts/test-port-harness.sh` (the Linux Platforms job for
  ports; macOS and Windows run its export ABI tier, which includes the
  pilot's checks).

Larger ports should add one such divergence check per module they port.

## Crates the compiler links

Ten port-first crates are in the compiler's own dependency closure
(`cargo tree -p vesper -e normal,build`): `vesper-alloc`, `vesper-beacon`,
`vesper-crucible`, `vesper-grammar`, `vesper-loom-calculus`,
`vesper-loom-solver`, `vesper-semver`, `vesper-serde`, `vesper-simpulse` and
`vesper-tensor`. Building their port needs a `vesper` compiler, so the
compiler cannot itself link the port, or it would need itself to build. Two
mechanisms keep that apart.

**The harness does not link the compiler.** With the P0 harness, which linked
the `vesper` library, a port in any of these crates failed with
`error: cyclic package dependency` (`vesper -> vesper-beacon -> [build]
vesper-port-harness -> vesper`). The harness now runs a `vesper` executable
(see step 3), so there is no package cycle.

**The seed-copy rule.** A Cargo feature cannot keep the compiler on the Rust
build: resolver 2 unifies the features of a package across the whole build,
so if any crate enabled `vesper-port` on a shared crate, the compiler would
get the port too. Use two package identities instead:

- The **seed package** `crates/<crate>/seed/`, package `<crate>-seed`, with
  the same library name, builds the frozen Rust code: `[lib] path =
  "../src/lib.rs"`, no build script, `[[test]]` entries pointing at
  `../tests/*.rs`, and `check-cfg = ["cfg(vesper_port)"]`. Its Rust code takes
  bug fixes only until self-hosting Stage 2 moves the compiler's own use of
  the crate. Its `BUILD.bazel` is written by hand (sources live in the parent
  package), as in `crates/vesper-beacon/seed/BUILD.bazel`.
- The **original package** `<crate>` is the port-backed build: its `build.rs`
  always calls `Port::build`, which sets `cfg(vesper_port)`, and its public
  functions forward to the binding under `#[cfg(vesper_port)]`, keeping the
  Rust body under `#[cfg(not(vesper_port))]` for the seed. The crate's own
  tests run against both packages (`cargo test -p <crate> -p <crate>-seed`).
- Every crate **inside** `cargo tree -p vesper -e normal,build` depends on
  the seed by rename, so its code does not change:

  ```toml
  vesper-beacon = { path = "crates/vesper-beacon/seed", package = "vesper-beacon-seed" }
  ```

  Every crate **outside** the closure keeps depending on `<crate>` and so
  links the port. Make this switch in its own commit.
- Both packages document the same library, so `cargo doc` cannot take both in
  one invocation (`document output filename collision`); document them one
  at a time.

`crates/vesper-beacon` shows the rule: `vesper` links `vesper-beacon-seed`
(`IMPLEMENTATION == "rust"`), and `vesper-beacon-arrow`, outside the closure,
links `vesper-beacon`. While a port is slower than its Rust build, the
user's parity rule keeps Rust the default there too: `vesper-beacon` builds
the Rust code unless its `vesper-port` feature is on, and its build script
still depends on `vesper-port-harness` unconditionally, so the guard below
counts it as port-backed either way (`IMPLEMENTATION` follows the feature,
`crates/vesper-beacon-arrow/tests/beacon_build.rs`; full port: lane 4, #901).

**The guard.** Two checks fail CI if a port-backed package (one whose build
script runs `vesper-port-harness`) enters the normal or build dependency
closure of `vesper` or of `vesper-codegen-verified` (which links no
Vesper-built code at all):

- Cargo: `crates/vesper-port-harness/tests/closure_guard.rs` reads
  `cargo metadata`. Its `guard_fails_when_the_compiler_links_a_port` test
  points the compiler's Beacon dependency at the port in the metadata and
  checks that the guard reports it.
- Bazel: `//crates/vesper-port-harness:compiler_closure_has_no_ports` reads
  `genquery` results for `kind('vesper_port_object', deps(...))` of `//:vesper`
  and `//crates/vesper-codegen-verified`, with `//crates/vesper-beacon-arrow`
  as the positive control.

A port that slips into the compiler's closure anyway also fails at build
time: the harness's nested compiler build sets
`VESPER_PORT_IN_COMPILER_BUILD`, and a port build script that sees it stops
with a message pointing here instead of recursing.

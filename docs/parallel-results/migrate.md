# Wave five lane 11: vesper migrate (C/Rust signature lifting and differential shadowing)

Design coverage: §29.3 (incremental Rust/C migration, `vesper migrate`, MPK
differential shadowing) and the lifter half of §29.5
(`LegacyRustParamSummary`, `lift_rust_param_to_vesper_projection`). The §29.5
ChoiceTape shrinker belongs to lane 5 (Crucible). Claim issue #131.

- Base: `af03122ea74d403f203170926945f9d35786b289` (origin/main).
- Branch: `claude/project-thread-kruy16`.
- New crate: `crates/vesper-migrate` (`#![forbid(unsafe_code)]`). Internal
  dependency: `vesper-ffi` only (read-only), as the wave-five contracts allow.
  External: `lang-c =0.15.1`, `syn =2.0.119` (`full`, `visit`, `clone-impls`),
  `proc-macro2 =1.0.107` (`span-locations`), `quote =1.0.47`. `syn`,
  `proc-macro2` and `quote` reuse versions already in `Cargo.lock`; `lang-c` is
  the one new package.

## What was built

### C header front end (`c::analyze_header`)

`lang-c` parses only preprocessed C, and running a host preprocessor would
behave differently on the Linux, macOS and Windows runners. So the front end
accepts a documented preprocessor-free subset and rejects everything else
with `ErrorKind::Preprocessor` and a line number:

- comments are stripped before parsing (line numbers are kept);
- `#include <stdint.h>`, `<stddef.h>`, `<stdbool.h>` (their types are built
  in through a typedef prelude; a header that redefines one is a `Duplicate`
  error);
- `#pragma once`, include guards (`#ifndef X` / `#define X` / `#endif` where the
  define has no body), and `#ifdef __cplusplus ... #endif` blocks, which are
  skipped;
- any other `#define`, `#include`, `#if`, or a line continuation is rejected.

Lifted: function prototypes, `struct` and `enum` definitions (tagged, or
anonymous ones named by their typedef), `typedef` chains, pointers with
pointee and pointer `const`-ness, arrays (array parameters decay), and the
fixed-width integers. `int`, `unsigned`, `short` and `long long` are accepted
because they have one width on all four supported targets. Enumerator values
are evaluated (literals in any base, `-`, `+`, `|`, `<<`, earlier
enumerators).

Mapping to `.vsys`: a scalar pointer followed by a `size_t` becomes
`nocapture in|inout CSlice[T]` (`in` for `const T *`). The capture claim is an
**assumption** a header cannot justify; it is printed as an `assumed:` comment
above the function in the `.vsys` and in the report. Struct layouts become
native-endian `vesper_ffi::Declaration`s and are compiled by
`vesper_ffi::Registry`, so implicit padding (for example `uint8_t` then
`uint32_t`) is a `WireLayout` refusal that quotes the Bridge's
`add explicit ZeroPad[3]` advice. A struct that contains a refused struct is
refused at a fixpoint.

Refused, with a typed `RefusalReason`: plain `char` and `long`
(`PlatformDependentWidth`), floating point, `_Bool` and C enums as ABI
carriers (`ImplementationDefinedCarrier`), structs by value or by pointer
(`Aggregate`), `void *` (`OpaquePointer`), single-element pointer parameters
(`PointerParameter`, see below), unions, bit-fields, variadics, function
pointers, `_Atomic`, globals, `static` functions, and `()` non-prototypes.

### Rust crate front end (`rust::analyze_crate_dir`, `rust::analyze_source`)

`analyze_crate_dir` reads the package name from `Cargo.toml` and follows
`pub mod x;` from `src/lib.rs` (`x.rs` or `x/mod.rs`), under file, byte and
item budgets. It lifts public free functions and public inherent methods.

Borrow summaries to modes: `&T` / `&self` become `in`, `&mut T` /
`&mut self` become `inout`, owned non-`Copy` values become `sink`, and `Copy`
scalars and fieldless `#[repr(uN)]` enums stay plain values. When the
borrowed type is a crate struct, a syntactic body scan builds a
`LegacyRustParamSummary` and the parameter gets a FieldTree footprint:

```text
extern fn parse_batch_header(in raw_bytes: ByteSlice, inout state.{last_crc, record_count}: DecoderStats) -> Result[i64, ParseError];
extern fn DecoderStats::summary(in self.{bytes_seen, last_crc}: DecoderStats) -> u64;
extern fn DecoderStats::reset(inout self: DecoderStats) -> Unit;
```

The scan is conservative: assignment, compound assignment, `&mut` of a field
and (for unique borrows) a method call on a field are writes; any whole-value
use, a shadowing binding, or a mention inside a macro drops the footprint. A
`&mut` parameter whose body only reads fields is narrowed to `in x.{...}`, as
the design's lifter does.

Refused with reasons: raw pointers without an annotation
(`RawPointerWithoutAnnotation`), `Cell`/`RefCell`/`UnsafeCell`/atomics/locks/
`Rc`/`Arc` and structs containing them, transitively
(`InteriorMutability`), `&'a mut T<'a>` (`SelfReferentialLifetime`), returned
borrows (`ReturnedBorrow`), types with lifetime parameters or nested borrows
(`BorrowingType`), generics, `impl Trait`, `dyn Trait` (`Generic`), function
pointers, `async fn`, destructuring parameters, data-carrying enums, tuple
structs, traits, consts and statics.

Raw-pointer annotation, written as a doc line so the crate still compiles:
`/// vesper-migrate: <param> in|inout [len <usize param>]`. Malformed,
mismatched (`*const` annotated `inout`) or dangling annotations are
`BadAnnotation` refusals.

Only `#[no_mangle]` / `#[unsafe(no_mangle)]` `extern "C"` functions get
`.vsys` entries. Every other function still gets a Vesper declaration plus a
`NotCAbi` note. A Rust struct gets a wire layout only with `#[repr(C)]`.

### Synthesis (`synth`)

- `sys_spec(&Migration, &SysSpecOptions) -> SysSpec` renders a declarative
  Bridge spec in `vesper.vsys/1` syntax, **parses it with
  `vesper_ffi::sys::parse`**, and compares every parsed function (parameter
  names, slice element/access, value carriers, result) with what it rendered
  before returning. A spec that the Bridge rejects is an
  `ErrorKind::Synthesis` error, never output. Target, link mode, link name,
  artifact digest, header and pkg-config are explicit options, since a header
  does not determine them.
- `vesper_declarations` renders structs (`@wire_layout(native)` when the
  layout compiled), enums, and `extern fn` signatures with modes and
  footprints.
- `report` lists lifted functions with their assumptions, compiled wire
  layouts with size and alignment, and every refusal with its reason.
- `lift::lift_rust_param_to_vesper_projection` keeps the design's rendering
  rules. One correction: the summary also records `whole_value_access`. The
  design sketch would render a unique borrow that is passed on whole as
  `in x`, which drops the write; here it stays `inout x`. Footprints are
  reduced to an antichain (`hdr` covers `hdr.crc`).

### Differential shadow harness (`shadow`)

`shadow::run(config, boundary, generate, legacy, replacement)` runs both
sides on caller-supplied boundary inputs and then on `config.cases` inputs
from a seeded SplitMix64 stream. It stops at the first input where the
outcomes differ, where an outcome is a returned value or a caught panic, and
reports the case index, the input and both outcomes. `shrink_bytes` greedily
reduces a diverging byte input (chunk deletion, then byte lowering; every
accepted step decreases (length, sum) in shortlex order, so it terminates).
`ShadowReport::isolation` is always `Isolation::Unavailable`, and its
`Display` says so and says that finite testing is not an equivalence proof.

The legacy side is a Rust closure. No C is compiled: `build.rs` C compilation
would need a C toolchain on every CI platform, and calling a compiled C
library needs unsafe FFI, which this lane may not use. A legacy C function is
shadowed through a Rust closure over its C-ABI shim.

### Worked example (`tests/fixtures`)

- `legacy_codec.h`: a C batch-header codec with structs, an enum, typedefs,
  array parameters, slice and out-pointer entry points, and one declaration
  for each refusal.
- `legacy_codec/`: a Rust crate (`parse.rs`, `stats.rs`, and a `lib.rs` with
  annotated `extern "C"` shims and items that must be refused).
- `expected/legacy_codec.{c,rs}.{vsys,vesper,report.txt}`: committed outputs.
  The golden test compares them exactly (CRLF-normalized);
  `VESPER_MIGRATE_BLESS=1` regenerates them for review.

The shadow test compiles the fixture's `parse.rs` and `stats.rs` as real code
(`#[path]` modules) and runs the legacy parser against a migrated replacement
that reads a checked big-endian `vesper_ffi` wire view (the design's
`view_wire_prefix[KafkaBatchWireHeader]` shape) and writes only
`state.{last_crc, record_count}`.

## Acceptance tests

| Test file | What it checks |
| --- | --- |
| `tests/golden.rs` (6) | All six golden outputs. C spec round-trips on all four targets with the expected ABI expansion (slice to pointer + `usize`) and a stable content identity. Typedef, array-decay, const-ness and layout sizes (`batch_header` 24/8, `decoder_stats` 32/8). Exact Rust modes and footprints. Only the three annotated C-ABI functions reach the Rust spec. Expected refusal for every refused fixture item. |
| `tests/refusals.rs` (12) | Preprocessor subset and line numbers. Every C and Rust refusal reason. Enum constant evaluation. Reserved parameter names renamed (`in` becomes `in_`). Bad link name or header path is a `Synthesis` error. The body-scan cases (compound assignment, indexing, method on field, `&mut` field, pass-on, shadowing, macro, closure, deref, unused). Typed syntax, duplicate and I/O errors. Byte, item, parameter and nesting budgets (5000-deep parens and generics are `Limit`, 1000 shifts are not). Every 7th/11th prefix and 200 random corruptions of the fixtures produce results or typed errors, never panics. |
| `tests/shadow.rs` (4) | Legacy and migrated parsers agree on 5 boundary + 20 000 generated inputs. Three planted bugs (off-by-one length, loose magic check, wrong endianness) are each caught, the reported input reproduces the divergence, and shrinking keeps it diverging (the off-by-one shrinks to one 25-byte header). Boundary inputs catch the off-by-one at case 2. Panics are reported as outcomes. Runs are reproducible from the seed. |
| unit tests in `src/lift.rs` (3) | The design's rendering rules, whole-value unique borrows, antichain footprints. |

## Commands and results (Linux x86_64, Rust 1.97.0)

```text
cargo fmt --all -- --check                                              ok
cargo clippy -p vesper-migrate --all-targets --locked -- -D warnings    ok
cargo test -p vesper-migrate --locked                                   25 passed
cargo clippy --workspace --locked --all-targets -- -D warnings          see PR
cargo test --workspace --locked                                         see PR
```

## Root patch

A separate commit registers `crates/vesper-migrate` in root `members` and
`default-members` (before `experiments/native-pulse`) and adds the new
packages to `Cargo.lock`. No workspace dependency entries, `src/**` wiring or
CI edits.

## Decisions and corrections

- **Single-element pointers are not `.vsys` parameters.** The design's
  sketch writes `nocapture inout out_crc: u32`, but `vesper_ffi::sys::parse`
  accepts pointer parameters only as `nocapture` `CSlice` (pointer plus
  length) and rejects address parameters. Emitting the sketch's form would
  produce a spec the Bridge rejects. So C out-pointers are `PointerParameter`
  refusals, Rust annotated single-element pointers keep their `inout` mode in
  the Vesper declaration but get no `.vsys` entry, and the fixture adds slice
  forms (`legacy_parse_batch_header_into`) that do round-trip. Supporting
  single-element nocapture pointers is a `vesper-ffi` change, outside this
  lane.
- The `rust_crate = "..."` library option in the design's example is not a
  `vesper.vsys/1` option, so it is not emitted. The crate name appears in the
  header comment.
- The design's `.vsys` file name is `sys/legacy_codec.vsys`; the parser's
  schema is `vesper.vsys/1`. Output text is returned to the caller; writing
  files is CLI work (integration B).
- C enums are recorded with their values, but never become `.vsys` enum
  carriers, because their width is implementation-defined. Rust enums need
  `#[repr(u8|u16|u32|u64)]`; signed reprs are refused because `.vsys` enum
  carriers are unsigned.

## Limits and remaining gaps

- Finite differential testing over generated inputs is **not an equivalence
  proof**. The §29.4 graduation to Lean equivalence is not attempted here.
- MPK/PKRU and POR_EL0 isolation is **not exercised**; the harness reports it
  unavailable and runs the legacy side in-process.
- The lifted modes are summaries of signatures and a syntactic body scan.
  They are not an implementation of Vesper's source typing rules, and the
  Rust ownership of this crate's types is not evidence that those rules hold.
- No `Mode::Linear` obligation inference (line 671 of the specification): a
  signature does not show which returned values must be consumed.
- The C front end does not run a preprocessor. Real-world headers with macros
  must be reduced to the subset first.
- The Rust front end does not follow `pub use` re-exports or `#[path]`
  modules, does not resolve items in private modules, and does not lift trait
  methods.
- `vesper migrate --source ...` CLI wiring and `vesper fuzz --differential`
  belong to integration B (#136). The Vesper-side `@differential_shadow`
  attribute and `Crucible::assert_differential_equiv` are not source-language
  features yet.

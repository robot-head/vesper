# Wave seven lane 6: Bridge wire-struct and Option slices, nullable pointers in `.vsys`

Design coverage: §13.2 (`wire struct` layouts), §13.3 (`.vsys` bindings and
`nocapture` borrows), §29.3 (`vesper migrate` generating `.vsys` from headers).
Claim issue #329.

- Base: `283c8115d2203232d0e9b473cc4bb0312c3a8509` (origin/main, a descendant
  of the required baseline `c025b9a`).
- Branch: `claude/project-thread-6c4idm`.
- Paths edited: `crates/vesper-ffi/**`, `crates/vesper-migrate/**`, this
  report. No root patch: no dependency, manifest or lockfile change.
- Single out-pointers (wave six, PR #281) were not redone.

## What was built

### 1. Slices of wire structs and `Option` slices (`vesper_ffi::sys`)

```text
wire struct rest_rec { id: u32, flags: u32, value: i64 }
fn rest_sum(nocapture in recs: CSlice[rest_rec]) -> ForeignVal[i64];
fn rest_bump(nocapture inout recs: Option[CSlice[rest_rec]], flag: u32) -> ForeignVal[Unit];
fn rest_checksum(nocapture in data: Option[CSlice[u8]]) -> ForeignVal[u32];
```

- `ParameterType::Slice { element: Pointee, access, nullable }`: the element
  is an integer or a declared `wire struct` (the same `Pointee` type single
  pointers use). `ParameterType::slice_layout() -> SliceLayout { element_size,
  alignment, access, nullable }`. ABI lowering is unchanged: pointer plus
  `size_t` element count; the C stride is the element's wire size.
- `prepare_call` checks a struct slice's base against the struct's alignment,
  `length * size` for overflow and the `i64::MAX` byte cap, and overlap against
  every other borrow when either is written.
- `Option[CSlice[T]]` admits exactly `(NULL, 0)` as `None`;
  `(NULL, n > 0)` is the new `ValidationError::NullSliceWithLength`.
- Typed parse errors: `out` slices (with or without `Option`); `Option`,
  `Ptr`, `NullablePtr`, nested `CSlice`, `bool`, enum, `Unit` and unknown slice
  elements; `Option[Option[...]]`.
- Identity: integer non-null slices keep their exact canonical bytes; the new
  forms use a separate `nocapture-slice/2` tag, so old descriptors keep their
  identities (the wave-six pinned-identity test still passes) and every new
  form has a distinct identity.

### Layout checks against the C header

`WireStructDescriptor::c_layout_probe(c_type, include)` renders a C11 program
that prints `sizeof`, `_Alignof`, and `offsetof`/`sizeof` of every field,
nested fields by path (`rec.id`). The test compiles it with the host `cc`
against the real header; `check_c_layout(output)` returns every
`CLayoutMismatch`: `Size`, `Alignment` (C needs more alignment than the wire
layout checks for), `FieldOffset`, `FieldSize`, `MissingField`, or `Malformed`
for output that is not the probe's (budget 64 KiB; never a panic). The library
itself runs no compiler.

### 2. Nullable C pointers lifted as `Option` (`vesper_migrate::c`)

A header pointer is now `Option[...]` unless the header says it is non-null:

- GNU `__attribute__((nonnull))` (every pointer parameter) or
  `__attribute__((nonnull(i, ...)))` (1-based indices), before or after the
  declarator;
- Clang `_Nonnull` on the pointer; `_Nullable` is accepted and stays `Option`;
- `__attribute__((returns_nonnull))` or a `_Nonnull` return pointer turns
  `ForeignVal[NullablePtr[T]]` into `ForeignVal[Ptr[T]]`.

The front end now parses with lang-c's Clang C11 flavor to accept those.
Contradictions and wrong indices are refused as `BadAnnotation`: `_Nullable`
plus `nonnull`, an index naming a non-pointer, out of range, or 0 (GCC rejects
the same header). Every lifted pointer's `assumed:` line says which way its
nullability was decided.

Struct pointer plus `size_t` (previously refused as `Aggregate`) now lifts to
`CSlice[S]` / `Option[CSlice[S]]` when `S` has a checked wire layout; padded or
undefined structs are refused as `PointerParameter`. Aliasing refusals cover
struct slices too (a written single `S *` next to an `S` slice).

The Rust annotation grammar gains a trailing `nullable`
(`/// vesper-migrate: data in len n nullable`), and `len` now accepts crate
struct pointees (`CSlice[Pair]`). Unannotated Rust references stay non-null.

`SysParam::ElementSlice { element, access, nullable }` is additive;
`SysParam::Slice` remains the non-null integer form (`SysParam::slice()` picks
the right one, `slice_parts()` reads both).

The worked example header gained `nonnull` attributes where the §29.3 design
signature is non-null, so
`fn legacy_parse_batch_header(nocapture in raw_bytes: CSlice[u8], nocapture inout out_crc: u32, nocapture inout out_count: i32) -> ForeignVal[i32];`
is still produced verbatim; `legacy_crc32c`, `legacy_encode` and `legacy_fill`
now lift as `Option`, and two new struct-slice functions lift. Goldens were
regenerated with `VESPER_MIGRATE_BLESS=1` and reviewed.

### 3. Per-assumption checks at the boundary

`FunctionDescriptor::assumptions()` lists each assumption with when it is
checkable. `check_arguments(args)` and `check_boundary(args, return_bits,
guards)` return a `BoundaryReport` with one `AssumptionCheck { position,
parameter, assumption, outcome }` per assumption and argument:

| Assumption | Checked | Violation |
| --- | --- | --- |
| `ArgumentShape` | before call | `ArgumentCount`, `ArgumentKind` |
| `ValidBits` (value args, non-pointer returns) | before call / after return | `Width`, `InvalidBool`, `InvalidEnum`, `NonzeroVoid` |
| `NonNull` | before call; returned `Ptr[T]` | `NullAddress`, `NullPointer`, `NullSliceWithLength` |
| `Aligned` | before call; returned pointer | `MisalignedAddress`, `MisalignedPointer` |
| `InBounds` | before call | `SliceRangeOverflow`, `SliceTooLarge`, `PointerRangeOverflow` |
| `Disjoint` | before call | `OverlappingSlices` |
| `SingleElement` (pointer) | after return, with a guard | `GuardOverwritten` |
| `WithinLength` (slice) | after return, with a guard | `GuardOverwritten` |
| `NoCaptureViaReturn` | after return (pointer results) | `ReturnAliasesBorrow` |
| `NoRetention` | never | always `Unchecked` |

`prepare_call` now fails with the first violation of the same report, so its
errors and their order are unchanged. A `GuardObservation { argument, before,
after }` holds the bytes just past a borrow, read by the (unsafe) caller
before and after the call from memory it owns; malformed, oversized (> 4 KiB),
duplicate or misdirected observations are `Unchecked`, never `Held`.

### 4. `vesper migrate`

`vesper migrate` (the root CLI, unchanged) calls `c::analyze_header` and
`synth::sys_spec`, so it emits the new forms directly; `tests/migrate_cli.rs`
still passes.

## Tests against real C

All compile the C code in the test with the host `cc` (`-std=c11 -Wall
-Wextra -Werror -O2`); they skip with a message where there is no `cc` or the
host is not a Linux/Darwin 64-bit descriptor target (Windows CI).

- `vesper-ffi/tests/sys_vsys_rest_c.rs`: a C library and a C host (no Rust
  `unsafe` anywhere in the test) place each borrow inside a larger array and
  print raw arguments, returns and guard bytes. Positive: struct `in`/`inout`
  slices (sum and in-place update correct, guard intact), `Option` slice and
  `Option` pointer called with `NULL` (C returns its `NULL` result), a
  one-element writer. Negative: a 4-but-not-8-aligned struct slice, an
  overflowing struct slice length, `(NULL, 3)` for an `Option` slice, `NULL`
  for a non-`Option` slice, a C function that writes two elements through a
  single pointer (`GuardOverwritten` at byte 0), one that writes `recs[n]`
  (`GuardOverwritten` at byte 4), one that returns a pointer into its borrowed
  slice (`ReturnAliasesBorrow`), and a null `Ptr[T]` return.
- Same file, layout probes against `vsys_rest.h`: the matching struct (by
  typedef and by tag), a nested struct with an explicit `ZeroPad` the C struct
  leaves unnamed (`MissingField`), swapped fields (two `FieldOffset`), a
  narrower field (`FieldSize`), byte-aligned big-endian fields (`Alignment`
  1 vs 8), malformed output and rejected probe inputs.
- `vesper-migrate/tests/nullability.rs`: compiles `legacy_codec.h` (so its
  `nonnull` attributes are valid C) and checks every wire struct migrate
  emitted from it against the compiler's layout.

## Checks

Host: Linux x86_64 cloud VM, kernel 6.18, GCC 13.3.0, Rust 1.97.0 and 1.100.0.
This VM reports no PKU, so the wave-six isolation tests reported
"unavailable" here (they are unchanged).

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo test --locked -p vesper-ffi` | 48 passed: isolation 5, sys 15, sys_c_abi 1, sys_pointers 10, sys_slices 6, sys_vsys_rest_c 2 (real C), wire 9 |
| `cargo test --locked -p vesper-migrate` | 44 passed: unit 3, golden 7, nullability 5 (one real C), refusals 14, review 7, shadow 8 |
| `cargo test --locked --test migrate_cli` | passed |
| `cargo +1.100.0 clippy --locked --all-targets -p vesper-ffi -p vesper-migrate -p vesper-warp-umem -p vesper-chrono -p vesper-nexus -p vesper-warp-tcp -p vesper-warp -- -D warnings` (every crate that depends on `vesper-ffi`) | clean |
| `cargo clippy --locked --all-targets -p vesper-ffi -p vesper-migrate -- -D warnings` (1.97) | clean |

`cargo test --workspace --locked` was not run locally (the workspace is
large); the PR's CI runs the full Bazel suite, Windows Cargo and macOS jobs.

## Decisions

- A non-`Option` `CSlice` is now non-null even when empty; the earlier rule
  let `(NULL, 0)` through. A header without `nonnull` lifts to
  `Option[CSlice[T]]`, which is where `(NULL, 0)` belongs, and many C APIs
  (`memcpy` among them) do not accept `NULL` with length 0. One existing test
  changed accordingly.
- `in` and `inout` slice elements may be wire structs but not `bool`/enums,
  as before for integers: foreign code may write any bit pattern into
  `inout`, and the rule is kept uniform for `in`.
- `NoCaptureViaReturn` checks only that a returned pointer is outside every
  borrowed range. A callee can still keep a pointer in a global; that is
  `NoRetention`, reported as unchecked rather than guessed.
- Guard checks compare bytes the caller observed; the library still reads no
  memory and stays free of `unsafe` outside the wave-six isolation module.
  A changed guard proves an overrun; an unchanged one does not prove its
  absence (the callee may write the same bytes, or past the guard).
- The C layout check requires C's alignment to be at most the wire
  alignment, not equal: a stricter wire alignment only rejects more slices.
- C nullability defaults to `Option` for every pointer the header does not
  mark, including `const` inputs. `_Nullable` is accepted silently because it
  agrees with the default.
- `isolated(mpk)` trampolines: compiler lowering of
  `handle Ffi[Lib] isolated(mpk)` stays with the native backend lane; nothing
  here emits or lowers trampolines.

## Remaining gaps

- No compiler integration: source `.vsys` imports, `Ffi[Lib]` capabilities,
  native calls and `isolated(mpk)` trampolines are not lowered here (native
  backend). Rust ownership in these crates is not evidence that the Vesper
  source typing rules for these forms are implemented.
- `nocapture` beyond the returned-pointer range check, and single-element use
  without a guard, remain declared assumptions.
- Only `nonnull`, `returns_nonnull`, `_Nonnull` and `_Nullable` are read.
  `[static 1]` array parameters, `__attribute__((access(...)))` and SAL
  annotations are not, and the C front end still needs preprocessor-free
  headers.
- Slices of slices, slices of pointers and `out` slices are rejected; there is
  no `.vsys` form for a struct with pointer fields.
- Real-C runtime evidence covers Linux x86_64 here and whatever macOS CI
  runs; Windows MSVC has no `cc` in CI and reports the fixtures unavailable.

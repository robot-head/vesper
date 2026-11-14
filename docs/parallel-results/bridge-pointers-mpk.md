# Wave six lane 1: Bridge single out-pointers and MPK/POE isolation domains

Design coverage: §13.2 (`wire struct` pointees), §13.4 (hardware isolation
domains), §29.3 (`vesper migrate` lifting of out-parameters and the
MPK-sandboxed differential shadow). Claim issue #243.

- Base: `07322e3ba47890e939a48967a1ea8f62fdd7aac1` (origin/main, a descendant of
  the required baseline `cda092f`).
- Branch: `claude/project-thread-sj4kaz`.
- Paths edited: `crates/vesper-ffi/**`, `crates/vesper-migrate/**`, this
  report, and a one-line root `Cargo.lock` patch in its own commit.

## What was built

### 1. Single-element pointers in `.vsys` (`vesper_ffi::sys`)

New parameter forms, each exactly one C pointer:

```text
wire struct Header { base_offset: i64be, crc: u32be, records: i32be }
fn crc32_update(nocapture inout crc: u32, nocapture in data: CSlice[u8]) -> ForeignVal[Unit];
fn legacy_parse_batch_header(
    nocapture in raw_bytes: CSlice[u8],
    nocapture inout out_crc: u32,
    nocapture inout out_count: i32,
) -> ForeignVal[i32];
fn read_header(nocapture in raw: CSlice[u8], nocapture out hdr: Option[Header]) -> ForeignVal[i32];
```

Public API (all additive):

- `PointerAccess::{In, Out, Inout}` (`keyword()`, `writes()`).
- `ParameterType::Pointer { pointee: Pointee, access, nullable }`;
  `Pointee::{Scalar(Scalar), Struct(WireStructDescriptor)}` with `size()`,
  `alignment()`, `name()`.
- `ParameterType::pointer_layout() -> Option<PointerLayout { size, alignment,
  access, nullable }>`: the typed layout of each form.
- `wire struct` declarations in `.vsys`, compiled through the existing
  `vesper_ffi::Registry` (zero implicit padding, total bit validity, nested
  structs, recursion rejected). `WireStructDescriptor { name(), declaration(),
  layout() }`; `LibraryDescriptor::wire_structs()` / `wire_struct(name)`.
- `Limits { wire_structs: 64, wire_fields: 256 }` (new fields; lowered limits
  are enforced, raised ones rejected like the others).
- `RawArgument::Pointer { address }`; `prepare_call` checks null (only
  `Option[T]` admits it), pointee alignment, range overflow, and overlap with
  every other slice/pointer range when either side is written. New
  `ValidationError::{NullPointer, MisalignedPointer { alignment },
  PointerRangeOverflow}`.
- `AbiSignature` lowers each single pointer to one `AbiType::Pointer`.

Typed rejections: an access keyword without `nocapture`; `nocapture` without
`in`/`out`/`inout`; `NullablePtr[T]` as a pointee ("declare `Option[T]`");
`Ptr[T]`, `Option[Ptr[T]]`, `Option[NullablePtr[T]]` (pointer-to-pointer);
`out` slices; `bool`/enum pointees (foreign code can write any bit pattern);
`Unit` and unknown pointees; padded, `usize`, `bool`, enum-carrying,
recursive, duplicate or oversized (> 1 MiB) wire structs.

Old forms are byte-identical: the canonical encoding appends a
`wire-structs` section only when one is declared, and a test pins the
identity of an old-form descriptor computed on the baseline
(`old_forms_keep_their_exact_canonical_identity`).

Real execution: the existing Linux x86_64 C/Rust ABI fixture now also calls
`bridge_crc_update(uint32_t *crc, const uint8_t *, size_t)` and
`bridge_split(uint64_t, bridge_pair *out)` through descriptors declared with
`nocapture inout crc: u32` and `nocapture out out: Pair`.

### 2. `vesper-migrate` lifts out-parameters

- C (`c::analyze_header`): `T *p` / `const T *p` with an integer or
  wire-laid-out struct pointee, not followed by `size_t`, becomes
  `nocapture inout|in p: T` with an `assumed:` line (a header cannot show
  non-null, single-element or non-retention). `legacy_parse_batch_header`,
  `legacy_fill(struct decoder_stats *)` and a new
  `legacy_read_header(..., batch_header_t *out_hdr)` now lift; their struct
  pointees are emitted as `wire struct` declarations (transitively).
- Rust (`rust::analyze_*`): in `#[no_mangle] extern "C"` functions, `&T` /
  `&mut T` of an integer or crate struct lift to `nocapture in|inout`; annotated
  raw pointers without `len` lift to single pointers, and the annotation
  grammar gains `out` (`/// vesper-migrate: stats out`). The design's §29.3
  entry is produced verbatim:
  `fn legacy_parse_batch_header(nocapture in raw_bytes: CSlice[u8], nocapture inout out_crc: u32, nocapture inout out_count: i32) -> ForeignVal[i32];`
- Still refused, with new typed reasons where needed:
  `RefusalReason::ArrayWithoutLength` (array parameters and byte pointers with
  no following length), `RefusalReason::AliasingPointers` (two pointers to the
  same pointee type, one a single pointer, one written: C `swap(uint32_t *,
  uint32_t *)`, annotated Rust raw-pointer pairs), `OpaquePointer` (`void *`,
  `T **`), `Aggregate` (struct pointer + `size_t`, i.e. struct arrays),
  `PointerParameter` (a struct pointee without a checked wire layout, a
  borrowed non-struct named type), `BadAnnotation` (`out` with `len`, a
  `*const` annotated `out`/`inout`).
- `SysParam::Pointer { pointee: SysPointee, access, nullable }`,
  `SysPointee::{Scalar, Struct}`, `PointerAccess` re-exported;
  `synth::aliasing_pair` and `synth::check_pointer_pointees` are public
  helpers. Every synthesized spec is still re-parsed by `vesper_ffi::sys` and
  compared back parameter by parameter (including wire struct declarations).
- Goldens regenerated with `VESPER_MIGRATE_BLESS=1` and reviewed.

### 3. Isolation domains (`vesper_ffi::isolation`, §13.4)

- `probe() -> Result<Mechanism, Unavailable>`; `Mechanism::{IntelPku, ArmPoe}`;
  `Unavailable { reason: UnavailableReason, detail }` with
  `UnsupportedTarget`, `CpuLacksFeature`, `KernelLacksSupport`,
  `KeysExhausted`, `Os(errno)`.
- `IsolationDomain::new()` allocates a key (`pkey_alloc`);
  `add_region(len)` maps zeroed page-aligned anonymous memory tagged with it
  (`pkey_mprotect`); `read`/`write` copy through raw pointers (region memory
  is never a Rust reference, so revoking rights never invalidates one);
  `region_address`, `region_len`, `rights()`;
  `with_revoked(Revoke::Write | Revoke::Access, f)` writes PKRU (`WRPKRU`) or
  `POR_EL0` (`MSR` + `ISB`) and restores the saved value through a drop guard on
  every exit, including unwinding panics; nested calls restore in order.
  `Drop` unmaps regions before `pkey_free`, so a key is never reused while
  pages carry it. The type is `!Sync` (rights are per-thread).
- `contain(timeout, f) -> Contained { messages, exit: ChildExit }` runs `f`
  in a forked child with a `SIGSEGV`/`SIGBUS` `SA_SIGINFO` handler that reports
  signal, `si_code` and fault address through a pipe and `_exit`s. A
  protection-key fault is `ChildExit::Fault { protection_key: true, .. }`
  (`SEGV_PKUERR`). Panics, other signals, unexpected exits and timeouts
  (child killed) are typed too. The child closes inherited descriptors other
  than stdio and its pipe.
- Unsafe: `vesper-ffi` changed from `forbid(unsafe_code)` to
  `deny(unsafe_code)`; the only `#[allow(unsafe_code)]` is `isolation::hw`,
  compiled only for Linux x86_64 and Linux AArch64, with a `SAFETY:` comment
  on each block. Other targets get a safe stub that reports
  `UnsupportedTarget`. It also uses `mmap`/`munmap`, `fork`/`pipe2`/`poll`/
  `waitpid`, `sigaction` and `getauxval` beyond the pkey calls the prompt
  names: mapping the region and containing the fault are part of the domain.

### 4. MPK-isolated differential shadow (`vesper_migrate::shadow`)

`shadow::run_isolated(config, boundary, generate, legacy, replacement,
domain, timeout)` runs the whole case loop in a contained child; each legacy
call runs under `domain.with_revoked(Revoke::Write, ..)`, so the
replacement's Vesper-owned buffers (put in the domain's regions) are
write-protected while legacy code runs. Results:

- agreement: `Isolation::Hardware { mechanism }`;
- a legacy write into Vesper memory: `ShadowReport::isolation_failure =
  Some(IsolationFailure { case, input, exit })` with
  `is_protection_violation()`, the input regenerated from the seed;
- a divergence: re-executed in the caller to recover both outcomes (sides
  must be deterministic, as reproducibility already required);
- no PKU/POE (`domain = Err(&Unavailable)`) or the child cannot start: exactly
  `run`, reported as `Isolation::Unavailable` with the reason.

`ShadowReport::agreed()` is now false on an isolation failure.

## Checks

Host: Linux x86_64 cloud VM, kernel 6.18, CPU flags include `pku` and `ospke`
(the earlier note that the cloud host lacks PKU no longer holds for this
runner). Rust 1.97.0.

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy -p vesper-ffi -p vesper-migrate --all-targets --locked -- -D warnings` | clean |
| `cargo clippy --locked -p vesper-warp-umem -p vesper-chrono -p vesper-nexus -p vesper-warp-tcp -p vesper-warp -p vesper-ffi -p vesper-migrate --all-targets -- -D warnings` (every crate that depends on `vesper-ffi`) | clean |
| `cargo clippy --lib --target aarch64-unknown-linux-gnu / aarch64-apple-darwin -- -D warnings` on the isolation module (standalone copy with only `libc`; `blake3`'s C build blocks a full cross check here) | clean: the POE path and the stub both compile warning-free |
| `cargo test --locked -p vesper-ffi` | 40 passed: isolation 5, sys 15, sys_c_abi 1 (real C), sys_pointers 10, wire 9 |
| `cargo test --locked -p vesper-migrate` | 39 passed: unit 3, golden 7, refusals 14, review 7, shadow 8 |
| `cargo test -p vesper-ffi --test isolation` | 5 passed, **real PKU**: write fault caught in a child with `SEGV_PKUERR` at the exact address; access-revoked read fault; restore after panic; timeout kill |
| `cargo test -p vesper-migrate --test shadow` | 8 passed, **real PKU**: stray legacy write reported as a protection-key isolation failure at the triggering case |

`cargo test --workspace --locked` was not run locally (the workspace is
large); the PR's CI runs the full Bazel suite, Windows Cargo and macOS jobs.

Which CI jobs exercised PKU: the isolation tests write
`isolation exercised: IntelPku` or `isolation unavailable on this host: ...`
straight to stderr (not captured by libtest). macOS (AArch64) has no Linux
POE, so it reports `UnsupportedTarget`; Windows likewise. The Linux Bazel job
runs on BuildBuddy runners; see the PR for what its log showed.

## Decisions

- Pointee aliasing is refused only for the *same* pointee type. C also lets
  `uint32_t *` alias `int32_t *` (and any byte pointer alias anything);
  refusing those would refuse the design's own §29.3 signature. Each lifted
  pointer carries an `assumed:` line, and `prepare_call` rejects overlapping
  written ranges at every call.
- A C non-const single pointer lifts to `inout`, not `out`: a header cannot
  show that the callee writes before reading. `out` comes from an explicit
  Rust annotation or a hand-written `.vsys`.
- A byte pointer without a length is refused (`ArrayWithoutLength`) rather
  than lifted as one `u8`; it is almost always a buffer or string.
- C functions whose struct pointee has no wire layout are refused whole (as
  other unliftable C signatures are); Rust functions keep their Vesper
  declaration and lose only the `.vsys` entry.
- Containment uses `fork`. The child is a single-threaded copy: if another
  parent thread is printing a panic at the instant of the fork, a panic in
  the child can block on std's stderr/backtrace locks until the timeout. The
  tests serialize forking tests for this reason.
- `shadow::Isolation` gained a variant and `ShadowReport` a field
  (`isolation_failure`). Code that destructured `Isolation::Unavailable`
  irrefutably must now match; nothing outside this crate did on `main`.
  `sys::Limits` gained two fields; struct-literal users need
  `..Limits::default()`.

## Remaining gaps

- No compiler integration: `handle Ffi[Lib] isolated(mpk)` is not lowered,
  no Cranelift trampoline issues `WRPKRU`/`MSR POR_EL0`, no stack pivot to a
  Domain 2 sandbox stack, and no separate foreign heap. The domain protects
  regions the caller allocates through it, not fiber stacks or arbitrary
  Vesper memory.
- Protection keys are per-thread rights in one address space. Legacy code
  that executes `WRPKRU`/`MSR POR_EL0` or calls `pkey_mprotect` can lift the
  restriction; this is a tripwire for stray writes, not a security boundary.
- The AArch64 POE path compiles but was not executed (no POE hardware here).
- `nocapture`, non-null and single-element remain declared assumptions; the
  Bridge checks numerical address ranges only. Rust ownership in these
  libraries is not evidence that the Vesper source typing rules for these
  forms are implemented.
- `.vsys` slices of wire structs, `Option` slices and nullable C pointers
  lifted from headers (always assumed non-null) are not supported.
- 1.5 ns domain switches are the design's claim; no timing was measured here.

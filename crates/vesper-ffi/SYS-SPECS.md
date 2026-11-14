# Bounded declarative `.vsys` contracts

`vesper_ffi::sys::parse(source, Limits::default())` parses and validates one
library descriptor without running build scripts, package tools, a C compiler,
or foreign code. This API is independent of `WireLayout`: native C scalar
signatures and byte-oriented wire representations have different contracts.

```text
sys library Codec (
    target = "x86_64-unknown-linux-gnu",
    abi = "sysv64",
    link = "static",
    name = "codec",
    artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000",
    header = "codec/codec.h",
    pkg_config = "codec ^1.2.3"
) {
    enum Status: u32 { Ok = 0, Full = 7 }
    fn encode(
        nocapture in src: CSlice[u8],
        nocapture inout dst: CSlice[u8],
        flags: u32,
    ) -> ForeignVal[Status];
    fn available() -> ForeignVal[bool];
    fn address() -> ForeignVal[NullablePtr[u32]];
}
```

The all-zero digest above is a placeholder to replace with the actual foreign
artifact's BLAKE3 content digest. `link().verify_artifact(bytes)` checks concrete
artifact bytes against that declaration. Content equality does not establish
origin, actual linker selection, safe behavior, or authority.

## Accepted grammar

A manifest contains exactly one `sys library Name (key = "value", ...) { ... }`.
ASCII identifiers match `[A-Za-z_][A-Za-z0-9_]*`; they have at most 128 bytes.
Whitespace and `//` line comments are accepted. Strings contain printable ASCII
with only `\"` and `\\` escapes. Trailing commas are accepted in option,
parameter and enum-variant lists. Integer literals are unsigned decimal `u64`.
Unknown syntax/options are errors. There is no imperative code or inclusion.

Required options are `target`, `abi`, `link`, `name` and `artifact`. The header
and pkg-config specification are optional metadata and are never resolved or
executed by the parser. Link names are nonempty bare `[A-Za-z0-9_][A-Za-z0-9_-]*` names,
not filesystem paths, platform suffixes or linker flags. Headers are relative
include paths with nonempty components and no `.`/`..` components. Pkg-config
metadata is exactly `name =A.B.C`, `name ^A.B.C` or `name ~A.B.C`, with unsigned
32-bit decimal version components. Artifact digests use `blake3:` plus exactly
64 lowercase hexadecimal digits. Duplicate options are errors.

The explicit target/convention pairs are:

| Target | Convention | Pointer/size_t width | Endianness |
| --- | --- | --- | --- |
| `x86_64-unknown-linux-gnu` | `sysv64` | 64 | little |
| `aarch64-unknown-linux-gnu` | `aapcs64` | 64 | little |
| `aarch64-apple-darwin` | `darwin64` | 64 | little |
| `x86_64-pc-windows-msvc` | `win64` | 64 | little |

A mismatch or an unlisted target fails. Cross-target descriptors do not depend
on host endianness or `usize`. `Target::is_host` is only an inspection helper and additionally requires a
64-bit little-endian host data model; x86_64 GNU x32 and AArch64 ILP32 are not
compatible hosts.
`static` and `dynamic` are the only link modes. This milestone records C calling
conventions and signed scalar carriers; it does not assign registers/stack
slots or produce a trampoline. A later code generator must implement the
specified target's complete C calling convention, including narrow integer
extension rules. Function symbols are undecorated C names for the C toolchain.

Declarations are `enum Name: unsigned_scalar { Variant = value, ... }`,
`wire struct Name { field: type, ... }` or
`fn symbol(parameters...) -> ForeignVal[return_type];`. Forward enum references
are allowed. Names and discriminants within an enum are unique; the nonempty
set fits the explicitly specified `u8/u16/u32/u64` carrier. Duplicate function
symbols, parameter names and enum declarations are rejected.

Value parameters admit `u8/i8/u16/i16/u32/i32/u64/i64/usize`, `bool`, or a declared
enum. Borrowed slice parameters are `nocapture in|inout name: CSlice[E]` or
`nocapture in|inout name: Option[CSlice[E]]`, where `E` is an integer scalar
or a declared `wire struct`. Each slice expands in place to a C pointer
followed by `size_t` **element count**, not byte count; the C stride is the
element's wire size. A non-`Option` slice is non-null even when empty; an
`Option` slice admits exactly `(NULL, 0)` as `None` (a null pointer with a
nonzero length is `NullSliceWithLength`). `ParameterType::slice_layout()`
gives element size, alignment, direction and nullability. Read-only slices
use a const pointer; unique slices a mutable pointer. Neither is a by-value C
slice struct. The declared C API must actually have this flattened signature;
the library does not infer a real API's signature from a header.

Borrowed single-element pointer parameters are
`nocapture in|out|inout name: T` or `nocapture in|out|inout name: Option[T]`,
where `T` is an integer scalar or a declared `wire struct`. Each is exactly one
C pointer. `in` is a const pointee; `out` and `inout` are unique pointees the
callee may write (`out` additionally declares that the callee writes before
reading). Only `Option[T]` admits null. `ParameterType::pointer_layout()` gives
the pointee's size, alignment, direction and nullability:

```text
wire struct Header { base_offset: i64be, crc: u32be, records: i32be }
fn crc32_update(nocapture inout crc: u32, nocapture in data: CSlice[u8]) -> ForeignVal[Unit];
fn read_header(nocapture in raw: CSlice[u8], nocapture out hdr: Option[Header]) -> ForeignVal[i32];
```

Rejected with typed errors: an access keyword without `nocapture`;
`nocapture` without an access keyword; `NullablePtr[...]` as a pointee
(nullable single pointers must say `Option[T]`); `Ptr[...]`, `Option[Ptr[...]]`
and other pointer-to-pointer forms; `out` slices; `bool` or enum pointees
(foreign code may write any bit pattern); `Unit` and unknown pointees; slice
elements that are `Option`, `Ptr`, `bool`, enums, `Unit`, unknown or nested
slices; `out` slices, with or without `Option`.

### Layout checks against the C header

`WireStructDescriptor::c_layout_probe(c_type, include)` renders a C11 program
that prints `sizeof`, `_Alignof`, and `offsetof`/`sizeof` of every field
(nested fields by `outer.inner` path) of the C type as the header declares it.
The caller compiles and runs it with the target C compiler;
`check_c_layout(output)` returns every mismatch: `Size`, `Alignment` (C needs
a stronger alignment than the wire layout checks for, as with byte-aligned
explicit-endian fields), `FieldOffset`, `FieldSize`, `MissingField`, or
`Malformed` for output that is not the probe's.

### Per-assumption boundary checks

`FunctionDescriptor::assumptions()` lists every assumption of a signature with
when it can be checked (`BeforeCall`, `AfterReturn`, `WithGuard`,
`Unverifiable`). `check_arguments(args)` reports each before-call assumption of
each argument separately (`ValidBits`, `NonNull`, `Aligned`, `InBounds`,
`Disjoint`) without stopping at the first violation; `prepare_call` fails with
the first of them. `check_boundary(args, return_bits, guards)` adds the
after-return checks: return validity; `NoCaptureViaReturn` (a returned pointer
inside a borrowed range is `ReturnAliasesBorrow`); `SingleElement` and
`WithinLength`, checked only when the caller supplies a `GuardObservation` of
the bytes just past the borrow before and after the call
(`GuardOverwritten`); and `NoRetention`, which is always `Unchecked`.

`wire struct` fields are `u8/i8`, native `u16..i64` (natural alignment,
little-endian on every accepted target), explicit `u16be/u32le/...` (byte
aligned), `ByteArray[N]`, `ZeroPad[N]` (1 <= N <= 1 MiB) or another wire struct.
Layouts compile through `vesper_ffi::Registry`: implicit padding, `usize`,
`bool`, enums, recursion and unknown structs are errors, and a pointee is at
most 1 MiB. At most 64 wire structs with 256 fields each are accepted.

Every return explicitly uses `ForeignVal[T]`, where T is a supported scalar,
`bool`, declared enum, `Unit` (C void), `Ptr[integer]`, or `NullablePtr[integer]`.
Capturing raw-pointer parameters, structs/unions by value, arrays by value, callbacks, floats,
platform-dependent C `char/long`, variadics, opaque handles/finalizers, nullable
handle options and nested pointer layouts are unsupported and rejected.

## Raw returns and owned failures

Logical `bool` uses **C `uint8_t`**, and logical enums use their explicit unsigned
integer carrier. They do not import C `_Bool` or C implementation-defined enum
representations. A caller must capture total-bit raw values, never first create
an unchecked Rust bool, enum or reference. Exact-width bits are stored in `u64`;
narrow signed returns are first cast through the corresponding unsigned width
(for example `i16 as u16 as u64`). Higher bits are checked rather than truncated.
Void uses zero bits.

`function.capture_return(bits, owner).validate()` checks integer width,
boolean 0/1, enum membership, and pointer nullability/alignment. Signed values
are explicitly sign-extended after the width check. A validated address remains
an integer wrapper: no reference, allocation provenance, mapped-memory validity,
aliasing, dereference permission or foreign ownership is inferred. An optional
`validate_non_error(&codes)` rejects at most 256 caller-specified sentinel bit
patterns while retaining the raw value and owner on failure.

Both success and failure return ownership of the caller-supplied `owner`.
`InvalidForeignValue::into_parts()` returns the validation error and original
`ForeignValue`; its `into_parts()` returns unchanged raw bits and owner. No
foreign handle finalizer is run implicitly. These Rust APIs permit ordinary
Drop and do not establish source-language linear exhaustion.

`function.prepare_call(raw_arguments, owner)` validates argument shape/scalar
bits, slice nullability, integer element alignment, checked byte lengths,
`isize::MAX` byte bounds, address-range overflow, and overlapping ranges when
either slice is mutable. `RawArgument::Pointer { address }` is checked for
null (allowed only for `Option[T]`), pointee alignment, range overflow, and
overlap with every other slice or pointer range when either side is written.
Only `Option` slices permit a null address, and only with length zero;
non-null addresses of empty slices still satisfy element alignment. Shared overlaps and adjacent unique ranges
are allowed. Preparation returns flattened typed carriers plus owner, or the
complete original arguments and owner on any error. These are numerical checks;
the unsafe embedding remains responsible for actual live allocations, their
borrow permissions and all foreign behavior. No foreign code is called here.

## Bounds and descriptor identity

Limits can be lowered but not raised above the hard defaults: source 256 KiB,
32768 tokens, 256 functions, 32 parameters/function, 64 enums, 256 variants/enum,
4096 bytes/string, and 1 MiB canonical descriptor output. Zero/oversized limits
are errors. Identifier/string/count budgets are checked before retaining the
next element; canonical writes check arithmetic and capacity before extension.
The source-byte limit bounds lexing work even for whitespace/comments. Enum
variant tables use shared immutable storage, so each parameter/return occurrence
and captured return retains a reference to one table rather than expanding the
whole enum. Descriptor clones preserve that sharing.

`identity()` is `vesper.vsys/1:blake3:<digest>`. Its digest hashes canonical bytes
with schema, library alias, complete explicit target/convention/pointer contract,
link mode/name/artifact digest, optional header/pkg-config metadata, every enum
and variant, ordered parameters including access/nocapture/element types, and
return-validity contracts, single-pointer direction/nullability/pointee,
slice nullability and wire-struct elements (as a separate `nocapture-slice/2`
tag, so integer non-null slices keep their earlier bytes), and,
only when present, a trailing `wire-structs` section with every field.
Enum/function/wire-struct declarations and enum variants sort
by name; parameter order remains significant. Descriptors that use no wire
structs keep the canonical bytes, and identities, of the earlier schema. Every byte field is prefixed by
its little-endian u32 length; collection counts are framed little-endian u64
fields, and optional strings have explicit some/none tags. Whitespace, comments,
option order and declaration order do not affect identity. Parser budgets do
not change the identity of an accepted contract. There is no canonical-byte
parser accepting untrusted unchecked descriptors.

The identity is a descriptor content key. It is not a library provenance
certificate, Loom capability, executable-code certificate or header verifier.
This crate does not synthesize `Ffi[Lib]`, check package ceilings, implement
source borrowing, load/link libraries, emit native calls, prove `nocapture`,
or force finalization. MPK/PKU and POE isolation domains live in the separate
`isolation` module (see README.md); no `.vsys` call is lowered through them.

The integration test compiles a tiny real C static library and a separate raw
Rust C-ABI host using `cc`, `ar` and `rustc`. It executes scalar/signed returns,
invalid bool/enum carriers, a pointer return, disjoint pointer+length byte
copy, an `inout u32` single pointer and an `out` wire-struct pointer. Runtime evidence is specifically Linux x86_64 GNU System V.
A second fixture (`tests/sys_vsys_rest_c.rs`) compiles a C library and a C
host on any Linux/Darwin 64-bit host with `cc`: wire-struct `in`/`inout`
slices, `Option` slices and pointers called with `NULL`, guard-detected
single-element and slice-length overruns, a returned pointer into a borrowed
slice, and layout probes of the same header. Other target
descriptors have metadata validation tests; they are not platform execution
evidence. Missing fixture toolchains/other hosts are explicitly reported.

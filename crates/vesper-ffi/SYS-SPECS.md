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

Declarations are either `enum Name: unsigned_scalar { Variant = value, ... }`
or `fn symbol(parameters...) -> ForeignVal[return_type];`. Forward enum references
are allowed. Names and discriminants within an enum are unique; the nonempty
set fits the explicitly specified `u8/u16/u32/u64` carrier. Duplicate function
symbols, parameter names and enum declarations are rejected.

Value parameters admit `u8/i8/u16/i16/u32/i32/u64/i64/usize`, `bool`, or a declared
enum. Borrowed parameters are exactly `nocapture in name: CSlice[integer]` or
`nocapture inout name: CSlice[integer]`. Each slice expands in place to a C
pointer followed by `size_t` **element count**, not byte count. Read-only slices
use a const pointer; unique slices a mutable pointer. Neither is a by-value C
slice struct. The declared C API must actually have this flattened signature;
the library does not infer a real API's signature from a header.

Every return explicitly uses `ForeignVal[T]`, where T is a supported scalar,
`bool`, declared enum, `Unit` (C void), `Ptr[integer]`, or `NullablePtr[integer]`.
Raw-pointer parameters, structs/unions, arrays by value, callbacks, floats,
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
either slice is mutable. Empty slices permit a null address; nonnull addresses
still satisfy element alignment. Shared overlaps and adjacent unique ranges
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
return-validity contracts. Enum/function declarations and enum variants sort
by name; parameter order remains significant. Every byte field is prefixed by
its little-endian u32 length; collection counts are framed little-endian u64
fields, and optional strings have explicit some/none tags. Whitespace, comments,
option order and declaration order do not affect identity. Parser budgets do
not change the identity of an accepted contract. There is no canonical-byte
parser accepting untrusted unchecked descriptors.

The identity is a descriptor content key. It is not a library provenance
certificate, Loom capability, executable-code certificate or header verifier.
This crate does not synthesize `Ffi[Lib]`, check package ceilings, implement
source borrowing, load/link libraries, emit native calls, prove `nocapture`,
force finalization, or provide MPK/PKRU/POE isolation.

The integration test compiles a tiny real C static library and a separate raw
Rust C-ABI host using `cc`, `ar` and `rustc`. It executes scalar/signed returns,
invalid bool/enum carriers, a pointer return and disjoint pointer+length byte
copy. Runtime evidence is specifically Linux x86_64 GNU System V. Other target
descriptors have metadata validation tests; they are not platform execution
evidence. Missing fixture toolchains/other hosts are explicitly reported.

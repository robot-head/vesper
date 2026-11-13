# Vesper Bridge wire layouts and sys-specs

This crate implements the byte-oriented layout/view milestone from design §13.2. It is independent of the compiler frontend so layout and memory-access contracts can be reviewed before adding `wire struct` syntax or CLIF lowering.

`Registry::new` accepts nominal `Declaration`/`Field` descriptors. `Registry::compile` rejects unsupported fields, invalid integer widths, duplicate names, missing/recursive struct definitions, implicit internal or tail padding, size overflow, and excessive descriptor depth/expansion. Successful `WireLayout` metadata has private fields and exposes checked offsets, sizes and alignment through read-only accessors.

`view_wire`, `split_wire`, and `split_wire_mut` perform bounds/alignment checks without allocating or copying the header. The mutable split returns disjoint header/payload borrows. Views provide checked nested projections, byte-array borrows and integer reads/writes. `initialize_wire` zeroes the header in place, including every `ZeroPad`, without touching payload bytes. Integer writes reject truncation and signedness changes before modifying memory. The crate denies unsafe Rust everywhere except the one gated `isolation::hw` module (below).

## Alignment contract

The design simultaneously asks for natural-alignment gap rejection and a packed, exactly 61-byte Kafka header, whose CRC starts at offset 17. This implementation distinguishes the two representations:

- Native-endian integers are naturally aligned (`u32` requires four-byte alignment). Any internal or tail alignment gap must be represented by explicit `ZeroPad[N]`.
- Explicit little/big-endian wire integers are byte-aligned wrappers. Their scalar operations assemble/disassemble bytes; no typed integer reference is created. This permits the Kafka header at any byte address while preserving the no-implicit-padding invariant.
- Nested wire structs inherit their validated layout alignment. Arrays and padding have alignment one.

`ZeroPad` is deterministically zero when constructed. Views of foreign initialized byte slices preserve padding, and semantic comparison ignores it. `validate_padding` is an explicit canonical-encoding check; it is separate from constant-time view construction. Callers requiring canonical zero padding must invoke it. A view never treats padding as a typed value or exposes a mutable padding-field accessor.

## Scope and integration

Supported scalars are signed/unsigned 8-, 16-, 32- and 64-bit integers, plus fixed byte arrays, explicit padding and nested nominal wire structs. Bool, sparse enums, pointers and unknown field kinds fail closed. Native-endian representation depends on the target; portable protocols should use explicit endian types. Native-endian accesses currently use host endianness and width-byte alignment; this is not a general C ABI layout engine. A cross-target frontend must supply/extend a target-layout contract before accepting host-independent native fields.

The separate [`sys` module](SYS-SPECS.md) now parses bounded `.vsys` contracts, records explicit C target/convention/link descriptors, checks canonical artifact identity and raw foreign-return refinements, and prepares owner-preserving numerical call arguments. Single-element `nocapture in|out|inout` pointer parameters may point to one integer or to a `wire struct` declared in the `.vsys` file; those declarations compile through `Registry`, so a pointee has zero implicit padding and total bit validity.

## Isolation domains (§13.4)

`isolation::IsolationDomain` allocates a protection key (`pkey_alloc`), maps page-aligned anonymous regions tagged with it (`pkey_mprotect`), and `with_revoked(Revoke::Write | Revoke::Access, f)` runs `f` with the current thread's rights to those regions removed: `WRPKRU` on x86_64 Linux (Intel/AMD PKU) and `MSR POR_EL0` on AArch64 Linux (Arm POE, `HWCAP2_POE`). The previous register value is restored on every exit, including unwinding panics. Region bytes are only copied through raw pointers (`read`/`write`), never exposed as Rust references. `isolation::contain` runs work in a forked child with a `SIGSEGV`/`SIGBUS` handler so a protection-key fault (`SEGV_PKUERR`) is reported as `ChildExit::Fault` with its address instead of killing the caller. Without PKU/POE (or on other targets) every constructor returns `Unavailable` with the reason. This is a same-address-space tripwire for stray writes, not a security boundary against code that can rewrite PKRU/POR_EL0 itself, and it does not pivot stacks or separate a foreign heap.

This milestone does not implement compiler source integration, HIR/PCG borrowing, CLIF generation, native foreign-call trampolines (and so no `handle Ffi[Lib] isolated(mpk)` lowering), or foreign-handle finalizers. Rust slice lifetimes protect this library's views; they are not evidence that the language's corresponding typing rules have been implemented.

The crate is registered in the main workspace; root workspace checks include its tests.

```sh
cargo test --manifest-path crates/vesper-ffi/Cargo.toml
cargo fmt --manifest-path crates/vesper-ffi/Cargo.toml --check
cargo clippy --manifest-path crates/vesper-ffi/Cargo.toml --all-targets -- -D warnings
```

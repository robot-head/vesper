# @vesper/codegen-freestanding

Freestanding images (`vesper build --target riscv64imac-unknown-none-elf` and
`aarch64-unknown-none`) in Vesper: self-hosting wave SH4, lane 14 (#1288).
Rust stays the default; nothing here replaces Rust code. Report:
[self-host-freestanding.md](../../docs/parallel-results/self-host-freestanding.md).

Part 1 ports everything that needs no Cranelift emission:

| File | Rust | Vesper |
|---|---|---|
| `src/stub.ves` | `src/freestanding/stub.rs` | the assembler (labels, fixups, literals), both stubs, `exit_stack_bytes` |
| `src/obj.ves` | the `object` crate reads the linker makes; `object::write::elf` for the stub | `parse_object`, relocation chains, `write_stub_object` (with `StringTable`'s suffix-sorted, tail-merged layout) |
| `src/link.ves` | `src/freestanding/link.rs`; `resolve_traps`, `patch_trap_table` of `src/native_freestanding.rs`; `checks::compare_code` | `Linker`, `compare_code` |
| `src/elf.ves` | `src/freestanding/elf.rs` | `write_executable`, `read_executable` |
| `src/script.ves` | `src/freestanding/script.rs` | `linker_script` |
| `src/error.ves` | `FreestandingError` | error codes and fields of a result |
| `src/lib.ves` | | the entries |

Each entry takes one request buffer (little-endian words and length-prefixed
strings) and returns one result buffer with a status word first; the Rust
half is `Request`, `Words` and `decode_error` in
`tests/self_host_freestanding.rs`.

Part 2 emits the runtime object (`src/freestanding/runtime.rs`) in Vesper
through `@vesper/cranelift`, in the sub-package `runtime/`
(`@vesper/codegen-freestanding-runtime`, entry `freestanding_runtime`). Its
differential is
`crates/vesper-codegen-swap-test/tests/self_host_freestanding_runtime.rs`
(the shim's crate graph keeps it out of the root package's tests).

Not yet ported: the floating-point check and Silicon's stack and interrupt
analysis (`checks.rs`, which read Cranelift `ir::Function`s), the entry's
compilation in `build_image` (the scalar and other family lanes), the
`freestanding` hook swap and the Windows object handling
(`src/native_windows.rs`).

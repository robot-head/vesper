# Wave seven, lane 27: freestanding RISC-V and AArch64 images (§26, §7)

Claim: issue #350. Branch `claude/project-thread-trt7k5`, based on
`283c8115d2203232d0e9b473cc4bb0312c3a8509` (origin/main at start, a
descendant of the required baseline `c025b9a`).

## What was built

`vesper::native::freestanding` compiles a checked entry function with the
existing Cranelift backend for a bare-metal ISA and links it, with no
allocator, no Pulse scheduler and no libc, into a static ELF executable that
boots on QEMU's `virt` machines and prints its result over the UART.

| Piece | File | What it does |
| --- | --- | --- |
| Targets and boards | `src/native_freestanding.rs` | `riscv64imac-unknown-none-elf` (machine mode) and `aarch64-unknown-none` (EL1); `Board` = RAM, load address, UART (NS16550A / PL011), exit device (SiFive test finisher / Arm semihosting / park). `build_image`, `link_with_external`, `qemu_command`. |
| Backend entry point | `src/native.rs` (own commit) | `object_for_isa`: object emission with a caller-built ISA; an entry whose only effects are `Fail` instances keeps its hidden failure-record parameter there (and only there). |
| Entry stub | `src/freestanding/stub.rs` | Hand-encoded machine code plus relocations, written as a relocatable ELF object (`.text.boot`). Masks local interrupts first (`csrci mstatus, MIE` / `msr daifset, #0xf`, immediates derived from Silicon's `irq_enable_bit`), parks secondary harts/cores, installs the trap vector (`mtvec` direct mode / a 16-entry `VBAR_EL1` table), enables FP/SIMD on AArch64 (`CPACR_EL1`), sets `sp` from `__stack_top`, zeroes `.bss`, calls the runtime; `vesper_board_exit(status)`. |
| Runtime | `src/freestanding/runtime.rs` | Generated as Cranelift IR: UART `putc`/`puts`/decimal/hex, `vesper_freestanding_main` (calls the entry, prints `vesper: main() = <value>`, exits 0) and `vesper_freestanding_trap(cause, epc, tval)`. |
| Aborts | runtime + trap table | Every trap site Cranelift emitted for the user code (backend aborts `user1`-`user4`, faulting loads/stores, division checks) and the runtime's unhandled-`Fail` aborts are listed in a read-only trap table (`count`, then `{pc, message, len}`). An entry `eff [Fail[E]]` whose failure escapes executes `trap user(16 + i)`; the exception enters the stub's vector, the handler finds the PC, prints `vesper: abort: unhandled Fail[RuntimeError] pc=0x… cause=0x… tval=0x…` and exits with status 3 (4 for an exception at no known site). |
| Linker | `src/freestanding/link.rs`, `elf.rs` | A static linker for exactly the relocations these objects use (`R_RISCV_64`, `R_RISCV_CALL(_PLT)`, `R_AARCH64_ABS64`, `R_AARCH64_CALL26`/`JUMP26`); anything else is `UnsupportedRelocation`. Undefined symbols are `UnresolvedSymbol` (nothing is left for a loader). Layout: `.text` (stub first) and `.rodata` in an R+X segment, then page-aligned `.data`, `.bss` and the stack in an R+W segment; the image must fit the board's RAM. Writes ELF64 `ET_EXEC` with a symbol table. |
| Linker script | `src/freestanding/script.rs` | The equivalent GNU ld / ld.lld script (`ENTRY(_start)`, `KEEP(*(.text.boot))`, `__bss_start`/`__bss_end`/`__stack_top`, RAM assertion). `link_with_external` links the same three objects with it. |
| Checks | `src/freestanding/checks.rs` | No floating point on riscv64imac; object code equals the code that was measured; Silicon's stack analysis; Silicon's interrupt-mask rules. |

### Frames checked with Silicon

Every user and runtime function is handed to
`vesper_silicon::stack::Program::compile_with_isa_flags` with the image's own
ISA (the ISA is built by Silicon's `target_isa_with`, so the flags are
identical: `opt_level=speed`, frame pointers kept, probestack off, non-PIC,
plus `has_zca=true, has_zcd=false` on RISC-V). Before that, each function is
compiled once more on its own and its bytes are compared with the bytes in
the object (`CodeMismatch` otherwise), and Silicon's measured code sizes are
compared with the same sizes, so the frames Silicon measures are the frames
in the image. Function names are rewritten into three namespaces (`u0:` user,
`u1:` runtime, `u2:0` the stub's exit), so one call graph spans the user
object and the runtime object. The policy has two roots, the runtime's main
and the trap handler (the stub gives the handler a fresh stack), each with the
image's stack size as budget; `vesper_board_exit` is an external with a
declared bound (16 bytes for the semihosting parameter block, 0 otherwise).
Any finding (`silicon.stack.recursion`, `budget_exceeded`,
`unbounded_external_call`, unresolved indirect calls) rejects the image.

Measured worst cases (this host, Cranelift 0.125.4): for
`tests/fixtures/freestanding/checked_sum.ves` main needs 272 bytes on both
targets (`vesper_freestanding_main` 32 → `main` 176 → `vesper_function_0` 64)
and the trap handler 96 (RISC-V) / 112 (AArch64) bytes; the default stack is
16 KiB.

### Interrupt-mask rules

The stub masks local interrupts before anything else and nothing unmasks
them, so the whole image is atomic context. `checks::interrupt_rules` runs
`vesper_silicon::atomic::check_atomic` with main and the trap handler as ISR
roots and the fiber-ABI park/Pulse symbols as suspending and the allocator
symbols as allocating, resolved through the backend's callee table. A unit
test builds a function calling `vesper_fiber_park` (→
`silicon.irq.suspend_in_isr`) and `malloc` (→ `silicon.irq.alloc_in_isr`).
From source this cannot fire today: the native backend rejects entries with
non-`Fail` effects before lowering and the linker rejects any undefined
symbol, so the rule is a second line, not the one that normally rejects.
The mask immediates themselves are derived from Silicon's model and checked by
decoding the first instruction of each image.

## Checks run

All on this host (Linux x86_64, Rust 1.97 stable and 1.100.0, QEMU 8.2.2
from Ubuntu `qemu-system-misc`/`qemu-system-arm`, LLD from the image):

- `VESPER_REQUIRE_QEMU=1 cargo test -p vesper --test freestanding`: 12 tests
  pass. Object and image tests run on every host; the QEMU and `ld.lld` tests
  skip with a message when the binary is absent.
  - `qemu_boots_match_the_evaluator`: ten programs (scalars, `i64::MIN`,
    bool, Unit, u8, inout, a tail-resumptive handler, enums, a handled
    division-by-zero `Fail`, the `checked_sum` fixture with records and a
    handled `ArithOverflow`) on both targets; the UART line equals
    `Runtime::run` on the evaluator and QEMU exits 0. RISC-V boots with
    `-cpu rv64,f=false,d=false,zfa=false -bios none`, so the core has no F/D
    and would trap on any floating-point instruction.
  - `qemu_abort_goes_through_the_trap_handler`: the `abort.ves` fixture
    (index 7 of a 3-tuple, `Fail[RuntimeError]` unhandled at the entry)
    prints the abort line with the PC of the runtime's abort site, RISC-V
    `mcause` 2 (illegal instruction) / AArch64 ESR EC 0, and exits 3.
  - `ld_lld_links_the_same_objects_with_the_script`: ld.lld places
    `_start`, the runtime, `main` and `__stack_top` at the same addresses as
    the built-in linker, and the lld image boots with the same output.
  - Rejections: hosted triples, entries with parameters, `f64` or aggregate
    results, non-`Fail` effects, a missing entry, floating point on
    riscv64imac, `f64 %` (`fmod`) on AArch64 (unbounded external call), a
    32-byte stack (`budget_exceeded`) and recursion (`recursion`).
- `cargo test -p vesper --lib freestanding`: the interrupt-rule unit test.
- `cargo fmt --all -- --check`; `cargo +1.100.0 clippy --workspace --locked
  --all-targets -- -D warnings`; `cargo test --workspace --locked`;
  `vesper fmt --check` on the two fixtures.

CI hosts (BuildBuddy Linux, macOS, Windows) have no QEMU, so there the boot
tests report that they were skipped; the object tests run everywhere.

## Root patch

In its own commit: root `Cargo.toml` gains `object = "=0.37.3"` (the version
already in `Cargo.lock`, features `read_core`, `write_core`, `elf`, `std`, as
`vesper-ebpf` uses it) and the `riscv64` feature on `cranelift-codegen`
(already enabled through `vesper-silicon`'s feature unification; now
explicit). `Cargo.lock` only adds `object` to the root package's dependency
list. No new crate.

## Decisions

- Cranelift's RISC-V backend refuses to run without the G extensions, so F
  and D are enabled for the code generator and riscv64imac is enforced on the
  IR (any float or vector value is `UnsupportedOperation`) and at run time
  (QEMU without F/D; `mstatus.FS` is also off at reset).
- `cranelift-object` 0.125 panics on `R_RISCV_PCREL_HI20` (colocated data on
  RISC-V), so the runtime marks its data references non-colocated (absolute
  literals). The backend's own object path is wrapped in `catch_unwind` and
  such a panic becomes a typed `Codegen` error; the native backend emits no
  data references today.
- A built-in linker rather than requiring `ld.lld`: CI hosts have no
  bare-metal linker, and the image must be built (and checked) everywhere.
  The script exists so external toolchains can link the same objects, and the
  gated test keeps the two in step.
- Trap-table PCs are filled after linking (the backend's functions are local
  symbols of its object); message pointers are ordinary relocations.
- An escaping `Fail` is the freestanding meaning of "abort". The narrow
  `native.rs` hunk accepts such entries only in `object_for_isa`; every hosted
  path still rejects them.

## Remaining gaps

- Only QEMU `virt` was booted; no hardware. No PMP/MPU stack guard: the stack
  bound is static only.
- Entry results are scalars (`i64`, `u64`, `u32`, `u8`, `bool`, `Unit`);
  parameters, aggregates and `f64` results are rejected. The failure payload
  is not printed, only which `Fail` instance escaped.
- No `vesper build --target` routing (CLI files belong to other lanes); the
  API is `vesper::native::freestanding::build_image`.
- No interrupts are ever unmasked, so no ISRs, IrqGuard source syntax or
  `@stack_limit` attribute; the stack budget is `ImageOptions::stack_bytes`.
- One core: secondary harts/cores park. No MMU (AArch64 runs with the MMU
  off, so every access is Device memory and must be aligned, which Cranelift's
  code is).
- DetFloat programs (NaN canonicalisation) are rejected on AArch64 because the
  analysed ISA flags do not include it.
- Rust ownership in this crate is not evidence that Vesper's source typing
  rules for bare metal are implemented; nothing here changes the checker.

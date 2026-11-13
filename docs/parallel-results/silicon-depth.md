# Wave six, lane 19: Silicon depth (§26)

Claim: issue #261. Branch `claude/project-thread-oosnze`, based on
`07322e3ba47890e939a48967a1ea8f62fdd7aac1` (origin/main at start; it
contains Silicon PR #149 as `1ff09ad` and the baseline `cda092f`).
Only `crates/vesper-silicon/**` and this report were edited; there is no root
patch (no new workspace member, no new dependency, `Cargo.lock` unchanged).
The earlier report is [silicon.md](silicon.md).

## What was built

| Item | Module | Evidence |
| --- | --- | --- |
| 1. riscv64 stack analysis | `prologue::measure_riscv64`, `stack` | `tests/stack.rs` (12 tests, now on three ISAs), 2 decoder unit tests |
| 2. Vendor SVD, read side effects, typestate accessor generation | `svd`, `hazard`, `mmio`, new `pac` | `tests/rp2040.rs` (6), `pac` unit tests (2), 1 new `compile_fail` doctest |
| 3. Devicetree input for memory and interrupt controllers | new `dtb` | `tests/dtb.rs` (9) |
| 4. Zicbom for non-coherent RISC-V DMA | `dma` | `tests/zicbom.rs` (7) |

### 1. riscv64 stack analysis

`vesper-silicon` now enables the `riscv64` feature of `cranelift-codegen`
0.125, and `Arch::Riscv64` is a supported analysis ISA (it was a typed
`UnsupportedIsa` rejection). `prologue::measure_riscv64` decodes the
prologue Cranelift emits:

```
addi sp,sp,-16 ; sd ra,8(sp) ; sd fp,0(sp) ; mv fp,sp     (frame record)
addi sp,sp,-N                                             (N < 2048)
lui t6,hi ; addi t6,t6,lo ; add sp,sp,t6                  (larger frames)
sd/fsd sX,off(sp) ...                                     (clobber saves)
```

in the 32-bit base encodings and in the compressed `C` forms
(`c.addi16sp`, `c.sdsp`, `c.fsdsp`, `c.mv`, `c.lui`, `c.li`, `c.addi`,
`c.add`), which Cranelift uses when `has_zca`/`has_zcd` are set. Constants
are tracked through `lui`/`addi`/`addiw` and their compressed forms into any
register, so `add sp,sp,rX` / `sub sp,sp,rX` are measured only when `rX`
holds a known value; an unknown value or an SP increment ends the prologue,
and the existing layout cross-check then rejects the function with
`FrameMeasurementMismatch` instead of guessing. `jal` pushes nothing, so
`call_push_bytes` is 0 and the 16-byte `ra`/`fp` record is
`frame_record_bytes`. `Program::compile_with_isa_flags` and
`stack::target_isa_with` take ISA-specific Cranelift settings (an unknown one
is `StackError::IsaSetup`).

Measured on this host from Cranelift 0.125.4 output (`opt_level=speed`,
probestack off, frame pointers kept): for `fn(i64)->i64` with a 32 / 256 / 64
/ 16-byte slot the riscv64 frames are 48 / 272 / 80 / 32 bytes; 3000- and
70 000-byte slots give 3024 and 70 016 (the `lui`+`addi`+`add` form).
On riscv64 every measured frame equals `16 + CompiledCode::frame_size`
exactly, for the base and compressed encodings alike (the compressed code is
shorter, the SP accounting identical). A caller passing 16 integer arguments
reserves at least 64 bytes more than one passing 4 on all three ISAs
(outgoing-argument area, which `frame_size` excludes). These are
measurements of Cranelift's output, not of a RISC-V core.

Cargo feature unification: enabling `riscv64` here enables it for the single
`cranelift-codegen 0.125.4` in every workspace build (Cargo and Bazel), so
the riscv64 backend is now compiled into the workspace. No other crate's
behaviour changes: `vesper-fiber-abi::emit_object` and the root native
`object` path reject non-x86_64/AArch64 targets before the Cranelift lookup,
and their "riscv64 is rejected" tests still pass. `Cargo.lock` is unchanged.

### 2. Vendor SVD, read side effects and generated accessors

Fixture: `fixtures/rp2040/RP2040.svd`, the unmodified Raspberry Pi RP2040 SVD
from pico-sdk (`079c6f39023649b154152db30f1d781e884879bc`, BSD-3-Clause,
SHA-256 `1c723301…d7b0a6`; provenance and licence in
`fixtures/rp2040/README.md`). It was chosen because it actually uses the
annotations the model checks: 302 `modifiedWriteValues=oneToClear` fields
and 47 `readAction=modify` fields. The whole 2.6 MB file parses with
`svd-parser` 0.14.10 (about 0.3 s in a debug build): 37 peripherals, 1181
registers at 1181 distinct addresses, 329 W1C fields after `derivedFrom`
expansion.

The model previously rejected `readAction=modify`. It now maps it to two new
`FieldAccess` kinds, `ReadSideEffect` (read-only: FIFO pops such as
`ADC.FIFO` and `SIO.FIFO_RD`, latching reads such as `TIMER.TIMELR`) and
`ReadSideEffectWrite` (writable: `UART0.UARTDR`, `SPI0.SSPDR`,
`SIO.SPINLOCKn`), and adds `RegisterClass::ConsumeWrite` with an
`mmio::ConsumeWrite` marker whose `Reg` has `consume` and `write` but no
plain `read` (a `compile_fail` doctest). The hazard rule for plain reads now
covers every consuming read (`readAction=clear` or `modify`). `readAction=
set`/`modifyExternal` stay `UnsupportedAccess`. The RP2040 file has no
`readAction=clear` field; the hand-written `vsim32.svd` still covers that.

Hazard tests on real registers: an RMW of `ADC.FCS` (read-write `EN` next to
W1C `OVER`/`UNDER`) is `silicon.mmio.rmw_of_write_clear_register`, in a trace
and in Cranelift IR at the real address; a plain read of `ADC.FIFO` or
`SIO.SPINLOCK0` is `silicon.mmio.read_to_clear_outside_consume`; listing the
function as a consumer accepts the FIFO pop.

`pac::generate_rust(&DeviceModel, &PacOptions)` renders deterministic Rust
text: per peripheral a module with `NAME`/`BASE_ADDRESS`, per register a
module with `PATH`, `ADDRESS`, `RESET` and `*_SHIFT`/`*_MASK` per field, and
a singleton struct of `Reg<Marker>` fields typed by register class whose
`take(&Bus)` binds every register (re-checking the class against the bus
model) and claims the singleton (`mmio::claim_peripheral`). Identifiers are
sanitised (keywords, leading digits, punctuation); collisions are
`PacError::NameCollision`. `fixtures/rp2040/pac.rs` is the generated module
set for ADC, SIO, TIMER and UART0; the test compiles it against this crate
(`include!`), drives it on a `SimBus` built from the RP2040 model
(acknowledging `OVER` with `write_clear_mask` leaves `UNDER` and `EN` set),
and checks that a fresh generation is byte-identical. Generating all 37
peripherals succeeds (1.2 MB of text; only the subset is compiled in tests).
The output is rustfmt-clean. `VESPER_REGEN_PAC=1 cargo test -p
vesper-silicon --test rp2040` rewrites the checked-in file. The `@svd_pac`
macro itself remains a later Forge step, as assigned.

### 3. Devicetree input

`dtb::DeviceTree::parse` reads version-17 DTBs. It is written in this crate:
the `fdt` crate (0.1.5, the most used parser) `expect`s and `assert`s on
malformed structure blocks, which violates the rule that untrusted bytes
never panic. Every header field, block bound and alignment, token, string
terminator, UTF-8 name and nesting level is checked, with budgets
(`MAX_DEPTH` 64, `MAX_NODES` 65 536, `MAX_RESERVATIONS` 1024); malformed
input is a typed `DtbError`. Helpers cover paths, phandles (duplicates are an
error), cell/string-list properties, `reg` with the parent's
`#address-cells`/`#size-cells` (1 or 2), `interrupt-parent` inheritance,
`interrupts` and `interrupts-extended`.

`dtb::Platform::from_tree` extracts:

- memory banks (`device_type = "memory"`, enabled, overlap-checked), the
  `/memreserve/` block and `/reserved-memory` children (with `no-map`);
  `usable_frame_ranges()` subtracts reservations widened to whole 4 KiB
  pages, and `frame_allocators(&Ledger)` builds one `FrameAllocator` per
  range (refusing more than `MAX_MODEL_FRAMES` = 16 GiB with
  `TooManyFrames`, so a hostile tree cannot make the model allocate without
  bound);
- interrupt controllers: RISC-V `riscv,cpu-intc` per hart, PLIC
  (`riscv,ndev`), GICv3 and GICv2; every enabled device's interrupts are
  resolved to the controller's number and range-checked (PLIC source
  `1..=ndev`, GIC SPI `n < 988` → INTID `32+n`, PPI `n < 16` → `16+n`,
  hart-local cause `< 64`); `install_isr(&mut Cpu, device, handler)` binds an
  ISR on the IrqGuard model to the device's platform interrupt and refuses
  devices without one;
- harts with `riscv,isa` and `riscv,cbom-block-size`, and
  `dma_coherence(device)` from `dma-coherent`/`dma-noncoherent` on the node
  or an ancestor (default coherent on RISC-V and non-coherent on Arm, as in
  Linux); non-coherent RISC-V needs Zicbom on every hart with one block size
  (`NoCacheBlockManagement` otherwise).

Fixtures (`fixtures/dtb/README.md`): the riscv64 `virt` and aarch64 `virt`
(GICv3) trees dumped by QEMU 8.2.2 (`-machine virt,dumpdtb=…`), and a
hand-written board (`vesper-rv-board.dts`, compiled with dtc 1.7.0) with two
banks, `/memreserve/`, a `no-map` and a mapped reservation (one starting
mid-page), a 32-source PLIC, a non-coherent and a coherent DMA master and a
disabled node with an out-of-range interrupt (ignored because disabled).

Tests: QEMU riscv `virt` gives 256 MiB at `0x8000_0000` (`(0x80000, 0x10000)`
frames), a PLIC with `ndev = 0x5f`, the UART on source 10, virtio-mmio on
1..=8, and the PLIC/CLINT outputs on hart causes 9/11 and 3/7; QEMU arm
`virt` gives the GICv3, PL011 SPI 1 → INTID 33, timer PPIs → 29/30/27/26,
coherent virtio. On the board, the usable ranges are exactly the expected
four, draining every allocator yields frames only inside banks and outside
every reservation, and the auditor balances after freeing them all; a UART
interrupt raised while thread code holds an ISR-shared lock under an
`IrqGuard` stays pending until the restore and then runs its ISR. Negative
tests: truncated header, bad magic, version 16, `totalsize` past the end, an
overflowing block size, a misaligned structure block, a PLIC source above
`ndev`, a dangling `interrupt-parent`, a hart without Zicbom, and a 1 TiB
bank. A robustness sweep parses every prefix of all three blobs and 9000
randomly corrupted copies, then runs the platform extraction on whatever
parses; none panic.

### 4. Zicbom DMA

`dma::DmaCoherence` (`Coherent`, `Zicbom { block_bytes }`,
`ArmNonCoherent`) and `DmaEngine::with_coherence` select the maintenance
sequence; `DmaEngine::new` keeps its earlier behaviour. New
`BarrierInst::{CboClean, CboFlush, CboInval}`. `zicbom_barriers` gives, as
Linux's RISC-V `arch_sync_dma_for_device`/`_for_cpu` do: before the device
touches the buffer, `CBO.CLEAN` per block then `FENCE IORW, IORW`; after a
device write, `FENCE IORW, IORW` then `CBO.INVAL` per block. A receive buffer
that does not start and end on block boundaries is refused at submit with
`PartialCacheBlock` and handed back (transmits from it are allowed: cleaning
a shared block is harmless). Coherent RISC-V and Arm engines emit ordering
barriers only.

`dma::NonCoherentCache` is a data-only model of a write-back cache in front
of memory that a bus master accesses directly; it executes `CBO.*` and
`DC CVAC/IVAC` (fences are no-ops in it). Tests show each operation is
necessary in the model: without the clean the device reads stale memory on
transmit, and on receive a dirty line evicted after the device write
overwrites the device's data; without the invalidate the CPU reads its stale
copy; invalidating a block shared with a CPU variable loses the CPU's update
(the case the engine refuses). A randomized test (64 seeds × 200 steps of
CPU writes, evictions, transmits and receives through a Zicbom engine)
checks after every transfer that the device read the CPU's view and the CPU
reads the device's data; with the invalidates removed from
`zicbom_barriers` it fails (checked by hand). The engine built from the
board tree's non-coherent DMA node maintains a 4 KiB frame from the tree's
frame allocator with 64 cleans + fence and fence + 64 invalidates.

## Checks

On this host (x86_64 Linux, rustc 1.97.0; Bazel CI uses Rust 1.100):

```sh
cargo fmt --all -- --check                                        # clean
cargo test -p vesper-silicon                                      # see below
cargo clippy -p vesper-silicon --locked --all-targets -- -D warnings  # clean
cargo test --workspace --locked                                   # see below
cargo clippy --workspace --locked --all-targets -- -D warnings    # see below
```

`vesper-silicon`: 12 unit tests, 9 atomic, 10 dtb, 2 lean (elaboration skipped
without `VESPER_LEAN`), 6 rp2040, 14 runtime, 12 stack, 4 svd, 8 zicbom, and
8 doctests (5 `compile_fail`), all passing.

Workspace: `cargo clippy --workspace --locked --all-targets -- -D warnings`
is clean. `cargo test --workspace --locked --no-fail-fast` ran 3356 tests:
3340 passed, 16 failed, all in `vesper-loom-build` (`tests/build.rs`,
`tests/cli.rs`) with `UnknownFingerprint { missing: ["engine_dependencies"] }`.
The same tests fail the same way on `origin/main` (`07322e3`) on this host,
so they are environmental and unrelated to this lane. CI's Bazel/Windows/macOS
runs are the reference.

Bazel was not run locally (no Bazel on this host); the crate's
`BUILD.bazel` is unchanged (`sync_build_files.py` produces no diff), and all
new fixtures live inside the crate directory, which `vesper_crate` already
globs.

Review follow-ups (Codex on the first push): implicit interrupt parents
(a device that is a child of its controller), subtrees under a disabled bus
skipped, versioned ISA extensions (`zicbom1p0`) accepted, writes to fully
read-only side-effect registers flagged, and coherent DMA builds its
ordering-only barrier without enumerating cache lines. Covered by
`fixtures/dtb/vesper-edge-cases.dts` and new tests.

## Decisions

- Own DTB parser instead of `fdt` 0.1.5, because `fdt` panics on malformed
  input; budgets make over-large trees typed errors.
- `readAction=modify` is modelled as a consuming read (two field kinds and a
  `ConsumeWrite` register class) rather than rejected, since real vendor
  files use it for FIFOs and spinlocks. The software bus logs such reads but
  does not model the FIFO or lock state behind them.
- Non-coherent receive buffers must cover whole cache blocks on the Zicbom
  path. The Arm path keeps its earlier behaviour (it still invalidates
  partial lines); tightening it is a separate change.
- Default DMA coherence per architecture follows Linux (`OF_DMA_DEFAULT_COHERENT`
  on RISC-V, non-coherent on arm64).
- No Lean changes: the existing `lean/SiliconFrames.lean` proofs are about
  W1C registers and frame conservation; nothing here changes those models.

## Remaining gaps

- No RISC-V hardware: frames are Cranelift's output measured on an x86_64
  host, Zicbom and the cache are software models (no real `cbo.*` executed),
  and `menvcfg.CBIE` is not modelled. The interrupt controllers are not
  emulated; DTB numbers are only fed to the `Cpu` model.
- Devicetree: no `interrupt-map` nexus (typed `UnsupportedInterruptNexus`),
  no `ranges` translation of device `reg`, cells above 2 rejected, dynamic
  `/reserved-memory` regions (size without `reg`) rejected, version-16 blobs
  rejected, and only `riscv,isa` (not `riscv,isa-extensions`) is consulted.
- The `@svd_pac` Forge macro and Vesper-text output are not written; the
  generated Rust targets the software `SimBus`.
- Rust ownership of the linear owners is host-library discipline, not an
  implementation of the Vesper source typing rules for `linear` values.

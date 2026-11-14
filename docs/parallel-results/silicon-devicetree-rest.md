# Wave seven, lane 18: Silicon devicetree rest (§26)

Claim: issue #341. Branch `claude/project-thread-rlvi4u`, based on
`283c811` (origin/main at start, a descendant of the required baseline
`c025b9a`). Only `crates/vesper-silicon/**` and this report were edited;
there is no root patch (no new crate, no new dependency, `Cargo.lock`
unchanged, `tools/bazel/sync_build_files.py` produces no diff). The
earlier reports are [silicon.md](silicon.md) and
[silicon-depth.md](silicon-depth.md).

## What was built

| Item | Module | Evidence |
| --- | --- | --- |
| 1a. `interrupt-map` nexus nodes | `dtb::DeviceTree::{interrupts, map_interrupt}`, `Platform::pci_intx` | `tests/devicetree_rest.rs` |
| 1b. `ranges` translation of device `reg` | `dtb::DeviceTree::{reg_translated, translate, pci_windows}`, `Platform::mmio` | same |
| 1c. cells above 2 | `dtb::DeviceTree::{bus_cells, reg_raw}` (up to 4, 128-bit) | same |
| 1d. dynamic `/reserved-memory` | `dtb::place_dynamic`, `ReservationSource::Dynamic` | same |
| 1e. version-16 blobs | `dtb::DeviceTree::parse` | same, and `tests/dtb.rs` |
| 1f. `riscv,isa-extensions` | `dtb::Hart::{isa_base, isa_extensions, has_extension}` | same |
| 2. `menvcfg.CBIE` in the Zicbom model | `dma::{ZicbomEnv, Cbie, PrivMode}`, `NonCoherentCache::set_zicbom_env`, `DmaEngine::set_zicbom_env` | `tests/zicbom.rs` (4 new) |
| 3. PLIC and CLINT/ACLINT emulation in the `Cpu` model | new `intc` (`RiscvIntc`, `Plic`, `IntcHandle`), `dtb::{PlicInfo, ClintInfo, HartTarget}` | `tests/intc.rs` (10) |

### Real devicetrees used (provenance in `fixtures/dtb/README.md`)

- QEMU 8.2.2 `virt` dumps: riscv64 (SiFive CLINT + PLIC + PCIe), riscv64
  with `aclint=on` (ACLINT MSWI/MTIMER/SSWI, new), aarch64 GICv3 (PCIe).
- SiFive HiFive Unleashed (FU540) and HiFive Unmatched (FU740), and
  StarFive VisionFive 2 v1.3B (JH7110) and VisionFive v1 (JH7100): the DTBs
  Ubuntu publishes in `linux-modules-6.8.0-60-generic_6.8.0-60.63.1_riscv64.deb`
  (ports.ubuntu.com), unmodified. They are built from Linux 6.8's
  `arch/riscv/boot/dts/{sifive,starfive}` sources.
- A version-16 copy of the Unmatched blob and of the hand-written board
  (`dtc -V 16`), and two new hand-written trees for cases no published
  tree has (a nexus chain behind a masked two-cell connector, nested
  non-identity `ranges`, a 3-cell non-PCI bus, dynamic reservations, a
  nexus loop).

### 1. Devicetree

**Nexus nodes.** A specifier whose parent has `interrupt-map` (and no
`interrupt-controller`, which wins as in Linux) is matched against the map:
the key is the child unit address (the first cells of the child's `reg`,
zero-padded; as many as the nexus's `#address-cells`, found on the nexus or
its nearest ancestor, default 2, the lookup Linux's `of_irq_parse_raw`
does) followed by the specifier, masked by `interrupt-map-mask` (default all
ones). Each entry is parsed with the parent's own `#address-cells` (absent =
0) and `#interrupt-cells`, so entries for different parents may have
different lengths. A match continues at the parent with the entry's parent
unit address and specifier until an interrupt controller is reached; more
than 16 hops is `NexusLoop`, no match is `NoInterruptMapEntry`, a map that
ends inside an entry is `BadProperty`, and `interrupt-map-pass-thru` is
still `UnsupportedInterruptNexus`. Every device interrupt in
`Platform::from_tree` now goes through this, and `Platform::pci_intx(host,
bus, dev, fn, pin)` resolves PCI INTx through a host bridge's map with the
PCI binding's unit-address encoding.

On real trees: QEMU riscv `virt` PCI slot s pin p reaches PLIC source
`0x20 + (s + p - 1) % 4` (the mask keeps two slot bits, so slot 4 = slot 0;
all 32 combinations checked); QEMU aarch64 `virt` reaches GIC SPI
`3 + (s + p - 1) % 4` = INTID 35.. with the GIC's 2-cell parent unit
address in each entry; the Unmatched's FU740 PCIe maps INTA..INTD to PLIC
57..60 for any device (mask `<0 0 0 7>`); the VisionFive 2's PCIe maps INTx
to the bridge's own `interrupt-controller` child (returned by
`map_interrupt`; `pci_intx` reports `NotAnInterruptController` because that
controller is not modelled). On the hand-written tree, connector line 1
with flags 8 reaches PLIC 31 (flags masked off) and line 2 goes through a
second nexus to PLIC 40.

**`ranges` and wide cells.** `#address-cells`/`#size-cells` up to 4 are
accepted (Linux's `OF_MAX_ADDR_CELLS`) and computed in 128 bits.
`reg_translated` walks every ancestor bus: an empty `ranges` is identity,
a missing one is `Untranslatable` (correct for `/cpus`, I2C and SPI
children: 2 to 13 such nodes per real tree, all the non-MMIO ones), and
otherwise the whole region must lie inside one entry (`NoRangesEntry`
otherwise; Linux checks only the start). PCI buses (`device_type =
"pci"`/`"pciex"` with 3 address cells) also match the `phys.hi` space code,
and `pci_windows` lists a host bridge's windows translated to CPU
addresses. `reg()` keeps its earlier meaning (untranslated, 64-bit) and
returns `AddressOverflow` for wider values. Memory banks, reservations and
the PLIC/CLINT windows now use translated addresses.

On real trees: the VisionFive 2's `usb@10100000` bus (`ranges = <0x0
0x10100000 0x100000>`, 1-cell child space) places the cdns3 controller's
three regions at `0x1010_0000`, `0x1011_0000`, `0x1012_0000`; the
Unmatched's PCIe windows are IO `0x6008_0000` (64 KiB), MEM32
`0x6009_0000` and `0x7000_0000`, prefetchable MEM64 `0x20_0000_0000` (128
GiB); QEMU riscv `virt`'s are IO at `0x300_0000`, MEM32 `0x4000_0000`,
MEM64 `0x4_0000_0000`. Hand-written: a two-level translation
(`0x100` → `0x1_0100` → `0x4_0001_0100`), a second window, an address
outside both windows, and a 3-cell non-PCI bus (`<1 0 0x10>` →
`0x5000_0010`).

**Dynamic `/reserved-memory`.** Children with `size` (and optionally
`alignment` and `alloc-ranges`) but no `reg` are placed after all static
regions, in tree order, as Linux's `__reserved_mem_alloc_size` does with
memblock's default top-down policy: in each `alloc-ranges` entry in turn,
or anywhere in memory, at the highest aligned address whose region lies in
one memory bank and outside everything reserved so far. They are
`ReservationSource::Dynamic` and are subtracted from the frame ranges like
the others. Model choices: `alignment` defaults to 4 KiB (Linux uses
`SMP_CACHE_BYTES`), and regions the tree does not mention (the kernel
image) are unknown, so a real kernel may place the region elsewhere. No
space is `ReservationDoesNotFit`. None of the published trees has a
dynamic region; the hand-written one places a 16 MiB, 4 MiB-aligned CMA
pool inside its 128 MiB `alloc-ranges` at `0x8700_0000` and a 1 MiB
buffer at the top of memory (`0x8ff0_0000`).

**Version 16.** Versions 16 and 17 (and later ones whose
`last_comp_version` is at most 17) are read; version 16 has a 36-byte
header without `size_dt_struct`, so its structure block is bounded by
`totalsize` and ends at `FDT_END`. `DeviceTree::version` records it. The
version-16 copies parse to node-for-node the same trees and frame ranges as
their version-17 originals; version 15 and `last_comp_version > version`
are `UnsupportedVersion`.

**`riscv,isa-extensions`.** `Hart` now carries `riscv,isa-base` and
`riscv,isa-extensions` (each entry checked to be a lowercase extension
name), and `Hart::has_extension` prefers the list over parsing `riscv,isa`,
as Linux does. All four published SiFive/StarFive trees use the new
binding (for example the JH7110 U74s list `zba` and `zbb`); the hand-written
tree has only the list (no `riscv,isa`) and names `zicbom`. The PLIC and
CLINT bindings' `0xffffffff` cause (an output present in hardware but not
for the OS, used for every M-mode context of the FU540 and FU740) used to
fail range checking; it is now kept as a hidden target and is not an
interrupt line.

The JH7100 (VisionFive v1) SoC bus is `dma-noncoherent` and its harts have
no Zicbom: `dma_coherence` is the typed `NoCacheBlockManagement`. Linux
maintains that SoC through the SiFive cache controller's flush registers,
which are not modelled.

### 2. `menvcfg.CBIE`

`ZicbomEnv { mode, menvcfg, senvcfg }` decides what each `CBO.*` does, as
the CMO specification (Zicbom 1.0.1) defines: M-mode is never gated;
`CBO.CLEAN`/`CBO.FLUSH` need `menvcfg.CBCFE` in S-mode and both `CBCFE`
bits in U-mode; `CBO.INVAL` follows `CBIE` (`00` illegal instruction, `01`
executed as `CBO.FLUSH`, `11` as written, `10` reserved, a typed
`ReservedCbie`), and in U-mode the weaker of the two registers wins. The
hypervisor's VS/VU modes are not modelled. The cache model executes
instructions through it (a trapping one is `CboIllegal` and changes
nothing), and the DMA engine refuses a submission whose clean or later
invalidate would trap, handing the buffer back; the engine's environment
cannot change while a transfer is outstanding (`TransfersInFlight`),
because a receive's completion sequence was validated at submit.

Tests: the full truth table; with `CBIE = 01` the receive protocol still
delivers the device's data because the before-device clean leaves the lines
clean, and a 32-seed randomized run of the engine and cache with an S-mode
driver at `CBIE = 01` stays consistent; a CPU store into the buffer during
the transfer (what the linear `DmaTransfer` forbids) is discarded under
`CBIE = 11` but overwrites the device's data under `CBIE = 01`; an S-mode
engine without `CBCFE` refuses transmits, and with `CBIE = 00` refuses
receives up front.

### 3. PLIC and CLINT/ACLINT emulation

`Platform` now lists `plics` (base, size, `ndev`, and per context the hart
and cause from `interrupts-extended`, hidden contexts included) and
`clints` (SiFive CLINT, ACLINT MSWI, SSWI and MTIMER with their windows and
per-index hart targets). `intc::RiscvIntc::new(&platform, privilege,
&ledger)` builds one IrqGuard `Cpu` per enabled hart plus register-level
models of those blocks:

- PLIC (specification 1.0.0 map): priority, pending, per-context enable,
  threshold and claim/complete registers; level-triggered gateways (a
  source is forwarded while its line is high and it is not in service; a
  completion of an enabled, in-service source re-forwards it if the line is
  still high; a completion for a context that has not enabled the source is
  ignored, as the specification says); the threshold gates the context's
  notification only, and a claim returns the highest-priority pending
  enabled source regardless of it (so a driver can poll with threshold 7),
  ties to the lowest id, priority 0 never; 3-bit priorities; source 0
  hard-wired; read-only pending (`ReadOnly`), unmapped and misaligned
  accesses are typed errors.
- CLINT/ACLINT: MSIP / SETSSIP / MTIMECMP / MTIME at the binding's offsets,
  with 32-bit halves; MTIP is the level `mtime >= mtimecmp` against the
  MTIME of the hart's own block (one counter per distinct MTIME address, as
  the ACLINT specification allows several).
- Delivery: a cause is enabled on a hart once a handler is installed; after
  every operation the levels are re-evaluated and a rising one is raised on
  the hart's `Cpu`, where `mstatus.MIE` (the `IrqGuard`) decides whether it
  runs now or stays pending. External delivery claims, runs the source's
  ISR (which gets the `IsrCtx` for `IsrSpinLock::lock_from_isr`, the hart,
  the source and an `IntcHandle` to acknowledge its device), completes, and
  repeats until the claim returns 0. A level still high after 64 handler
  runs is counted as a storm; a delivery that finds nothing is counted as
  spurious. M-mode software gets MEIP/MSIP/MTIP, S-mode software SEIP and
  the ACLINT SSWI's SSIP; the S-mode timer (SBI or Sstc) is not described
  by a devicetree and is refused with `TimerNeedsMachineMode`.

Tests on the real trees: QEMU riscv `virt` UART (source 10) through context
0, delivered immediately or held under an `IrqGuard` and delivered on
restore; priority order and the lowest-id tie-break; a threshold of 3
holding a priority-1 source pending until lowered; an ISR sharing an
`IsrSpinLock` counter with thread code; a storm from an ISR that never
quiets its device; one claim winning when two harts enable a source (the
other's claim is spurious); a manual claim/complete sequence including an
ignored completion; a periodic CLINT timer (including two periods elapsing
while masked, which runs the handler until MTIMECMP passes MTIME) and an
MSIP inter-processor interrupt; on the ACLINT tree, MTIMER/MSWI addresses
and an S-mode SSWI interrupt; on the Unleashed, the S-mode contexts 2, 4,
6, 8 of harts 1 to 4 (the M-mode ones are hidden) and the UART on source 4
delivered to hart 3; on the VisionFive 2, the UART (source 32) in M-mode on
hart 2 and the CLINT layout. A random register/line/time sweep over four
trees and both privileges never panics; two MTIMER blocks with separate
MTIME registers keep separate counters.

Review follow-ups (Codex on the first push): claims ignore the threshold,
MTIME is per register, and the DMA engine's Zicbom environment is pinned
while transfers are outstanding.

## Checks

On this host (x86_64 Linux; rustc 1.97.0 and 1.100.0):

```sh
cargo fmt --all -- --check                                                    # clean
cargo test -p vesper-silicon                                                  # all pass, see below
cargo +1.100.0 clippy -p vesper-silicon --locked --all-targets -- -D warnings # clean
cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings       # see below
cargo test --workspace --locked --no-fail-fast                                # see below
python3 tools/bazel/sync_build_files.py                                       # no diff
```

`vesper-silicon` integration tests: atomic 9, devicetree_rest 10 (new), dtb
10, intc 10 (new), lean 2 (elaboration skipped without `VESPER_LEAN`), rp2040
6, runtime 14, stack 12, svd 4, zicbom 12 (4 new), plus the unit and doc
tests, all passing.

Workspace: `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D
warnings` is clean (exit 0; Cargo prints an unrelated unused-dependency
manifest warning for `vesper-pact`). `cargo test --workspace --locked
--no-fail-fast` (rustc 1.97.0) ran 4038 tests: 4018 passed and 20 failed,
none in `vesper-silicon` and none in a file this lane touches. 16 are the
`vesper-loom-build` tests that fail with `UnknownFingerprint { missing:
["engine_dependencies"] }` on this host (the wave-six silicon report saw
the same failures on `origin/main`). The other 4 are root §42
oracle/replay acceptance tests (`lean4lean42_acceptance`,
`oracle42_acceptance`, `oracle42_runner`, `replay42_kernel`) that need a
local Lean toolchain and the §42 setup, which this host lacks; §42 belongs
to another agent. The reference is CI on Bazel, Windows and macOS.

Bazel was not run locally. All new fixtures live inside the crate
directory, which `vesper_crate` already globs, so `BUILD.bazel` is
unchanged.

## Decisions

- Linux's rules where the devicetree specification leaves room: a node
  with `interrupt-controller` is a controller even with an `interrupt-map`;
  the nexus unit-address width is looked up on ancestors (default 2) for the
  first hop and the parent's `#address-cells` (default 0) afterwards; a
  missing `ranges` is untranslatable (the Apple identity quirk is
  PowerPC-only in Linux). Stricter than Linux: a translated region must lie
  wholly inside one `ranges` entry.
- `reg()` keeps its 64-bit, untranslated meaning so existing callers do not
  change; translation is a separate call.
- Dynamic reservations use memblock's top-down placement with a 4 KiB
  default alignment, documented as a model of where Linux would put them,
  not a guarantee.
- The PLIC model implements 3 priority bits (as QEMU's `virt`); real
  implementations may have more, which a devicetree does not state.
- One handler per PLIC source; a cause is enabled in the model's `mie` by
  installing a handler, rather than modelling `mie` writes separately.
- No Lean changes: `lean/SiliconFrames.lean` proves the W1C and frame
  conservation models, which this lane does not change.

## Remaining gaps

- No RISC-V hardware: the PLIC, CLINT/ACLINT, cache and `envcfg` are
  software models validated against their specifications and the published
  trees, not against silicon, and no `cbo.*` was executed on a core.
- Not modelled: `interrupt-map-pass-thru`, more than one PLIC, APLIC/IMSIC
  (AIA), MSI and edge-triggered sources, the S-mode timer, VS/VU modes for
  `CBIE`, the SiFive cache controller flush used by the JH7100, and
  `dma-ranges`.
- Devicetree interrupt-controller children of PCIe bridges (VisionFive 2)
  resolve but are not emulated.
- Rust ownership of the linear owners is host-library discipline, not an
  implementation of the Vesper source typing rules for `linear` values.

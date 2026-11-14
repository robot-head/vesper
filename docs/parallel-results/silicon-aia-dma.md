# Silicon AIA, PLIC edges, dma-ranges and the JH7100 cache flush (wave eight, lane 26)

Claim issue: #475. Base: `origin/main` at `1094e64`. Paths edited:
`crates/vesper-silicon/**` and this report. `lean/SiliconFrames.lean` is
unchanged. No root patch and no new dependencies.

## What was built

### RISC-V AIA (`src/aia.rs`, `src/dtb.rs`, `src/intc.rs`)

- **APLIC domains** (`AplicDomain`), register-exact to AIA 1.0 §4:
  - `domaincfg` (IE, DM; DM read-only when only one delivery mode exists).
  - `sourcecfg` with delegation (D + child index) and the source modes
    inactive, detached, edge rising/falling and level high/low.
  - `setip`/`setipnum`/`in_clrip`/`clripnum`, `setie`/`setienum`/`clrie`/
    `clrienum`, and `setipnum_le`/`_be`.
  - `genmsi` and `target[i]`.
  - Root-domain-only `mmsiaddrcfg`/`smsiaddrcfg` with the lock bit.
  - IDCs (`idelivery`, `iforce`, `ithreshold`, `topi`, `claimi`).
  - The §4.7 pending rules for each mode, including the rectified input of
    a detached source (always 0) and the fact that `setipnum` on a level
    source in MSI mode needs the input high.
  - MSI addresses use the §4.9.1 formula. `MsiAddrCfg::hart_of` inverts it.
- **IMSIC interrupt files** (`ImsicFile`):
  - `seteipnum_le`/`_be`.
  - The indirect registers `eidelivery`, `eithreshold`, `eip*` and `eie*`.
    On RV64 only the even-numbered `eip`/`eie` exist; an odd number is a
    typed `BadIselect`.
  - `*topei` read and claim, where the lowest identity wins.
- **Devicetree** (`ControllerKind::Aplic` and `ControllerKind::Imsic`):
  - Compatibles `riscv,aplic`/`qemu,aplic` and `riscv,imsics`/`qemu,imsics`.
  - `riscv,children` and `riscv,delegation` (also the older
    `riscv,delegate`), and `msi-parent`.
  - IMSIC `riscv,guest-index-bits`, `riscv,hart-index-bits` (defaulting to
    ceil(log2 harts), as in Linux `imsic_parse_fwnode`),
    `riscv,group-index-bits` and `riscv,group-index-shift`.
  - Several `reg` windows per IMSIC (one per socket), with
    `ImsicInfo::file_of(hart, guest)`.
  - `Platform::trigger` gives the trigger type of a two-cell specifier.
    `Platform::msi_parent` gives a device's MSI parent.
- **Emulation in `RiscvIntc`**:
  - It emulates the M-mode firmware: it programs and locks
    `mmsiaddrcfg`/`smsiaddrcfg` from the IMSIC nodes and, for a
    supervisor-level platform, applies the DT delegations as OpenSBI does.
  - MSIs written by the APLIC (or by a device through `msi_write`) land in
    the IMSIC file their address decodes to, including guest files. An MSI
    to an address that no file decodes is counted in `lost_msis`.
  - `claim_external` claims from the IMSIC first, then the APLIC IDC, then
    the PLIC. `complete_external` re-triggers a level source that is still
    high in MSI mode by writing `setipnum`, as AIA §4.9.2 and Linux
    `aplic_msi_irq_eoi` do.
  - `install_isr` on an APLIC line routes it, and so does
    `set_controller_line`. In MSI mode an identity is allocated from a band
    that matches the priority. In direct mode the line is programmed with
    IPRIO = 8 − priority.

### PLIC (`src/intc.rs`, `src/dtb.rs`)

- Any number of PLICs, addressed as `Ctl::Plic(k)`. QEMU's two-socket
  `virt` has one per socket. The `MultiplePlics` error is kept for API
  compatibility but is no longer returned.
- Edge-triggered sources on the two-cell PLIC bindings: T-Head C900
  (`thead,c900-plic`, as used by the Allwinner D1) and Andes NCEPLIC100.
  Flag 1 means rising edge and flag 4 means level high. While a source is
  in service, the gateway latches one further edge, and it forwards that
  edge on completion.

### `interrupt-map-pass-thru` (`src/dtb.rs`)

`map_one` copies the child specifier's pass-thru bits into the parent
specifier, as Linux `of_irq_parse_raw` does. The
`UnsupportedInterruptNexus` variant is kept but is no longer returned for
pass-thru.

### `dma-ranges` (`src/dtb.rs`, `src/dma.rs`)

- `DeviceTree::dma_windows` composes every bus level between a device and
  the root by intersecting them. A missing `dma-ranges` is identity, as is
  an empty one at its level.
- `Platform::dma_address`/`cpu_address` translate between CPU and bus
  addresses, and `DtbError::NotDmaAddressable` reports an address outside
  the windows.
- `DmaEngine::set_dma_windows` is refused while transfers are in flight.
  Descriptors carry the bus address. A buffer outside the windows is handed
  back with `DmaError::NotDmaAddressable`.

### SiFive cache controller (`src/ccache.rs`, `src/dma.rs`)

- `SifiveCcache` models Config (0x000), WayEnable (0x008), FLUSH64 (0x200)
  and FLUSH32 (0x240, address >> 4).
- `DmaCoherence::SifiveCcache { flush64, line_bytes }` is chosen only for
  `starfive,jh7100-ccache`, and only when the harts lack Zicbom. This
  matches Linux, where `ccache_flush_range` is installed as the non-standard
  cache ops only for the JH7100.
- `ccache_barriers` emits Linux's sequence: a fence, then one
  `sd line, FLUSH64` per 64-byte line, then a fence
  (`BarrierInst::CcacheFlush64`). A device write that would leave a partial
  line returns `PartialCacheBlock`.
- The VisionFive v1 tree now resolves to this sequence instead of
  `NoCacheBlockManagement`.

## Real devicetrees and QEMU evidence

- QEMU 8.2.2 dumps (commands and SHA-256 in `fixtures/dtb/README.md`):
  - `virt,aia=aplic`
  - `virt,aia=aplic-imsic`
  - `virt,aia=aplic-imsic,aia-guests=2` with `-smp 4`
  - two-socket NUMA `virt` without AIA and with `aia=aplic-imsic`
- The published Allwinner D1 Nezha tree (C906 two-cell PLIC), from the same
  Ubuntu linux-modules 6.8.0-60 package as the earlier SiFive and StarFive
  boards.
- One hand-written tree (`vesper-aia-edge.dts`) covers two PLICs,
  pass-thru, nested `dma-ranges` and a non-JH7100 ccache.
- **Live QEMU cross-check** (`tests/aia_qemu.rs`): `fixtures/qemu/aia-probe.S`
  is a bare-metal M-mode op interpreter. It accepts MMIO reads and writes,
  `mireg`/`sireg`, `*topei`, claims and `mip`, and prints its results on the
  UART. Harts other than 0 park in `wfi`. Each scenario runs the same op
  list on the model and on `qemu-system-riscv64 -machine virt,aia=... -smp 2
  -bios none`:
  - MSI mode
  - direct mode
  - supervisor level, where the S domain is delegated by M, MSIs go to the
    S file and `stopei` is claimed

  The scenarios drive the real `virt` UART (source 10) through its THRE
  interrupt.

  The model must equal both the recorded QEMU 8.2.2 output and the live
  output when QEMU is on `PATH`. It ran live here and all four tests
  passed.
- **QEMU 8.2.2 departures from the specification.** The model follows the
  spec, and `qemu_8_2_departures_from_the_specification` pins QEMU's actual
  value next to the model's for each case. On a QEMU other than 8.2 the
  test prints QEMU's value instead of asserting it.
  1. In MSI mode, `setipnum` on a level source whose input is low still
     forwards.
  2. A direct-mode level source stays pending after its input drops.
  3. A `claimi` that clears `iforce` leaves MEIP set.
  4. An undelegated child domain's `sourcecfg` keeps its written value
     instead of reading 0.
  5. `in_clrip` of a detached source shows the raw wire.
  6. Reserved SM=2 is stored as written. The model stores it as inactive.

No real AIA hardware, D1 board or JH7100 board was available. Everything
above was exercised only on the models and QEMU.

## Checks

Toolchains: the local Rust is 1.97, and the clippy run used 1.100.0 to
match Bazel CI.

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo +1.100.0 clippy -p vesper-silicon --locked --all-targets -- -D warnings` | clean |
| `cargo test -p vesper-silicon --locked` | 136 passed, 0 failed (includes the 4 live-QEMU tests and the Lean test) |
| `env PATH=/nonexistent .../aia_qemu-*` | 4 passed (recorded output only) |
| `cargo test -p vesper-silicon --test aia_qemu` with QEMU 8.2.2 on `PATH` | 4 passed (live) |
| `python3 tools/bazel/sync_build_files.py` | no diff |
| `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings` | clean |
| `cargo test --workspace --locked --no-fail-fast` | 4486 passed, 22 failed in 8 targets outside this lane (see below) |

The 22 workspace failures are in `vesper` (`lean4lean42_acceptance`,
`oracle42_acceptance`, `oracle42_runner`, `proof_audit_private`,
`replay42_kernel`, `resource_model42`) and in `vesper-loom-build` (`build`,
`cli`). They need the §42 Lean replay setup or the Loom build fingerprint,
which this sandbox does not have. None of them depends on
`vesper-silicon`, and every `vesper-silicon` target passed.

## Decisions

- **The spec over QEMU** wherever they differ. Each departure is pinned in a
  test.
- **Priority mapping:**
  - Direct mode uses IPRIO = 8 − priority, so a higher IrqGuard priority
    gets a lower (more urgent) IPRIO, which matches the APLIC ordering.
  - MSI mode allocates identities in priority bands, so `*topei` (where the
    lowest identity wins) agrees with the IrqGuard priority.
- **Firmware emulation.** `RiscvIntc::new` does the M-mode setup that
  OpenSBI does: it programs and locks the MSI address configuration, and on
  a supervisor-level platform it applies the DT delegations. Supervisor
  code therefore sees what Linux sees.
- **Defaults.** A missing `dma-ranges` is identity, as in Linux. The SiFive
  ccache is used for DMA only on the JH7100, also as in Linux.
- **Typed errors on untrusted input.** Malformed AIA nodes, out-of-range
  delegations, bad `iselect`, unmapped or misaligned MMIO and random
  register traffic all return typed errors. A randomized test covers the
  random traffic. There is no `unsafe` code.

## Public API

- Every change is an addition: new modules (`aia`, `ccache`), new types,
  methods and functions, and new `Platform` fields.
- Code that matches the extended enums exhaustively or builds `Platform`
  with a struct literal will need updating. The extended enums are
  `DmaCoherence`, `BarrierInst`, `ControllerKind`, `DtbError`, `DmaError`
  and `IntcError`, which gained variants.
- Lane 27 (PR #373) uses only `stack`, `atomic`, `irq` and `Arch`, and none
  of them changed.

## Remaining gaps

- No Lean changes. `SiliconFrames.lean` does not model AIA or `dma-ranges`.
- No hypervisor-level delivery. Guest interrupt files receive MSIs and
  their `eip` can be read, but `hgei`/`vstopei` and VS-level claims are not
  modelled.
- No PCI `msi-map`/`msi-parent` with requester IDs. Device MSIs use
  `msi_write` directly.
- The IMSIC `eidelivery` value 0x40000000 (delivery from a PLIC/APLIC
  through the IMSIC) is not modelled.
- No real hardware; see the evidence section.
- The linear owners are Rust ownership discipline, not the Vesper typing
  rules.

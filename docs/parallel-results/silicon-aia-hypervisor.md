# Silicon: Lean AIA and dma-ranges model, guest interrupt files, PCI MSI maps, `eidelivery` 0x40000000 (wave nine, lane 30)

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../manuscript/README.md) is the sole specification; use the
> [section map](../section-map.md) for earlier numbering. Confirm live ownership before acting.

Claim issue: #607. Base: `origin/main` at `5a018bd` (a descendant of the
required `37f7b8b`). Paths edited: `crates/vesper-silicon/**`, the new
`lean/SiliconAia.lean` and this report. `lean/SiliconFrames.lean` is
unchanged. No root patch and no new dependencies. Before starting, the open
issues and pull requests were searched for overlapping AIA, hypervisor or
Silicon work; only the claim issue matched.

## What was built

### Guest interrupt files and the hypervisor CSRs (`src/intc.rs`)

- Each S-level IMSIC file is followed by guest files `1..=GEILEN`.
  - GEILEN is `2^riscv,guest-index-bits - 1`, capped at 63.
  - `RiscvIntc::limit_guest_files(n)` implements fewer files, because
    GEILEN belongs to the hart and is not in the devicetree. Linux probes it
    by writing all ones to `hgeie`, and QEMU's `aia-guests=N` gives N files
    inside a `2^guest-index-bits` page stride.
  - An MSI to a page past the last implemented file is lost.
- Per hart, `hstatus.VGEIN` is a 6-bit field stored as written. It is
  checked when it is used.
- `hgeie` has writable bits `GEILEN:1`.
- `hgeip` is read-only: bit `g` is guest file `g`'s output, that is,
  `eidelivery` = 1 and an enabled, pending identity below the threshold.
- The two H-extension bits of `mip`:
  - SGEIP (12) is `hgeip & hgeie != 0`.
  - VSEIP (10) is `hgeip[VGEIN]`, and `hgeie` does not gate it.
  - `hyp_pending` reads both.
- `vs_imsic_csr_read`/`_write` (`vsiselect`/`vsireg`), `vstopei` and
  `claim_vstopei` reach the guest file VGEIN names. When VGEIN is 0 or
  above GEILEN, they return `IntcError::NoGuestFile`, which stands for the
  illegal-instruction trap of AIA §2.3/§3.9.
- **VS-level claims.** `RiscvIntc::install_guest_msi(hart, guest, handler)`
  is for HS-mode software only; it returns `GuestNeedsSupervisor` otherwise.
  - It allocates the lowest free identity in the guest file, enables it and
    the file, sets the guest's `hgeie` bit, and returns the address/data
    pair.
  - On SGEIP the modelled hypervisor serves the guests with live bits,
    lowest first. For each one it switches VGEIN to that guest, claims
    through `vstopei`, runs the handler with cause VSEIP, and restores
    VGEIN.
  - `guest_deliveries()` logs each claim.
  - A masked hart holds SGEIP pending (the `IrqGuard` `Cpu`), as for the
    other causes.

### `eidelivery` = 0x40000000 (`src/aia.rs`, `src/intc.rs`)

- `ImsicFile::with_external_delivery` makes a file whose WARL `eidelivery`
  also takes 0x40000000 and resets to it (§3.8.1). Every other file keeps
  bit 0 of a write, as QEMU 8.2.2's files do. That includes every guest
  file, which the specification forbids from taking the value.
- `RiscvIntc::new` gives the option to exactly those M/S files whose hart
  and level a PLIC context or a direct APLIC IDC also reaches.
- **The hart's external level at that level:**
  - With `eidelivery` = 1 it is the file's output only.
  - With 0x40000000 it is the PLIC/APLIC output. The file then behaves as
    though `eidelivery` were 0, and `*topei` is unaffected.
  - With 0 it is nothing.
- The claim loop follows the same rule. It claims from the IMSIC only when
  the file delivers, and it falls through to `claimi`/PLIC claims only
  under 0x40000000.
- **Routing:**
  - Routing a wired source (`install_isr` on a PLIC, or on a direct APLIC
    domain) selects 0x40000000.
  - That is refused with `IntcError::ImsicOwnsLevel` while the file is
    delivering its own MSIs.
  - Routing an MSI selects 1, after which the PLIC no longer reaches the
    hart. A test shows this.

### PCI `msi-map` / `msi-parent` with requester IDs (`src/dtb.rs`, `src/intc.rs`)

- `requester_id(bus, device, function)` gives `bus << 8 | device << 3 | fn`.
- `Platform::msi_map(node)` parses `msi-map` (fixed 4-cell entries, as
  Linux's `of_map_id` reads them) and `msi-map-mask` (all ones by default).
  - Each of these is a typed `BadProperty`: an empty property, a length
    that is not a multiple of 4, a zero or overflowing entry, and a base the
    mask ignores.
  - An unknown phandle is `UnknownPhandle`.
- `Platform::pci_msi_route(host, rid)` follows Linux `of_msi_map_id`.
  - The nearest node with `msi-map` decides: the host, then its ancestors.
  - The first entry whose range holds `rid & mask` gives the controller and
    the device ID `msi_base + (masked - rid_base)`.
  - No matching entry is `DtbError::NoMsiMapEntry`, meaning no MSI domain.
  - Without `msi-map`, the nearest `msi-parent` takes the requester ID
    unchanged.
  - With neither, the result is `NoMsiController`.
- `Platform::pci_dma_windows`/`pci_cpu_address` and
  `DeviceTree::bus_dma_windows` apply a host bridge's own `dma-ranges`, and
  its ancestors', to the functions below it, which the tree does not list.
- `RiscvIntc::pci_msi_write(platform, host, rid, bus_addr, data)` handles a
  write from a PCI function.
  - It routes the requester ID and translates the bus address through
    `dma-ranges`.
  - It delivers the write wherever the CPU address decodes. With no IOMMU
    this is what QEMU `virt` does too.
  - It returns a `PciMsi` record: requester and device ID, routed
    controller, bus and CPU address, data, the IMSIC, hart and guest it
    landed in, and `in_parent` (whether that file belongs to the routed
    controller).
  - A write that decodes to no interrupt file is refused (`Unmapped`) and
    counted in `lost_msis`.

### APLIC delivery through the IMSIC, including to guests

S-level APLIC targets carry a guest index (bits 17:12). The model already
encoded it; it is now checked end to end. `smsiaddrcfgh.LHXS` gives the
guest stride, and an APLIC source lands in a guest file, raises `hgeip` and
is claimed through `vstopei`.

### Lean model (`lean/SiliconAia.lean`, Lean 4.35.0)

Every theorem is about the Lean definitions in this file. None is about the
Rust code, QEMU or hardware.

- **IMSIC file.** The model has `eidelivery`, the threshold, and `eip`/`eie`
  as `Nat → Bool`.
  - `top_spec`/`top_none`: `*topei` is the least eligible identity.
  - `claim_clears_top`/`claim_keeps_others`: a claim clears exactly that
    identity's pending bit.
  - `msi_makes_pending`/`msi_asserts`: an MSI of an enabled identity below
    the threshold makes a delivering file assert.
  - `external_file_is_silent`, `topei_ignores_delivery`, `warl_values` and
    `warl_guest_never_external`: the 0x40000000 rule.
- **Hart external level** (`hartLevel`). `level_file_on`, `level_external`
  and `level_off`.
- **APLIC MSI addresses** (`msiPage`, §4.9.1).
  - `msiPage_injective`: different (group, hart, guest) triples within the
    field widths give different pages.
  - `qemu_s_level_drops_hart`: shows the collision that QEMU 8.2.2's
    reading of LHXW produces (departure 7 below).
- **APLIC forwarding in MSI mode** (`forward`). `forward_pending`,
  `forward_count` and `forward_targets`: one MSI per pending, enabled
  source, carrying its target, which ends not pending; others keep their
  bit.
- **Hypervisor CSRs** (`Hyp`). `hgeip_iff`, `vseip_iff`, `hgeie_bit0`,
  `sgeip_needs_hgeie`, `vs_access_iff`, `vsclaim_isolated` and
  `vsclaim_own_file`. A VS-level claim changes only the guest file VGEIN
  selects: never the hart's own file, never another guest's.
- **`dma-ranges`.**
  - `toCpu_sound`, `toCpu_none` and `roundtrip`.
  - `compose_sound`: a window composed from two bus levels by intersection,
    as `DeviceTree::dma_windows` does, translates the same as the two
    levels one after the other.
- **`msi-map`** (`mapId`, as `of_map_id` matches). `mapId_sound`,
  `mapId_none` and `mapId_mask_idem`.
- **A concrete trace.** `hypTrace` runs the 61-op hypervisor script of
  `tests/aia_qemu.rs` on the Lean model, for hart 0 with GEILEN 2 and 255
  identities.
  - `hypTrace_matches_qemu` proves, by kernel evaluation
    (`decide +kernel`), that it prints the 43 values QEMU 8.2.2 printed.
  - `tests/lean.rs` checks that the Lean file's `qemuHyp` list is the same
    `HYPERVISOR_EXPECTED` constant (`tests/common/aia_hyp_expected.rs`)
    that the Rust model and live QEMU are compared against.
- There is no `sorry`, `admit`, `axiom`, `native_decide` or
  `implemented_by`. `#print axioms` shows only `propext`,
  `Classical.choice` and `Quot.sound`.
- `tests/lean.rs` scans both Silicon Lean files and elaborates them when
  `VESPER_LEAN` is set.

## QEMU evidence

All live runs used QEMU 8.2.2 (Ubuntu `1:8.2.2+ds-0ubuntu1.18`), the
default `rv64` CPU (which has the H extension), and `-bios none`.

- **The probe.** `fixtures/qemu/aia-probe.S` gained ops for `hstatus`,
  `hgeie`, `hgeip`, `vsiselect`/`vsireg`, `vstopei` (read and claim),
  `mip & (SGEIP|VSEIP)` and halfword stores. It also gained an M-mode trap
  handler that prints `0xfa17 << 48 | mcause` and resumes at the next op.
  It was rebuilt with clang 18 / ld.lld 18; the new SHA-256 is in
  `fixtures/dtb/README.md`.
- **New compared scenarios** (`tests/aia_qemu.rs`). For each, the model
  equals the recorded output and the live output:
  1. `hypervisor_guest_files_match_qemu`, on
     `virt,aia=aplic-imsic,aia-guests=2 -smp 4`:
     - GEILEN and `hgeie`
     - VGEIN selection, and the trap for VGEIN 0 and 3
     - `hgeip` following delivery, enables and the threshold
     - SGEIP against VSEIP
     - VS claims, and isolation from the hart's own S file
     - the odd-`eip` and reserved-selector traps
  2. `aplic_to_guest_file_matches_qemu`: S-domain MSIs to guest files 1
     and 2 through the guest index and `smsiaddrcfgh.LHXS`.
  3. `pci_msi_matches_qemu`, with `-device edu` at 00:01.0 (requester ID
     0x08):
     - BAR and command setup, then the MSI capability at 0x40 (64-bit)
     - MSIs into the S file, guest file 1, hart 1's file, and the M-level
       IMSIC. The M-level IMSIC is not the host's `msi-parent`; it is
       delivered and flagged `in_parent: false`.
     - The 16-bit MSI data width.
- **The existing scenarios** (MSI mode, direct mode, supervisor level) still
  pass against live QEMU.
- **New QEMU 8.2.2 departure from the specification.** It is pinned in
  `qemu_8_2_departures_from_the_specification`, where the model follows the
  specification.
  - QEMU takes an S-level domain's LHXW/HHXW/HHXS from `smsiaddrcfgh`
    (`riscv_aplic_msi_send`), where they do not exist and read zero.
  - The specification (§4.5.4, §4.9.1) takes them from `mmsiaddrcfgh`.
  - QEMU therefore drops the hart index of S-level MSIs: hart 1's guest-2
    MSI lands in hart 0's guest-2 file.
  - The other six departures are listed in `silicon-aia-dma.md`.
- **Other QEMU facts this lane relied on, each checked live:**
  - `hgeie` reads back `0b110` after writing `0xffff`, so GEILEN equals
    `aia-guests`.
  - `eidelivery` takes bit 0 of a write of 0x40000000 or 0x40000001.
  - `mireg` traps on `aia=aplic`, which has no IMSIC.
  - An MSI written to a page past the last guest file is dropped.

No hardware with AIA, the H extension, an IOMMU or a PCI MSI controller was
available. Everything above is the model and QEMU.

## Checks

The local Rust is 1.97. The clippy runs used 1.100.0 to match Bazel CI.
Lean is 4.35.0 from the GitHub release zip, matching `lean-toolchain`.

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo +1.100.0 clippy -p vesper-silicon --locked --all-targets -- -D warnings` | clean |
| `VESPER_LEAN=…/lean cargo test -p vesper-silicon --locked` with QEMU 8.2.2 on `PATH` | 149 passed, 0 failed (includes 7 live-QEMU tests, 8 new model tests and 4 Lean tests) |
| `env PATH=/nonexistent target/debug/deps/aia_qemu-*` | 7 passed (recorded QEMU output only) |
| `lean lean/SiliconAia.lean` | no output (no errors, no warnings) |
| `python3 tools/bazel/sync_build_files.py` | no diff |
| `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings` | see below |
| `cargo test --workspace --locked --no-fail-fast` | see below |

Workspace-wide clippy and tests: pending in this draft; results are added before merge.

## Decisions

- **The specification over QEMU**, as before. Departure 7 is pinned in a
  test and stated in Lean (`qemu_s_level_drops_hart`).
- **GEILEN is a hart property.** The devicetree gives only the stride
  (`guest-index-bits`), so the default is the stride's maximum, and
  `limit_guest_files` sets the implemented count, as QEMU's `aia-guests` and
  Linux's `hgeie` probe do.
- **Who supports `eidelivery` 0x40000000.** No binding says, so the model
  infers it: a file whose hart and level a PLIC context or direct IDC also
  reaches. Wired routes select it and MSI routes select 1. A wired route is
  refused while the file delivers MSIs, rather than silently stealing the
  level.
- **The VS-level handler model.** HS-mode software serves SGEIP by
  switching VGEIN, claiming through `vstopei` and restoring VGEIN. That is
  a modelling choice: there is no virtual hart, and `hvip`, `hvictl` and
  `vsie` are not modelled.
- **PCI MSIs land where they decode** (no IOMMU), as on QEMU. The routed
  controller is reported next to where the write went, instead of being
  enforced. A write that decodes to nothing is refused and counted lost.
- **MSI data width.** `pci_msi_write` takes the 32-bit value written. A
  plain MSI capability writes its 16-bit data zero-extended, as the PCI
  scenario shows.
- **`msi-map` follows Linux `of_map_id`**, except that an entry that
  overflows 32 bits is a typed error here, where C would wrap.

## Public API

Every change is an addition, except where noted.

- `aia`: `EIDELIVERY_OFF`/`_ON`/`_EXTERNAL`;
  `ImsicFile::with_external_delivery`, `eidelivery`, `delivers_external`
  and `supports_external_delivery`.
- `intc`:
  - New constants: `VSEIP`, `SGEIP`, `MAX_GEILEN`.
  - New types: `GuestDelivery` and `PciMsi`.
  - New `IntcError` variants: `NoGuestFile`, `NoSuchGuest`,
    `GuestNeedsSupervisor`, `MsiControllerNotModelled`, `ImsicOwnsLevel`.
  - New `RiscvIntc`/`IntcHandle` methods: `geilen`, `limit_guest_files`,
    `set_vgein`/`vgein`, `set_hgeie`/`hgeie`, `hgeip`, `hyp_pending`,
    `vs_imsic_csr_read`/`_write`, `vstopei`, `claim_vstopei`,
    `guest_msi_target`, `install_guest_msi`, `guest_deliveries`,
    `pci_msi_write` and `pci_msis`.
- `dtb`:
  - New types: `MsiMapEntry` and `MsiRoute`.
  - New functions: `requester_id`, `Platform::msi_map`,
    `Platform::pci_msi_route`, `Platform::pci_dma_windows`,
    `Platform::pci_cpu_address` and `DeviceTree::bus_dma_windows`.
  - New `DtbError` variants: `NoMsiMapEntry` and `NoMsiController`.
- **Behaviour changes:**
  - A hart's external level at a level that has an IMSIC file now follows
    that file's `eidelivery`. Before, it was the OR of every controller.
  - On existing platforms nothing differs: no QEMU dump and no published
    board in the fixtures has both an IMSIC file and a wired output for
    the same hart and level.
- Root code uses only `stack`, `atomic`, `irq` and `prologue`, and none of
  them changed.

## Remaining gaps

- No real hardware: the AIA, H-extension and PCI behaviour was exercised
  only on the models and on QEMU 8.2.2.
- **Hypervisor CSRs not modelled:** `hvip` (including injected VSEIP),
  `hvictl`, `hviprio`, `vsie`/`vsip`, `hstateen0.IMSIC`, VS-mode execution
  and the virtual instruction exception. The guest side is the interrupt
  files and the CSRs listed above.
- An APLIC level source forwarded to a guest file is not re-triggered when
  the VS claim completes. The re-trigger exists for the hart's own file.
  The AIA expects hypervisors to emulate APLICs for guests anyway.
- **PCI not modelled:** MSI-X tables, per-vector masking, `iommu-map`, any
  IOMMU or MSI page-table checking, the GICv3 ITS (the AArch64 route is
  computed but not delivered), and `msi-parent` specifier cells
  (`#msi-cells` > 0).
- **Lean coverage:**
  - The trace check covers the hypervisor script only, not the APLIC-guest
    or PCI scripts.
  - `compose_sound` is per window pair, not over whole window lists.
  - The forwarding model has no delegation or source modes; those stay in
    the Rust model and its QEMU checks.
- The linear owners elsewhere in the crate are still Rust ownership
  discipline, not the Vesper source typing rules for `linear` values.

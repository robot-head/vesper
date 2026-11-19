# Port wave P3a lane 7: vesper-silicon, parts 1-2 (devicetree parsing, the AIA models and the interrupt fabric)

Claim [#1042](https://github.com/robot-head/vesper/issues/1042). Base `main` at
3c075a4 (2026-11-18). Plan: `docs/agent-prompts/port-p3/07-silicon.md`
(plan PR #1044).

This is the first part of the lane. It splits the crate into the seed copy and
the port-backed build, and ports the two pieces with the most untrusted-input
and state-machine logic that nothing else in the crate depends on:
`DeviceTree::parse` and the RISC-V AIA register models (`ImsicFile`,
`AplicDomain`). Part 2 (below, "Part 2: the interrupt fabric") ports the
controller state of `intc.rs`. The rest of the lane's list (see "Not ported
yet") follows in later parts.

## Builds

`vesper-silicon` is in the compiler's dependency closure (`src/stack_bound.rs`,
`src/native_verifiers.rs` and `src/freestanding/**` use its stack, prologue,
atomic and IrqGuard code), so it follows the seed-copy rule of
`docs/maintenance/porting-a-crate.md`:

| Package | Path | What it is | Who links it |
|---|---|---|---|
| `vesper-silicon-seed` | `crates/vesper-silicon/seed` | the Rust build of `../src`, library `vesper_silicon`, no build script | the compiler (`vesper`), its only dependent |
| `vesper-silicon` | `crates/vesper-silicon` | `build.rs` links the Loom package `vesper/` and sets `cfg(vesper_port)` | its own tests and `examples/abba.rs` only |

Under the owner's rule, Rust stays the default: no dependent links the port,
and no Rust code is deleted. `tests/parity.rs` checks which build each package
is (`vesper_silicon::IMPLEMENTATION`).

## API inventory (re-measured before porting)

| Module | src lines | Public API | This part |
|---|--:|---|---|
| `dtb` | 2,911 | `DeviceTree` (`parse`, tree access, property decoding, `reg`/`ranges`/`dma-ranges` translation, PCI windows, interrupt parents and `interrupt-map` nexus resolution), `Platform::from_tree` and its queries, `place_dynamic`, `isa_has_extension`, `requester_id`, the info types | `DeviceTree::parse` (header, reservation block, structure block, strings, phandle table, every error) |
| `aia` | 1,008 | `ImsicFile`, `AplicDomain`, `MsiAddrCfg::address`/`hart_of`, `SourceMode`, `Msi`, `AiaError`, register constants | `ImsicFile` and `AplicDomain` (every method), `MsiAddrCfg::address` |
| `intc` | 2,765 | PLIC/APLIC/IMSIC/CLINT emulation wired to harts (`RiscvIntc`, `IntcHandle`, ...) | part 2: the controller state and its operations; dispatch, routing and the handler table stay Rust |
| `dma`, `frames`, `ccache`, `ledger`, `irq` | 2,016 | DMA ranges and transfers, frame allocator and auditor, SiFive cache controller, the leak ledger, IrqGuard `Cpu` | later parts |
| `svd`, `pac` | 836 | the SVD register model and PAC text generation | later part (the model; `svd-parser` stays Rust) |
| `stack`, `prologue`, `hazard`, `atomic`, `mmio` | 2,690 | Cranelift analyses, MMIO typestate | stay Rust (lane plan) |

## What was ported

The Loom package `@vesper/silicon` (`crates/vesper-silicon/vesper`, 1,833
lines, 32 exported functions, export ABI v3):

| File | Rust source | Contents |
|---|---|---|
| `lib.ves` | | the exported types (`AiaFault`, `MsiCfg`, `Msi`, the `Imsic` and `Aplic` handles, `DtbFault`, the `Fdt` tape) and the exports |
| `aia.ves` | `aia.rs` | IMSIC files (`seteipnum`, MMIO, the indirect CSRs, `topei` and claims, the `eidelivery` 0x40000000 rule), APLIC domains (`domaincfg`, `sourcecfg` and delegation, the six source modes and the pending-bit rules of §4.7, the pending/enable arrays and their `*num` registers, `target` in both formats, `genmsi`, `msiaddrcfg` with the lock bit, the direct-mode IDCs), MSI addresses (§4.9.1) |
| `dtb.ves` | `dtb.rs` | `DeviceTree::parse`: versions 16 and 17, every length, offset, alignment, terminator, UTF-8 check, token, nesting and count limit in the Rust build's order, and the phandle table with its two errors |

The Rust glue:

- `src/aia.rs` keeps the shared types and selects the models: the Rust build
  moved unchanged to `src/aia/rust.rs` (`cfg(not(vesper_port))`), and
  `src/aia/port.rs` holds `ImsicFile(binding::Imsic)` and
  `AplicDomain(binding::Aplic)`, whose methods forward to the binding. Plain
  fields (`ids`, `eidelivery`, `base`, `sources`, `msi_cfg`, ...) are read in
  place through the handle's view. `Clone` and `PartialEq` call the port
  (`imsic_clone`, `imsic_eq`, `aplic_clone`); `Debug` prints the same fields.
- `DeviceTree::parse` forwards to `src/dtb/port.rs` under `cfg(vesper_port)`;
  the Rust body is `parse_rust` under `cfg(not(vesper_port))`.

Decisions:

- **A tape, not Rust values.** The export ABI cannot return `Vec<Node>` with
  `String` names (strings are refused as sequence elements). `dtb_parse`
  returns an `Fdt` handle with three words per node (name offset and length,
  parent), five per property (node, name offset and length, value offset and
  length), the reservations and the phandle table, all offsets into the
  caller's blob; the wrapper builds the same `Node`, `Prop` and `BTreeMap`
  values. Vesper checks every name as UTF-8 (the `std::str::from_utf8`
  rules), so the wrapper's conversion cannot fail.
- **Errors.** Structural failures raise `DtbFault`, with the Rust build's
  `&'static str` details as codes. The two phandle errors need the node's path,
  which the wrapper builds, so the tape records them (`ph_fault`) and the
  wrapper raises `BadProperty` or `DuplicatePhandle` after building the tree.
  `AiaError::NoDeliveryMode` gets the domain's path in the wrapper.
- **The phandle table.** The Rust build inserts `phandle` then
  `linux,phandle` (the first property of each name) node by node into a
  `BTreeMap` and fails on the first phandle another node already holds. The
  port collects the same entries (up to the first malformed one), sorts
  `phandle << 32 | position` and takes the lowest position whose phandle an
  earlier entry of another node holds, which is the Rust build's first error,
  in O(n log n).
- **Arithmetic.** Register values are `u64` words holding the Rust build's
  `u32`s; every operation is the wrapping or total one the Rust build uses on
  in-range values (`wrapping_shl_u64`, `leading_zeros_u64`,
  `trailing_zeros_u64`), so the models raise no `ArithOverflow`.

## What stays in Rust

| File or item | Reason | Unblocked by |
|---|---|---|
| `stack.rs`, `prologue.rs`, `hazard.rs`, `atomic.rs` | Cranelift `ir` and machine code | stays Rust (lane plan) |
| `mmio.rs` | `unsafe` volatile access | stays Rust |
| `svd.rs` parsing | `svd-parser` | stays Rust; the model after parsing is a later part |
| `intc.rs` dispatch, routing and the handler table | the handlers are `Box<dyn FnMut>` closures that change controller state while they run; the controller state itself is ported (part 2) | stays Rust: the dispatch loop asks the port for one claim at a time |
| `dtb.rs` past `parse` | not ported yet | a later part |
| `dma.rs`, `frames.rs`, `ccache.rs`, `ledger.rs`, `irq.rs`, `pac.rs` | not ported yet | later parts |
| `seed/` | the compiler's copy | self-hosting Stage 2 |

The crate's `unsafe` count is unchanged in the Rust build (`mmio.rs` and
`pac.rs` text only). The port-backed build adds the generated binding
(`port/binding.rs`, `unsafe` FFI calls); the Vesper source has none.

## Unit tests and test changes

- No ported module has `#[cfg(test)]` tests (the crate's unit tests are in
  `pac.rs` and `prologue.rs`); they run in both packages.
- `tests/lean.rs` found `lean/` through `CARGO_MANIFEST_DIR`; it now steps
  out of `seed/` when the seed runs it.
- New `tests/parity.rs` (port package only): every fixture parses to the same
  tree in both builds, with trailing bytes; hand-written trees reach the
  phandle table's cases (duplicates across nodes and spellings, the same
  phandle twice on one node, a later property of the same name, a malformed
  phandle before a duplicate, versions 16 and 17); patched token streams and
  header fields reach all 11 structural error kinds; truncations and 8,400
  random byte edits of the 21 fixtures must agree. 60 IMSIC scripts and 80
  APLIC scripts of 400-500 random operations (MMIO reads and writes over the
  whole register map, misaligned and unmapped addresses, wires, delegation,
  claims, MSIs, `msiaddrcfg`) compare every result and the observable state
  after every step.
- New `tests/port_memory.rs`: parsing, cloning, MSIs and drops free every
  sequence the port allocates.

All 149 tests of the crate pass against both packages, and the 8 new ones
against the port (`cargo test -p vesper-silicon -p vesper-silicon-seed`; the
QEMU cross-check `tests/aia_qemu.rs` compares against recorded QEMU 8.2.2
output, and `tests/aia_hypervisor.rs` runs everywhere).

## Divergence checks

`build.rs` (`vesper-port-divergent`, one edit at a time with
`VESPER_SILICON_DIVERGENT`) and `BUILD.bazel` (`silicon_vesper_divergent_*`)
apply one edit per ported module; `port/check.sh`, sourced by
`scripts/test-port-harness.sh`, asserts which test catches it:

| Module | Edit | Fails | Still passes |
|---|---|---|---|
| IMSIC | an identity equal to `eithreshold` is delivered | `parity::imsic_files_match` | `parity::every_fixture_parses_identically` |
| APLIC | equal IDC priorities go to the higher source | `parity::aplic_domains_match` | `parity::imsic_files_match` |
| PLIC (fabric) | a claim leaves its source pending | `parity::interrupt_fabrics_match` | `parity::imsic_files_match` |
| devicetree | node names start one byte late | `dtb::qemu_riscv_virt_memory_harts_and_plic` | `dtb::malformed_headers_are_typed_errors` |

## Part 2: the interrupt fabric

Part 2 ports the controller state of `intc.rs`: every PLIC, the APLIC domain
trees, the IMSIC files with their guest files, the CLINT/ACLINT blocks, the
MTIME counters and the hypervisor CSRs (`vgein`, `hgeie`), with the
operations over them: levels, settling the APLIC trees, MSI delivery, every
register read and write, wires, claim and complete. `RiscvIntc` keeps its
API.

How it is split:

- **One method interface.** The Rust fabric's controller state moved into
  `intc/rust.rs` as `Core` (`cfg(not(vesper_port))`); `intc/port.rs` is a
  `Core` holding one Vesper `Fabric` handle with the same methods, each one
  call into `vesper/src/intc.ves` (1,700 lines). `intc.rs` keeps the
  `Platform` info it routes with, the allocated identities, the asserted and
  enabled causes, the logs and the statistics.
- **Dispatch stays Rust, one claim at a time.** The handler table
  (`Box<dyn FnMut>`) and the dispatch loops stay in `intc.rs`, which asks
  the port for one claim (`fabric_claim_external`), runs that handler, lets
  it change controller state through `IntcHandle`, completes the claim and
  asks again. Nothing computes a dispatch list up front.
- **Banks, not handles of handles.** Export ABI v3 refuses sequences of
  handles, so the AIA models became banks: an `Imsic` holds many interrupt
  files and an `Aplic` many domains, as headers (copy structs) plus shared
  per-file and per-source sequences, and every model function takes the
  file or domain index. The standalone `ImsicFile` and `AplicDomain` are
  banks of one. The fabric holds one bank of each, and its other controllers
  the same way (`PlicHdr`, `DomTopo`, `ImsicBlk`/`ImsicSlot`, `ClintHdr`
  over shared sequences).
- **A fused settle step.** The Rust `settle` asks `delegated_to`, `mode` and
  `set_active_here` of every source of every child domain. The port does a
  child's whole source loop in one function (`m_aplic_inherit`) with both
  domains' headers read once: the first version made those calls per source
  and ran `aia-intc` at 5.2x Rust's instructions; the fused loop runs it at
  1.09x.
- Hart targets cross as one word, `hart << 32 | cause` (cause `0xffffffff`
  for an output the tree marks unused); absent indices are `u64::MAX`.
  Faults are `IntcFault`, carrying `AiaFault` for the models' errors, and the
  wrapper adds the domain path to `NoDeliveryMode`.

Tests: `tests/parity.rs::interrupt_fabrics_match` runs one script per
fixture, privilege and seed (114 runs on RISC-V fixtures, 102 of them with
external deliveries) through both builds: every device's handler (which
lowers its line), an MSI handler and a guest-file MSI handler per hart,
software and timer handlers, then 200 random operations (device lines,
register reads and writes across every controller window, MTIME, MSIs, IMSIC
and hypervisor CSRs), comparing every result, the delivery count after each
step and the final delivery logs, statistics and lost MSIs. All crate tests
pass against both packages, and `tests/port_memory.rs` drops whole fabrics
with nothing left allocated.

## Performance

`examples/abba.rs` runs both builds in one process (the seed as the dev
dependency `vesper-silicon-rust`): per epoch Rust, port, port, Rust, 20 runs
each after 5 warm-up runs, analyzed with vesper-bench. Release builds, Intel
Xeon @ 2.10 GHz (4 vCPUs, cloud host), 11 epochs. Instructions are callgrind
counts per run (20 runs minus a 0-run baseline).

Part 2, after the fabric port (base 3dbbdc3):

| Workload | Rust ns/run | Port ns/run | Wall ratio (range) | Rust Ir/run | Port Ir/run | Ir ratio |
|---|--:|--:|--:|--:|--:|--:|
| `dtb-parse` (36 KB VisionFive 2 tree) | 224,779 | 234,477 | 1.05x (1.03-1.09) | 2,321,000 | 2,561,604 | 1.10x |
| `dtb-platform` (parse, `Platform`, per-device queries) | 362,489 | 381,843 | 1.05x (1.04-1.12) | 3,640,249 | 3,905,859 | 1.07x |
| `aia-intc` (QEMU `aplic-imsic`, 64 UART MSIs) | 251,874 | 303,651 | 1.21x (1.18-1.22) | 3,632,888 | 3,977,942 | 1.09x |
| `aplic-direct` (63 sources, 256 wire changes and claims) | 67,216 | 138,337 | 2.05x (2.05-2.07) | 723,251 | 1,255,633 | 1.74x |
| `imsic` (2,047 identities, 256 MSIs and claims) | 2,007 | 6,933 | 3.48x (3.22-3.52) | 18,850 | 100,376 | 5.33x |

Part 1, for comparison:

| Workload | Rust ns/run | Port ns/run | Wall ratio (range) | Rust Ir/run | Port Ir/run | Ir ratio |
|---|--:|--:|--:|--:|--:|--:|
| `dtb-parse` (36 KB VisionFive 2 tree) | 230,998 | 254,163 | 1.10x (0.92-1.32) | 2,320,942 | 2,560,678 | 1.10x |
| `dtb-platform` (parse, `Platform`, per-device queries) | 392,161 | 403,189 | 1.03x (0.86-1.09) | 3,638,598 | 3,910,551 | 1.07x |
| `aia-intc` (QEMU `aplic-imsic`, 64 UART MSIs) | 259,322 | 923,089 | 3.52x (3.34-3.63) | 3,619,990 | 12,384,123 | 3.42x |
| `aplic-direct` (63 sources, 256 wire changes and claims) | 69,249 | 136,921 | 1.99x (1.87-2.10) | 723,248 | 1,034,465 | 1.43x |
| `imsic` (2,047 identities, 256 MSIs and claims) | 1,517 | 4,925 | 3.39x (1.98-3.71) | 18,851 | 64,389 | 3.42x |

`crates/vesper-silicon` is not at parity, so Rust stays the default.

What the numbers say (part 2):

- **The fabric is near parity.** With the callers in the same library as the
  models, `aia-intc` went from 3.42x to 1.09x of Rust's instructions (3.52x
  to 1.21x wall). The cross-boundary calls per source are gone: `intc.rs`
  now makes one call per operation.
- **The standalone models got slower.** `aplic-direct` (1.43x to 1.74x) and
  `imsic` (3.42x to 5.33x) drive one `AplicDomain` or `ImsicFile` through the
  binding. As banks, each call copies the file's or domain's header out of
  a sequence and adds its offset to every index. Nothing in the crate uses
  the standalone models on a hot path any more (the fabric calls the banks
  directly), but their API is public.

What the numbers said (part 1):

- **Parsing is close.** The port's parse checks the blob in Vesper and the
  wrapper then builds the same `String`s and `Vec`s the Rust build builds,
  so the allocation work is shared and the 10% is the Vesper pass.
- **The AIA gap is the boundary, not the models.** The models' methods are
  a few instructions each in Rust, inlined into `intc.rs`. In the port each
  is a call across the C ABI, and `intc.rs` (not ported yet) calls them per
  source: its `settle` asks `delegated_to`, `mode` and `set_active_here` of
  every source of every child domain on each pass, 1.9 million calls per 20
  `aia-intc` runs. Callgrind attributes 23% of the port's instructions to
  `memcpy` inside those calls, and most of the rest to call overhead around
  bodies that are a bounds check and a load.

Compiler and ABI gaps, each with a repro:

1. **`in` handles are copied on every call.** The generated binding passes an
   `in` handle as a scratch copy of its words (`let mut arg_a = a.words;` in
   `aplic_mode`, `imsic_top`, ...), because the export ABI lets a callee
   overwrite aggregate arguments. Vesper never writes an `in` aggregate
   (E0307), and export ABI v3 already lends `in` mirrors in place; handles
   could be lent the same way. Repro: `pub struct H { a: u64, s: ChunkSeq[u64] }`
   and `pub fn h_a(in h: H) -> u64 { h.a }` in a `// @export abi 3` library:
   the binding copies `H`'s words before each call, a `memcpy` of 27 words for
   `Aplic`.
2. **No inlining across the boundary.** A model method of a few instructions
   costs a call, the copy above and the status handling per use. Part 2
   closed this for the fabric by moving the callers into the library.
3. **No sequences of handles (part 2).** Exporting `ChunkSeq[Imsic]` is
   refused ("only sequences and slices of copy elements cross the
   boundary"), and a non-copy element cannot be read in place (E1522) or
   passed `inout` (E0300). A fabric of many files therefore keeps one bank
   with headers and offsets, which is what makes the standalone models
   slower. Repro: `pub struct F { files: ChunkSeq[Imsic] }` with an export
   taking `inout f: F`.
4. **Header reads copy the whole struct (part 2).** `match a.hdr.try_get(d)`
   copies all 21 words of an `AplicHdr` to read `sources`; a field read
   through the sequence would be one load. The fused settle step works
   around this by reading each header once per loop.

## Not ported yet (later parts of this lane)

- `dtb.rs` after `parse`: property decoding, `ranges` and `dma-ranges`
  translation, PCI windows, interrupt-parent and `interrupt-map` resolution,
  and `Platform::from_tree` with its queries. Its errors carry node paths and
  formatted details, so the port will return codes and node indices and the
  wrapper will format them, as for the phandle errors here.
- `dma.rs`, `frames.rs`, `ccache.rs`, the model half of `irq.rs`, `ledger.rs`.
- The SVD register model and its queries (`svd.rs` after `svd-parser`), and
  `pac.rs` if its only `unsafe` is in emitted text.

## Platforms run

Linux x86_64 (this cloud host): both packages' tests, the divergence checks,
`cargo +1.100.0 clippy --all-targets -p vesper-silicon -p vesper-silicon-seed
-- -D warnings`, `vesper fmt --check`, the ABBA benchmark and callgrind. CI runs
the four required checks (Bazel on BuildBuddy, Linux, macOS AArch64, Windows
MSVC); the port's lines in `scripts/test-port-harness.sh` run on the Linux
port shards.

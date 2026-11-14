//! Devicetree nexus maps, `ranges` translation, wide cells, dynamic
//! `/reserved-memory`, version-16 blobs and `riscv,isa-extensions`, on the
//! published DTBs of QEMU `virt`, the SiFive HiFive Unleashed / Unmatched and
//! the StarFive VisionFive 1 / 2 (provenance in `fixtures/dtb/README.md`),
//! plus hand-written edge cases.

use vesper_silicon::Arch;
use vesper_silicon::dtb::{
    DeviceTree, DmaCoherence, DtbError, HartTarget, MemRange, PciSpace, PciWindow, Platform,
    ReservationSource, place_dynamic,
};

const RV_VIRT: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt.dtb");
const ARM_VIRT: &[u8] = include_bytes!("../fixtures/dtb/qemu-aarch64-virt-gicv3.dtb");
const ACLINT: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aclint.dtb");
const UNLEASHED: &[u8] = include_bytes!("../fixtures/dtb/hifive-unleashed-a00.dtb");
const UNMATCHED: &[u8] = include_bytes!("../fixtures/dtb/hifive-unmatched-a00.dtb");
const UNMATCHED_V16: &[u8] = include_bytes!("../fixtures/dtb/hifive-unmatched-a00-v16.dtb");
const VF1: &[u8] = include_bytes!("../fixtures/dtb/jh7100-starfive-visionfive-v1.dtb");
const VF2: &[u8] = include_bytes!("../fixtures/dtb/jh7110-starfive-visionfive-2-v1.3b.dtb");
const BOARD: &[u8] = include_bytes!("../fixtures/dtb/vesper-rv-board.dtb");
const BOARD_V16: &[u8] = include_bytes!("../fixtures/dtb/vesper-rv-board-v16.dtb");
const NEXUS: &[u8] = include_bytes!("../fixtures/dtb/vesper-nexus-ranges.dtb");
const LOOP: &[u8] = include_bytes!("../fixtures/dtb/vesper-nexus-loop.dtb");

const GIB: u64 = 1 << 30;

fn mr(base: u64, size: u64) -> MemRange {
    MemRange { base, size }
}

fn target(hart: u32, cause: Option<u32>) -> HartTarget {
    HartTarget { hart, cause }
}

#[test]
fn sifive_unleashed_hidden_contexts_and_isa_extensions() {
    let p = Platform::from_dtb(UNLEASHED).unwrap();
    assert_eq!(p.arch, Arch::Riscv64);
    assert_eq!(p.memory, [mr(0x8000_0000, 8 * GIB)]);
    // The E51 monitor core (cpu@0) is disabled; the four U54s remain.
    let ids: Vec<u32> = p.harts.iter().map(|h| h.id).collect();
    assert_eq!(ids, [1, 2, 3, 4]);
    for h in &p.harts {
        assert_eq!(h.isa_base.as_deref(), Some("rv64i"));
        let ext = h.isa_extensions.as_ref().unwrap();
        assert_eq!(
            ext,
            &[
                "i", "m", "a", "f", "d", "c", "zicntr", "zicsr", "zifencei", "zihpm"
            ]
        );
        assert!(h.has_extension("zifencei") && h.has_extension("D"));
        assert!(!h.has_extension("zicbom"));
    }
    // PLIC: 53 sources, 9 contexts; the M-mode ones are marked 0xffffffff
    // (present in hardware, not for the OS) and kept as hidden.
    let plic = &p.plics[0];
    assert_eq!(
        (plic.path.as_str(), plic.base, plic.ndev),
        ("/soc/interrupt-controller@c000000", 0xc00_0000, 53)
    );
    assert_eq!(
        plic.contexts,
        [
            target(0, None),
            target(1, None),
            target(1, Some(9)),
            target(2, None),
            target(2, Some(9)),
            target(3, None),
            target(3, Some(9)),
            target(4, None),
            target(4, Some(9)),
        ]
    );
    // Hidden outputs are not interrupt lines.
    let plic_out: Vec<u32> = p
        .irq_lines
        .iter()
        .filter(|l| l.device == plic.path)
        .map(|l| l.number)
        .collect();
    assert_eq!(plic_out, [9, 9, 9, 9]);
    assert_eq!(p.irqs_of("/soc/serial@10010000").unwrap(), [4]);
    assert_eq!(
        p.dma_coherence("/soc/serial@10010000").unwrap(),
        DmaCoherence::Coherent
    );
}

#[test]
fn sifive_unmatched_pcie_windows_and_intx_nexus() {
    let p = Platform::from_dtb(UNMATCHED).unwrap();
    assert_eq!(p.memory, [mr(0x8000_0000, 16 * GIB)]);
    assert_eq!(p.irqs_of("/soc/serial@10010000").unwrap(), [39]);
    let t = p.tree();
    let host = t.find("/soc/pcie@e00000000").unwrap();
    // 3 address cells on the PCI side, translated through /soc's `ranges;`.
    assert_eq!(
        t.pci_windows(host).unwrap(),
        [
            PciWindow {
                space: PciSpace::Io,
                prefetchable: false,
                pci_addr: 0x6008_0000,
                cpu_addr: 0x6008_0000,
                size: 0x1_0000
            },
            PciWindow {
                space: PciSpace::Mem32,
                prefetchable: false,
                pci_addr: 0x6009_0000,
                cpu_addr: 0x6009_0000,
                size: 0xff7_0000
            },
            PciWindow {
                space: PciSpace::Mem32,
                prefetchable: false,
                pci_addr: 0x7000_0000,
                cpu_addr: 0x7000_0000,
                size: 0x1000_0000
            },
            PciWindow {
                space: PciSpace::Mem64,
                prefetchable: true,
                pci_addr: 0x20_0000_0000,
                cpu_addr: 0x20_0000_0000,
                size: 0x20_0000_0000
            },
        ]
    );
    // The host's own registers: three windows at 0xe_0000_0000 etc.
    assert_eq!(
        p.mmio("/soc/pcie@e00000000").unwrap()[0].base,
        0xe_0000_0000
    );
    // interrupt-map-mask <0 0 0 7>: INTA..INTD -> PLIC 57..60 for any slot.
    for dev in [0u8, 1, 31] {
        let got: Vec<u32> = (1..=4)
            .map(|pin| {
                p.pci_intx("/soc/pcie@e00000000", 1, dev, 0, pin)
                    .unwrap()
                    .number
            })
            .collect();
        assert_eq!(got, [57, 58, 59, 60]);
    }
    let line = p.pci_intx("/soc/pcie@e00000000", 0, 0, 0, 1).unwrap();
    assert_eq!(line.controller, "/soc/interrupt-controller@c000000");
    assert!(matches!(
        p.pci_intx("/soc/pcie@e00000000", 0, 0, 0, 5),
        Err(DtbError::BadInterruptSpecifier { .. })
    ));
    assert!(matches!(
        p.pci_intx("/soc/serial@10010000", 0, 0, 0, 1),
        Err(DtbError::BadProperty { .. })
    ));
}

#[test]
fn qemu_virt_pci_swizzle_on_plic_and_gic() {
    // riscv virt: slot s, pin p -> PLIC 0x20 + (s + p - 1) % 4; the mask
    // keeps two slot bits, so slot 4 behaves like slot 0.
    let p = Platform::from_dtb(RV_VIRT).unwrap();
    let host = "/soc/pci@30000000";
    for slot in 0..8u8 {
        for pin in 1..=4u8 {
            let want = 0x20 + (u32::from(slot % 4) + u32::from(pin) - 1) % 4;
            assert_eq!(p.pci_intx(host, 0, slot, 0, pin).unwrap().number, want);
        }
    }
    let t = p.tree();
    let windows = t.pci_windows(t.find(host).unwrap()).unwrap();
    assert_eq!(
        windows
            .iter()
            .map(|w| (w.space, w.pci_addr, w.cpu_addr, w.size))
            .collect::<Vec<_>>(),
        [
            (PciSpace::Io, 0, 0x300_0000, 0x1_0000),
            (PciSpace::Mem32, 0x4000_0000, 0x4000_0000, 0x4000_0000),
            (PciSpace::Mem64, 0x4_0000_0000, 0x4_0000_0000, 0x4_0000_0000),
        ]
    );
    // aarch64 virt: the parent unit address has the GIC's 2 cells; SPI
    // 3 + (s + p - 1) % 4 -> INTID 35 + ...
    let p = Platform::from_dtb(ARM_VIRT).unwrap();
    let host = p
        .tree()
        .nodes()
        .iter()
        .enumerate()
        .find(|(_, n)| n.name.starts_with("pcie@"))
        .map(|(i, _)| p.tree().path(i))
        .unwrap();
    for slot in 0..4u8 {
        for pin in 1..=4u8 {
            let l = p.pci_intx(&host, 0, slot, 0, pin).unwrap();
            assert_eq!(l.number, 35 + (u32::from(slot) + u32::from(pin) - 1) % 4);
            assert_eq!(l.specifier[0], 0, "SPI");
        }
    }
}

#[test]
fn starfive_visionfive2_nested_ranges_and_pcie_intc() {
    let p = Platform::from_dtb(VF2).unwrap();
    assert_eq!(p.memory, [mr(0x4000_0000, 4 * GIB)]);
    assert_eq!(p.harts.len(), 4);
    assert!(
        p.harts
            .iter()
            .all(|h| h.has_extension("zba") && h.has_extension("zbb"))
    );
    // usb@10100000 maps its 32-bit child space at 0x10100000; the cdns3
    // controller's three regions land there.
    assert_eq!(
        p.mmio("/soc/usb@10100000/usb@0").unwrap(),
        [
            mr(0x1010_0000, 0x1_0000),
            mr(0x1011_0000, 0x1_0000),
            mr(0x1012_0000, 0x1_0000)
        ]
    );
    // The untranslated values are the child bus's.
    let t = p.tree();
    let usb = t.find("/soc/usb@10100000/usb@0").unwrap();
    assert_eq!(t.reg(usb).unwrap()[1], mr(0x1_0000, 0x1_0000));
    assert_eq!(
        p.irqs_of("/soc/usb@10100000/usb@0").unwrap(),
        [100, 108, 110]
    );
    // An I2C device's reg is not a CPU address.
    let pmic = t
        .nodes()
        .iter()
        .enumerate()
        .find(|(i, n)| n.name.starts_with("pmic@") && t.has_prop(*i, "reg"))
        .map(|(i, _)| i)
        .unwrap();
    assert!(matches!(
        t.reg_translated(pmic),
        Err(DtbError::Untranslatable { .. })
    ));
    // PCIe INTx goes to the bridge's own interrupt controller child, which
    // the platform model does not emulate.
    let host = t.find("/soc/pcie@940000000").unwrap();
    let (ctl, spec) = t.map_interrupt(host, host, vec![0, 0, 0], vec![2]).unwrap();
    assert_eq!(
        (t.path(ctl).as_str(), spec.as_slice()),
        ("/soc/pcie@940000000/interrupt-controller", &[2][..])
    );
    assert!(matches!(
        p.pci_intx("/soc/pcie@940000000", 0, 0, 0, 1),
        Err(DtbError::NotAnInterruptController { .. })
    ));
    let w = t.pci_windows(host).unwrap();
    assert_eq!(
        (w[1].space, w[1].prefetchable, w[1].cpu_addr, w[1].size),
        (PciSpace::Mem64, true, 0x9_0000_0000, 0x4000_0000)
    );
    // The opensbi reservation is taken out of the frames.
    assert_eq!(p.reservations[0].range, mr(0x4000_0000, 0x20_0000));
    assert_eq!(p.usable_frame_ranges().unwrap()[0].0, 0x40200);
    // CLINT: one software + one timer output per hart, hart 0 (S7) included.
    let clint = &p.clints[0];
    assert_eq!(clint.targets.len(), 10);
    assert_eq!(clint.targets[2], target(1, Some(3)));
    assert_eq!(clint.targets[3], target(1, Some(7)));
}

#[test]
fn starfive_visionfive1_noncoherent_without_zicbom_uses_the_ccache() {
    // The JH7100 SoC bus is dma-noncoherent and its U74 harts have no
    // Zicbom: Linux maintains it with the SiFive cache controller's FLUSH64
    // register (wave eight lane 26 models it; it used to be a refusal).
    let p = Platform::from_dtb(VF1).unwrap();
    assert_eq!(
        p.dma_coherence("/soc/serial@12440000").unwrap(),
        DmaCoherence::SifiveCcache {
            flush64: 0x0201_0200,
            line_bytes: 64,
        }
    );
    let srcs: Vec<_> = p.reservations.iter().map(|r| r.range).collect();
    assert_eq!(
        srcs,
        [mr(0xfa00_0000, 0x100_0000), mr(0x10_7a00_0000, 0x100_0000)]
    );
    let frames = p.usable_frame_ranges().unwrap();
    assert!(frames.iter().all(|&(b, n)| {
        let (lo, hi) = (b * 4096, (b + n) * 4096);
        srcs.iter().all(|r| hi <= r.base || lo >= r.end().unwrap())
    }));
}

#[test]
fn hand_written_nexus_chain_ranges_wide_cells_and_dynamic_reservations() {
    let p = Platform::from_dtb(NEXUS).unwrap();
    // Connector line 1 (flags masked off) -> PLIC 31; line 2 -> second
    // nexus -> PLIC 40; ordinary devices on the translated bus.
    assert_eq!(p.irqs_of("/button").unwrap(), [31]);
    assert_eq!(p.irqs_of("/sensor").unwrap(), [40]);
    assert_eq!(
        p.irqs_of("/soc/apb@400000000/sub@10000/timer@100").unwrap(),
        [20]
    );
    assert_eq!(
        p.mmio("/soc/apb@400000000/sub@10000/timer@100").unwrap(),
        [mr(0x4_0001_0100, 0x10)]
    );
    assert_eq!(
        p.mmio("/soc/apb@400000000/uart@202000").unwrap(),
        [mr(0x4_1020_2000, 0x100)]
    );
    assert!(matches!(
        p.mmio("/soc/apb@400000000/orphan@300000"),
        Err(DtbError::NoRangesEntry { .. })
    ));
    // 3 address cells: (1, 0, 0x10) -> 0x5000_0010.
    let t = p.tree();
    let wide = t.find("/wide/dev@1,0,10").unwrap();
    assert_eq!(t.reg_raw(wide).unwrap()[0].address, [1, 0, 0x10]);
    assert!(matches!(t.reg(wide), Err(DtbError::AddressOverflow { .. })));
    assert_eq!(t.reg_translated(wide).unwrap(), [mr(0x5000_0010, 0x20)]);
    // Dynamic regions: the 16 MiB CMA pool, 4 MiB aligned, top-down inside
    // its alloc-range; the 1 MiB buffer at the top of memory.
    let dynamic: Vec<_> = p
        .reservations
        .iter()
        .filter(|r| matches!(r.source, ReservationSource::Dynamic { .. }))
        .map(|r| (r.range, r.source.clone()))
        .collect();
    assert_eq!(
        dynamic,
        [
            (
                mr(0x8700_0000, 0x100_0000),
                ReservationSource::Dynamic {
                    path: "/reserved-memory/cma".into(),
                    no_map: false
                }
            ),
            (
                mr(0x8ff0_0000, 0x10_0000),
                ReservationSource::Dynamic {
                    path: "/reserved-memory/vbuf".into(),
                    no_map: true
                }
            ),
        ]
    );
    assert_eq!(
        p.usable_frame_ranges().unwrap(),
        [(0x80200, 0x6e00), (0x88000, 0x7f00)]
    );
    // A region that cannot fit is a typed error.
    let node = t.find("/reserved-memory/cma").unwrap();
    assert!(matches!(
        place_dynamic(t, node, &[mr(0x8000_0000, 0x10_0000)], &[]),
        Err(DtbError::ReservationDoesNotFit {
            size: 0x100_0000,
            alignment: 0x40_0000,
            ..
        })
    ));
    // riscv,isa-extensions names Zicbom, so a non-coherent device would get
    // Zicbom maintenance.
    assert!(p.harts[0].has_extension("zicbom"));
    assert_eq!(p.harts[0].isa, None);
    assert_eq!(p.clints.len(), 1);
}

#[test]
fn nexus_loops_are_typed_errors() {
    assert!(matches!(
        Platform::from_dtb(LOOP),
        Err(DtbError::NexusLoop { .. })
    ));
}

#[test]
fn version_16_blobs_parse_like_their_version_17_originals() {
    for (v16, v17) in [(BOARD_V16, BOARD), (UNMATCHED_V16, UNMATCHED)] {
        let a = DeviceTree::parse(v16).unwrap();
        let b = DeviceTree::parse(v17).unwrap();
        assert_eq!((a.version, b.version), (16, 17));
        assert_eq!(a.nodes(), b.nodes());
        assert_eq!(a.reservations, b.reservations);
        assert_eq!(
            Platform::from_tree(a)
                .unwrap()
                .usable_frame_ranges()
                .unwrap(),
            Platform::from_tree(b)
                .unwrap()
                .usable_frame_ranges()
                .unwrap()
        );
    }
    // Version 15, or a last-compatible version above the version, is not.
    let mut b = BOARD_V16.to_vec();
    b[20..24].copy_from_slice(&15u32.to_be_bytes());
    assert!(matches!(
        DeviceTree::parse(&b),
        Err(DtbError::UnsupportedVersion { version: 15, .. })
    ));
    let mut b = BOARD_V16.to_vec();
    b[24..28].copy_from_slice(&17u32.to_be_bytes());
    assert!(matches!(
        DeviceTree::parse(&b),
        Err(DtbError::UnsupportedVersion { .. })
    ));
    // A v16 blob cut before its FDT_END is truncated, not a panic.
    assert!(DeviceTree::parse(&BOARD_V16[..BOARD_V16.len() - 40]).is_err());
}

/// Offset of the value of the property `name` whose value is `value`.
fn prop_value_at(blob: &[u8], name: &str, value: &[u8]) -> usize {
    let be = |at: usize| u32::from_be_bytes(blob[at..at + 4].try_into().unwrap()) as usize;
    let (off_struct, off_strings) = (be(8), be(12));
    let mut want = name.as_bytes().to_vec();
    want.push(0);
    let nameoff = blob[off_strings..]
        .windows(want.len())
        .position(|w| w == want)
        .unwrap();
    let mut pat = vec![0, 0, 0, 3];
    pat.extend((value.len() as u32).to_be_bytes());
    pat.extend((nameoff as u32).to_be_bytes());
    pat.extend(value);
    off_struct
        + blob[off_struct..]
            .windows(pat.len())
            .position(|w| w == pat)
            .unwrap()
        + 12
}

#[test]
fn malformed_cells_ranges_and_maps_are_typed_errors() {
    // #address-cells = 5 on the wide bus.
    let mut b = NEXUS.to_vec();
    let at = prop_value_at(&b, "#address-cells", &3u32.to_be_bytes());
    b[at..at + 4].copy_from_slice(&5u32.to_be_bytes());
    let p = Platform::from_dtb(&b).unwrap();
    assert!(matches!(
        p.mmio("/wide/dev@1,0,10"),
        Err(DtbError::UnsupportedCells { cells: 5, .. })
    ));
    // A mask that drops bit 0 makes the button's line 1 read as line 0.
    let mut b = NEXUS.to_vec();
    let at = prop_value_at(&b, "interrupt-map-mask", &[0, 0, 0, 0xff, 0, 0, 0, 0]);
    b[at + 3] = 0xfe;
    let p = Platform::from_dtb(&b).unwrap();
    assert_eq!(p.irqs_of("/button").unwrap(), [30]);
    // Line 9 has no map entry.
    let mut b = NEXUS.to_vec();
    let at = prop_value_at(&b, "interrupts", &[0, 0, 0, 1, 0, 0, 0, 8]);
    b[at + 3] = 9;
    assert!(matches!(
        Platform::from_dtb(&b),
        Err(DtbError::NoInterruptMapEntry { ref specifier, .. }) if specifier == &[9, 8]
    ));
    // Shrinking the sub-bus window to 0x100 bytes leaves timer@100 outside.
    let mut b = NEXUS.to_vec();
    let at = prop_value_at(&b, "ranges", &[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0x10, 0]);
    b[at + 8..at + 12].copy_from_slice(&0x100u32.to_be_bytes());
    let p = Platform::from_dtb(&b).unwrap();
    assert!(matches!(
        p.mmio("/soc/apb@400000000/sub@10000/timer@100"),
        Err(DtbError::NoRangesEntry { .. })
    ));
    // A ranges property that is not whole entries: shorten it to 8 bytes
    // and turn the dropped size cell into an FDT_NOP token.
    let mut b = NEXUS.to_vec();
    let at = prop_value_at(&b, "ranges", &[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0x10, 0]);
    b[at - 8..at - 4].copy_from_slice(&8u32.to_be_bytes());
    b[at + 8..at + 12].copy_from_slice(&4u32.to_be_bytes());
    let p = Platform::from_dtb(&b).unwrap();
    assert!(matches!(
        p.mmio("/soc/apb@400000000/sub@10000/timer@100"),
        Err(DtbError::BadProperty { .. })
    ));
}

fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn real_blobs_survive_truncation_and_corruption() {
    for blob in [ACLINT, UNLEASHED, UNMATCHED_V16, VF1, VF2, NEXUS, LOOP] {
        for n in (0..blob.len()).step_by(7) {
            let _ = DeviceTree::parse(&blob[..n]);
        }
        let mut seed = 0x2545_f491_4f6c_dd1du64 ^ blob.len() as u64;
        for _ in 0..1500 {
            let mut b = blob.to_vec();
            for _ in 0..(1 + rng(&mut seed) % 4) {
                let i = (rng(&mut seed) as usize) % b.len();
                b[i] = rng(&mut seed) as u8;
            }
            if let Ok(t) = DeviceTree::parse(&b) {
                for id in 0..t.nodes().len() {
                    let _ = t.reg_translated(id);
                    let _ = t.interrupts(id);
                    let _ = t.pci_windows(id);
                }
                if let Ok(p) = Platform::from_tree(t) {
                    let _ = p.usable_frame_ranges();
                    for l in &p.irq_lines {
                        let _ = p.dma_coherence(&l.device);
                        let _ = p.pci_intx(&l.device, 0, 1, 0, 1);
                    }
                }
            }
        }
    }
}

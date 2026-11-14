//! Devicetree input: real QEMU trees and a hand-written board, feeding the
//! frame-conservation, IrqGuard and DMA models.

use std::cell::RefCell;
use std::rc::Rc;

use vesper_silicon::Arch;
use vesper_silicon::dtb::{
    ControllerKind, DeviceTree, DmaCoherence, DtbError, MemRange, Platform, ReservationSource,
};
use vesper_silicon::frames::{FrameAuditor, PAGE_BYTES};
use vesper_silicon::irq::{Cpu, IsrSpinLock};
use vesper_silicon::ledger::Ledger;

const RV_VIRT: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt.dtb");
const ARM_VIRT: &[u8] = include_bytes!("../fixtures/dtb/qemu-aarch64-virt-gicv3.dtb");
const BOARD: &[u8] = include_bytes!("../fixtures/dtb/vesper-rv-board.dtb");
const EDGES: &[u8] = include_bytes!("../fixtures/dtb/vesper-edge-cases.dtb");

#[test]
fn implicit_parents_disabled_buses_and_versioned_extensions() {
    let p = Platform::from_dtb(EDGES).unwrap();
    // A child of the PLIC without interrupt-parent uses the PLIC.
    let gpio = p
        .irq_lines
        .iter()
        .find(|l| l.device == "/plic@c000000/gpio@c100000")
        .unwrap();
    assert_eq!(
        (gpio.controller.as_str(), gpio.number),
        ("/plic@c000000", 5)
    );
    // Nothing under the disabled bus is extracted (its source 70 would be
    // out of range, and its memory bank would be counted).
    assert!(!p.irq_lines.iter().any(|l| l.device.starts_with("/offbus")));
    assert_eq!(p.memory.len(), 1);
    let ledger = Ledger::new();
    let mut cpu = Cpu::new(p.arch, &ledger);
    assert!(matches!(
        p.install_isr(&mut cpu, "/offbus/dev@20000000", |_| {}),
        Err(DtbError::NoInterrupt { .. })
    ));
    // "zicbom1p0" names Zicbom.
    assert_eq!(
        p.dma_coherence("/plic@c000000/gpio@c100000").unwrap(),
        DmaCoherence::Zicbom { block_bytes: 64 }
    );
    use vesper_silicon::dtb::isa_has_extension;
    assert!(isa_has_extension("rv64gc_zicbom2", "zicbom"));
    assert!(isa_has_extension("rv64gc_zicbom", "zicbom"));
    assert!(!isa_has_extension("rv64gc_zicbomx", "zicbom"));
    assert!(!isa_has_extension("rv64gc_zicbom1p", "zicbom"));
    assert!(!isa_has_extension("rv64gc_zicboz", "zicbom"));
}

#[test]
fn qemu_riscv_virt_memory_harts_and_plic() {
    let p = Platform::from_dtb(RV_VIRT).unwrap();
    assert_eq!(p.arch, Arch::Riscv64);
    assert_eq!(
        p.memory,
        [MemRange {
            base: 0x8000_0000,
            size: 0x1000_0000
        }]
    );
    assert!(p.reservations.is_empty());
    assert_eq!(p.harts.len(), 2);
    assert!(p.harts.iter().all(|h| h.cbom_block_bytes == Some(64)));
    let plic = p
        .controllers
        .iter()
        .find(|c| matches!(c.kind, ControllerKind::Plic { .. }))
        .unwrap();
    assert_eq!(plic.path, "/soc/plic@c000000");
    assert_eq!(plic.kind, ControllerKind::Plic { ndev: 0x5f });
    let harts = p
        .controllers
        .iter()
        .filter(|c| matches!(c.kind, ControllerKind::RiscvHartIntc { .. }))
        .count();
    assert_eq!(harts, 2);
    // The UART is PLIC source 10 and the eight virtio-mmio slots are 1..=8.
    assert_eq!(p.irqs_of("/soc/serial@10000000").unwrap(), [10]);
    let virtio: Vec<u32> = p
        .irq_lines
        .iter()
        .filter(|l| l.device.starts_with("/soc/virtio_mmio@"))
        .map(|l| l.number)
        .collect();
    let mut sorted = virtio.clone();
    sorted.sort();
    assert_eq!(sorted, (1..=8).collect::<Vec<_>>());
    // The PLIC's own outputs go to each hart's S- and M-mode external
    // interrupt (causes 9 and 11); the CLINT's to the timer/software causes.
    let plic_out: Vec<u32> = p
        .irq_lines
        .iter()
        .filter(|l| l.device == "/soc/plic@c000000")
        .map(|l| l.number)
        .collect();
    assert_eq!(plic_out, [11, 9, 11, 9]);
    // 256 MiB of 4 KiB frames, nothing reserved in the raw QEMU tree.
    assert_eq!(p.usable_frame_ranges().unwrap(), [(0x80000, 0x10000)]);
}

#[test]
fn qemu_aarch64_virt_gicv3_spis_and_ppis() {
    let p = Platform::from_dtb(ARM_VIRT).unwrap();
    assert_eq!(p.arch, Arch::Aarch64);
    assert_eq!(
        p.memory,
        [MemRange {
            base: 0x4000_0000,
            size: 0x1000_0000
        }]
    );
    let gic = p
        .controllers
        .iter()
        .find(|c| c.kind == ControllerKind::Gic { v3: true })
        .unwrap();
    assert_eq!(gic.path, "/intc@8000000");
    // PL011 UART: SPI 1 -> INTID 33.
    assert_eq!(p.irqs_of("/pl011@9000000").unwrap(), [33]);
    // Arch timer: four PPIs 13, 14, 11, 10 -> INTIDs 29, 30, 27, 26.
    assert_eq!(p.irqs_of("/timer").unwrap(), [29, 30, 27, 26]);
    // virtio-mmio: dma-coherent on Arm.
    let v = p
        .irq_lines
        .iter()
        .find(|l| l.device.starts_with("/virtio_mmio@"))
        .unwrap();
    assert_eq!(p.dma_coherence(&v.device).unwrap(), DmaCoherence::Coherent);
    // A node without dma-coherent on Arm is non-coherent.
    assert_eq!(
        p.dma_coherence("/pl011@9000000").unwrap(),
        DmaCoherence::ArmNonCoherent
    );
}

#[test]
fn board_reservations_shape_the_frame_allocators() {
    let p = Platform::from_dtb(BOARD).unwrap();
    assert_eq!(p.memory.len(), 2);
    let srcs: Vec<_> = p.reservations.iter().map(|r| &r.source).collect();
    assert_eq!(srcs[0], &ReservationSource::MemReserve);
    assert!(matches!(
        srcs[1],
        ReservationSource::Node { no_map: true, .. }
    ));
    assert!(matches!(
        srcs[2],
        ReservationSource::Node { no_map: false, .. }
    ));
    // Bank 0: 0x80000000 + 4 MiB minus /memreserve/ [0x80000000, +128 KiB),
    // OpenSBI [0x80040000, +256 KiB) and the framebuffer, which starts
    // mid-page at 0x80200800 and so takes two whole pages. Bank 1 is whole.
    let ranges = p.usable_frame_ranges().unwrap();
    assert_eq!(
        ranges,
        [
            (0x80020, 0x20),
            (0x80080, 0x180),
            (0x80202, 0x1fe),
            (0x100000, 0x200)
        ]
    );
    let ledger = Ledger::new();
    let mut allocs = p.frame_allocators(&ledger).unwrap();
    let total: u64 = allocs.iter().map(|a| a.total()).sum();
    assert_eq!(total, 0x20 + 0x180 + 0x1fe + 0x200);
    // Drain every allocator: no frame lands in a reserved range or outside
    // memory, and conservation holds for each allocator.
    let mut held = Vec::new();
    for a in &mut allocs {
        while let Some(f) = a.alloc() {
            let pa = f.phys_addr();
            assert!(
                p.memory
                    .iter()
                    .any(|m| pa >= m.base && pa + PAGE_BYTES <= m.end().unwrap())
            );
            for r in &p.reservations {
                let (lo, hi) = (r.range.base, r.range.end().unwrap());
                assert!(pa + PAGE_BYTES <= lo || pa >= hi, "{pa:#x} in {r:?}");
            }
            held.push(f);
        }
    }
    assert_eq!(held.len() as u64, total);
    // Return each frame to the allocator that owns it.
    for f in held {
        let mut f = Some(f);
        for a in allocs.iter_mut() {
            match a.free(f.take().unwrap()) {
                Ok(()) => break,
                Err((back, _)) => f = Some(back),
            }
        }
        assert!(f.is_none(), "frame without an owner");
    }
    for a in &allocs {
        let audit = FrameAuditor::audit(a, &[], 0).unwrap();
        assert_eq!(audit.free, audit.total);
    }
    assert_eq!(ledger.total_leaked(), 0);

    // A tree claiming more memory than the model's budget is refused
    // before anything is allocated.
    let mut b = BOARD.to_vec();
    let at = find(&b, &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x20, 0, 0]);
    b[at + 8..at + 12].copy_from_slice(&0x100u32.to_be_bytes()); // 1 TiB bank
    let big = Platform::from_dtb(&b).unwrap();
    assert!(matches!(
        big.frame_allocators(&Ledger::new()),
        Err(DtbError::TooManyFrames { .. })
    ));
}

#[test]
fn board_interrupts_install_isrs_on_the_irqguard_model() {
    let p = Platform::from_dtb(BOARD).unwrap();
    assert_eq!(p.irqs_of("/soc/dma@10010000").unwrap(), [5, 6]);
    // The disabled node's out-of-range source 40 is not checked or wired.
    assert!(!p.irq_lines.iter().any(|l| l.number == 40));
    assert!(matches!(
        p.irqs_of("/cpus"),
        Err(DtbError::NoInterrupt { .. })
    ));

    let ledger = Ledger::new();
    let mut cpu = Cpu::new(p.arch, &ledger);
    let lock = Rc::new(IsrSpinLock::new(&cpu, 0u32));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let (l2, s2) = (lock.clone(), seen.clone());
    let uart = p
        .install_isr(&mut cpu, "/soc/serial@10000000", move |ctx| {
            *l2.lock_from_isr(ctx).unwrap() += 1;
            s2.borrow_mut().push(ctx.irq());
        })
        .unwrap();
    assert_eq!(uart, 10);
    // A UART interrupt while thread code holds the ISR-shared lock under an
    // IrqGuard stays pending until the guard is restored.
    let g = cpu.disable_local_save();
    {
        let mut v = lock.lock(&g).unwrap();
        *v += 100;
        cpu.raise(uart);
        assert!(seen.borrow().is_empty());
    }
    assert_eq!(cpu.pending(), 1);
    cpu.restore_local(g).unwrap();
    assert_eq!(*seen.borrow(), [10]);
    let g = cpu.disable_local_save();
    assert_eq!(*lock.lock(&g).unwrap(), 101);
    cpu.restore_local(g).unwrap();
    // No ISR can be bound to a device the tree gives no platform interrupt.
    assert!(matches!(
        p.install_isr(&mut cpu, "/memory@80000000", |_| {}),
        Err(DtbError::NoInterrupt { .. })
    ));
    assert!(matches!(
        p.install_isr(&mut cpu, "/soc/nope", |_| {}),
        Err(DtbError::NoSuchNode(_))
    ));
    assert_eq!(ledger.total_leaked(), 0);
}

#[test]
fn board_dma_coherence_selects_zicbom() {
    let p = Platform::from_dtb(BOARD).unwrap();
    assert_eq!(
        p.dma_coherence("/soc/dma@10010000").unwrap(),
        DmaCoherence::Zicbom { block_bytes: 64 }
    );
    assert_eq!(
        p.dma_coherence("/soc/nic@10020000").unwrap(),
        DmaCoherence::Coherent
    );
    // RISC-V default (no marking) is coherent, as in Linux.
    assert_eq!(
        p.dma_coherence("/soc/serial@10000000").unwrap(),
        DmaCoherence::Coherent
    );
}

// ---- negative and robustness tests -------------------------------------

/// Byte offset of the first occurrence of `needle`.
fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .position(|w| w == needle)
        .expect("needle present")
}

fn patch_u32(blob: &mut [u8], at: usize, v: u32) {
    blob[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

#[test]
fn malformed_headers_are_typed_errors() {
    assert!(matches!(
        DeviceTree::parse(&BOARD[..20]),
        Err(DtbError::Truncated { .. })
    ));
    let mut b = BOARD.to_vec();
    b[0] ^= 1;
    assert!(matches!(DeviceTree::parse(&b), Err(DtbError::BadMagic(_))));
    let mut b = BOARD.to_vec();
    patch_u32(&mut b, 20, 15); // version 15 (16 and 17 are supported)
    assert!(matches!(
        DeviceTree::parse(&b),
        Err(DtbError::UnsupportedVersion { .. })
    ));
    let mut b = BOARD.to_vec();
    patch_u32(&mut b, 4, u32::MAX); // totalsize past the end
    assert!(matches!(
        DeviceTree::parse(&b),
        Err(DtbError::Truncated { .. })
    ));
    let mut b = BOARD.to_vec();
    patch_u32(&mut b, 36, u32::MAX - 8); // size_dt_struct overflows
    assert!(matches!(
        DeviceTree::parse(&b),
        Err(DtbError::BlockOutOfBounds { .. })
    ));
    let mut b = BOARD.to_vec();
    let off = u32::from_be_bytes([b[8], b[9], b[10], b[11]]);
    patch_u32(&mut b, 8, off + 2);
    assert!(matches!(
        DeviceTree::parse(&b),
        Err(DtbError::Misaligned { .. })
    ));
}

#[test]
fn semantic_errors_are_typed() {
    // PLIC source 0x5f is the last valid one; make the UART use 0x60.
    let mut b = RV_VIRT.to_vec();
    let tree = DeviceTree::parse(&b).unwrap();
    let uart = tree.find("/soc/serial@10000000").unwrap();
    assert_eq!(tree.prop_cells(uart, "interrupts").unwrap(), Some(vec![10]));
    // The property value <0x0a> follows its 12-byte header; patch it via the
    // unique byte pattern of the serial node's reg + interrupts.
    let at = find(&b, b"serial@10000000");
    let rest = &b[at..];
    let v = rest.windows(4).position(|w| w == [0, 0, 0, 0x0a]).unwrap();
    patch_u32(&mut b, at + v, 0x60);
    assert!(matches!(
        Platform::from_dtb(&b),
        Err(DtbError::IrqOutOfRange {
            irq: 0x60,
            limit: 0x5f,
            ..
        })
    ));
    // A dangling interrupt-parent phandle.
    let mut b = RV_VIRT.to_vec();
    let at = find(&b, b"serial@10000000");
    let rest = &b[at..];
    let v = rest.windows(4).position(|w| w == [0, 0, 0, 5]).unwrap();
    patch_u32(&mut b, at + v, 0x77);
    assert!(matches!(
        Platform::from_dtb(&b),
        Err(DtbError::UnknownPhandle { phandle: 0x77, .. })
    ));
}

#[test]
fn zicbom_is_required_for_noncoherent_riscv_dma() {
    // Rename the board's "_zicbom" to "_zicbxm" on one hart.
    let mut b = BOARD.to_vec();
    let at = find(&b, b"_zicbom");
    b[at + 5] = b'x';
    let p = Platform::from_dtb(&b).unwrap();
    assert!(matches!(
        p.dma_coherence("/soc/dma@10010000"),
        Err(DtbError::NoCacheBlockManagement { .. })
    ));
    // Coherent devices do not need it.
    assert_eq!(
        p.dma_coherence("/soc/nic@10020000").unwrap(),
        DmaCoherence::Coherent
    );
}

/// Deterministic xorshift for the robustness sweep.
fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn truncations_and_byte_flips_never_panic() {
    for blob in [RV_VIRT, ARM_VIRT, BOARD] {
        for n in 0..blob.len() {
            let _ = DeviceTree::parse(&blob[..n]);
        }
        let mut seed = 0x9e37_79b9_7f4a_7c15u64 ^ blob.len() as u64;
        for _ in 0..3000 {
            let mut b = blob.to_vec();
            for _ in 0..(1 + rng(&mut seed) % 4) {
                let i = (rng(&mut seed) as usize) % b.len();
                b[i] = rng(&mut seed) as u8;
            }
            if let Ok(t) = DeviceTree::parse(&b) {
                let _ = Platform::from_tree(t).map(|p| {
                    let _ = p.usable_frame_ranges();
                    for l in &p.irq_lines {
                        let _ = p.dma_coherence(&l.device);
                    }
                });
            }
        }
    }
}

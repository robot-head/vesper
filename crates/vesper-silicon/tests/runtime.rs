//! Typestate MMIO, IrqGuard, DMA and frame-conservation protocol models.

use std::cell::Cell;
use std::rc::Rc;

use vesper_silicon::Arch;
use vesper_silicon::dma::*;
use vesper_silicon::frames::*;
use vesper_silicon::irq::*;
use vesper_silicon::ledger::{Ledger, OwnerKind};
use vesper_silicon::mmio::*;
use vesper_silicon::svd::{DeviceModel, RegisterClass};

fn bus() -> Bus {
    SimBus::new(&DeviceModel::from_svd(include_str!("../fixtures/vsim32.svd")).unwrap())
}

#[test]
fn w1c_ack_preserves_concurrent_pending_bits() {
    let b = bus();
    let irq = Reg::<Write1ToClear>::bind(&b, "ETHMAC.IRQ_STATUS").unwrap();
    b.borrow_mut().hw_set("ETHMAC.IRQ_STATUS", 0b1011).unwrap();
    irq.write_clear_mask(1 << 0).unwrap();
    assert_eq!(irq.read(), 0b1010, "only RX_DONE acknowledged");
    assert!(matches!(
        irq.write_clear_mask(1 << 7),
        Err(BusError::MaskNotWriteClear { .. })
    ));
    // What an RMW would have done on the same hardware model: clears all.
    let rw_view = b.borrow_mut().peek("ETHMAC.IRQ_STATUS").unwrap();
    assert_eq!(rw_view, 0b1010);
}

#[test]
fn w1c_ack_keeps_plain_bits_in_mixed_registers() {
    let b = bus();
    let sr = Reg::<Write1ToClear>::bind(&b, "SPI1.SR").unwrap();
    b.borrow_mut().hw_set("SPI1.SR", 0b1_0001).unwrap(); // CRCERR + RXNE
    sr.write_clear_mask(sr.field_mask("CRCERR").unwrap())
        .unwrap();
    assert_eq!(sr.read(), 0b0011, "CRCERR cleared, RXNE/TXE untouched");
}

#[test]
fn read_to_clear_is_consumed() {
    let b = bus();
    let rx = Reg::<ReadToClear>::bind(&b, "UART0.RXDATA").unwrap();
    b.borrow_mut().hw_set("UART0.RXDATA", 0x41).unwrap();
    assert_eq!(rx.consume(), 0x41);
    assert_eq!(rx.consume(), 0);
}

#[test]
fn binding_checks_the_model_class() {
    let b = bus();
    let e = Reg::<ReadWrite>::bind(&b, "ETHMAC.IRQ_STATUS").unwrap_err();
    assert_eq!(
        e,
        BusError::ClassMismatch {
            register: "ETHMAC.IRQ_STATUS".into(),
            model: RegisterClass::WriteClear,
            requested: RegisterClass::ReadWrite
        }
    );
    assert!(Reg::<ReadWrite>::bind(&b, "UART0.RXDATA").is_err());
    assert!(Reg::<ReadOnly>::bind(&b, "ETHMAC.MAC_ID").unwrap().read() == 0x5645_5350);
    let tx = Reg::<WriteOnly>::bind(&b, "UART0.TXDATA").unwrap();
    tx.write(0x42).unwrap();
    // Read-only bits ignore writes, write-1-to-set bits latch.
    let ctrl = Reg::<ReadWrite>::bind(&b, "UART0.CTRL").unwrap();
    ctrl.write(0x1f1).unwrap();
    assert_eq!(ctrl.read(), 0x101);
    assert!(matches!(
        Reg::<ReadOnly>::bind(&b, "NOPE.X"),
        Err(BusError::UnknownRegister(_))
    ));
}

#[test]
fn spi_typestate_and_singletons() {
    let b = bus();
    let spi = Spi::<Disabled>::take(&b, "SPI1").unwrap();
    assert!(matches!(
        Spi::<Disabled>::take(&b, "SPI1"),
        Err(BusError::AlreadyTaken(_))
    ));
    // SPI2 was derived from SPI1 in the SVD and binds the same way.
    let _spi2 = Spi::<Disabled>::take(&b, "SPI2").unwrap();

    let (spi, e) = spi.into_master(8).unwrap_err();
    assert_eq!(e, SpiError::InvalidBaudDivider(8));
    let mut spi = spi.into_master(3).unwrap();
    let cr1 = b.borrow_mut().peek("SPI1.CR1").unwrap();
    assert_eq!(cr1, (1 << 6) | (3 << 3) | (1 << 2), "SPE | BR=3 | MSTR");
    // No received byte yet: typed error, not success.
    assert_eq!(spi.transfer(0x5a), Err(SpiError::RxEmpty));
    b.borrow_mut().hw_set("SPI1.SR", 1).unwrap();
    assert_eq!(spi.transfer(0xa5), Ok(0xa5), "loopback through DR");
    b.borrow_mut().hw_clear("SPI1.SR", 0b10).unwrap();
    assert_eq!(spi.transfer(1), Err(SpiError::TxNotEmpty));
    let spi = spi.disable();
    assert_eq!(b.borrow_mut().peek("SPI1.CR1").unwrap() & (1 << 6), 0);
    assert_eq!(spi.name(), "SPI1");
}

#[test]
fn irq_guard_masks_and_restores_per_arch() {
    for arch in [Arch::Aarch64, Arch::X86_64, Arch::Riscv64] {
        let ledger = Ledger::new();
        let mut cpu = Cpu::new(arch, &ledger);
        let before = cpu.psr();
        assert!(cpu.interrupts_enabled());
        let outer = cpu.disable_local_save();
        assert!(!cpu.interrupts_enabled());
        assert_eq!(outer.saved_psr(), before);
        let inner = cpu.disable_local_save();
        // Out-of-order restore returns the guard.
        let (outer, e) = cpu.restore_local(outer).unwrap_err();
        assert!(matches!(e, IrqError::OutOfOrder { .. }));
        cpu.restore_local(inner).unwrap();
        assert!(
            !cpu.interrupts_enabled(),
            "{arch:?}: outer guard still masks"
        );
        cpu.restore_local(outer).unwrap();
        assert!(cpu.interrupts_enabled());
        assert_eq!(cpu.psr(), before);
        assert_eq!(ledger.live(OwnerKind::IrqGuard), 0);
        assert_eq!(ledger.total_leaked(), 0);
    }
}

#[test]
fn irq_masked_while_lock_held_defers_the_isr() {
    let ledger = Ledger::new();
    let mut cpu = Cpu::new(Arch::Aarch64, &ledger);
    let stats = Rc::new(IsrSpinLock::new(&cpu, 0u32));
    let isr_saw = Rc::new(Cell::new(None));
    {
        let stats = stats.clone();
        let isr_saw = isr_saw.clone();
        cpu.register_isr(42, move |ctx| {
            let mut s = stats
                .lock_from_isr(ctx)
                .expect("never contended on this core");
            *s += 100;
            isr_saw.set(Some(*s));
        });
    }
    let guard = cpu.disable_local_save();
    {
        let mut s = stats.lock(&guard).unwrap();
        *s += 1;
        // Device raises the IRQ while the lock is held: stays pending.
        cpu.raise(42);
        assert_eq!(cpu.pending(), 1);
        assert_eq!(isr_saw.get(), None);
        assert_eq!(stats.lock(&guard).err(), Some(LockError::WouldDeadlock));
    }
    cpu.restore_local(guard).unwrap();
    assert_eq!(cpu.delivered(), [42]);
    assert_eq!(isr_saw.get(), Some(101));
    // Unmasked: delivered immediately.
    cpu.raise(42);
    assert_eq!(isr_saw.get(), Some(201));
    // A guard from another core is not a proof for this lock.
    let mut other = Cpu::new(Arch::Aarch64, &ledger);
    let g = other.disable_local_save();
    assert_eq!(stats.lock(&g).err(), Some(LockError::WrongCpu));
    assert!(matches!(
        cpu.restore_local(g),
        Err((_, IrqError::WrongCpu { .. }))
    ));
}

#[test]
fn dropped_linear_owners_are_counted_leaks() {
    let ledger = Ledger::new();
    let mut cpu = Cpu::new(Arch::X86_64, &ledger);
    drop(cpu.disable_local_save());
    assert_eq!(ledger.leaked(OwnerKind::IrqGuard), 1);
    let mut alloc = FrameAllocator::new(0x100, 4, &ledger).unwrap();
    drop(alloc.alloc().unwrap());
    assert!(matches!(
        FrameAuditor::audit(&alloc, &[], 0),
        Err(AuditError::Leaked { frames: 1, .. })
    ));
}

#[test]
fn dma_barriers_per_arch() {
    assert!(
        dma_barriers(Arch::X86_64, DmaPhase::BeforeDevice, 0x1000, 512, 64)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        dma_barriers(Arch::Riscv64, DmaPhase::AfterDeviceWrite, 0x1000, 512, 64).unwrap(),
        [BarrierInst::FenceIorwIorw]
    );
    let a = dma_barriers(Arch::Aarch64, DmaPhase::BeforeDevice, 0x1030, 64, 64).unwrap();
    // Unaligned 64 bytes straddle two lines.
    assert_eq!(
        a,
        [
            BarrierInst::DcCvac(0x1000),
            BarrierInst::DcCvac(0x1040),
            BarrierInst::DsbSy
        ]
    );
    let text: Vec<String> = a.iter().map(ToString::to_string).collect();
    assert_eq!(text, ["dc cvac, 0x1000", "dc cvac, 0x1040", "dsb sy"]);
    let inv = dma_barriers(Arch::Aarch64, DmaPhase::AfterDeviceWrite, 0x2000, 128, 64).unwrap();
    assert_eq!(
        inv,
        [
            BarrierInst::DcIvac(0x2000),
            BarrierInst::DcIvac(0x2040),
            BarrierInst::DsbSy
        ]
    );
    assert_eq!(
        dma_barriers(Arch::Aarch64, DmaPhase::BeforeDevice, 0, 1, 48),
        Err(DmaError::BadLineSize(48))
    );
    assert_eq!(
        dma_barriers(Arch::Aarch64, DmaPhase::BeforeDevice, u64::MAX, 2, 64),
        Err(DmaError::RegionOverflow)
    );
}

#[test]
fn descriptor_wire_layout_is_16_bytes() {
    let d = SgDmaDescWire {
        phys_addr: 0x1122_3344_5566_7788,
        byte_len: 0x99aa_bbcc,
        flags: 0xddee,
        next_idx: 0x0102,
    };
    let b = d.to_bytes();
    assert_eq!(SgDmaDescWire::SIZE, 16);
    assert_eq!(&b[..8], &0x1122_3344_5566_7788u64.to_le_bytes());
    assert_eq!(&b[14..], &[0x02, 0x01]);
    assert_eq!(SgDmaDescWire::from_bytes(&b), Ok(d));
    assert_eq!(
        SgDmaDescWire::from_bytes(&b[..15]),
        Err(DmaError::BadDescriptorLength(15))
    );
}

#[test]
fn dma_transfer_owns_the_buffer_until_completion() {
    let ledger = Ledger::new();
    let mut dma = DmaEngine::new(Arch::Aarch64, 2, 8, 64, &ledger).unwrap();
    let buf = StaticDmaBuf::<512>::new(0x2000_0000);
    let xfer = dma.submit_rx(0, buf).unwrap();
    let d = dma.descriptor(xfer.desc_slot()).unwrap();
    assert_eq!(
        (d.phys_addr, d.byte_len, d.flags),
        (0x2000_0000, 512, SgDmaDescWire::FLAG_DEVICE_WRITES)
    );
    assert_eq!(dma.barriers().len(), 512 / 64 + 1);
    // Busy channel returns the buffer.
    let (back, e) = dma
        .submit_rx(0, StaticDmaBuf::<64>::new(0x3000))
        .unwrap_err();
    assert_eq!(e, DmaError::ChannelBusy(0));
    assert_eq!(back.dma_region(), (0x3000, 64));
    // Not complete yet: the transfer comes back.
    let (xfer, e) = dma.poll_complete(xfer).unwrap_err();
    assert_eq!(e, DmaError::NotComplete);
    dma.device_complete(0, 600, None).unwrap();
    let (res, buf) = dma.poll_complete(xfer).unwrap();
    assert_eq!(
        res,
        Err(DmaError::DeviceOverrun {
            reported: 600,
            capacity: 512
        })
    );
    assert_eq!(dma.barriers().last(), Some(&BarrierInst::DsbSy));
    assert!(dma.barriers().contains(&BarrierInst::DcIvac(0x2000_0000)));
    let xfer = dma.submit_rx(0, buf).unwrap();
    dma.device_complete(0, 60, None).unwrap();
    let (res, buf) = dma.poll_complete(xfer).unwrap();
    assert_eq!(res, Ok(60));
    assert_eq!(buf.prefix(60).len(), 60);
    assert_eq!(ledger.live(OwnerKind::DmaTransfer), 0);
    assert!(matches!(
        dma.submit_tx(9, buf),
        Err((_, DmaError::NoSuchChannel(9)))
    ));
}

#[test]
fn transfers_are_bound_to_their_engine_and_slot() {
    let ledger = Ledger::new();
    let mut a = DmaEngine::new(Arch::Aarch64, 2, 1, 64, &ledger).unwrap();
    let mut b = DmaEngine::new(Arch::Aarch64, 2, 1, 64, &ledger).unwrap();
    let xa = a.submit_rx(0, StaticDmaBuf::<64>::new(0x1000)).unwrap();
    let xb = b.submit_rx(0, StaticDmaBuf::<64>::new(0x2000)).unwrap();
    // One ring slot: a second in-flight transfer on another channel must
    // not overwrite the first descriptor.
    let (buf, e) = a.submit_rx(1, StaticDmaBuf::<64>::new(0x3000)).unwrap_err();
    assert_eq!(e, DmaError::RingFull);
    assert_eq!(a.descriptor(xa.desc_slot()).unwrap().phys_addr, 0x1000);
    b.device_complete(0, 8, None).unwrap();
    let (xa, e) = b.poll_complete(xa).unwrap_err();
    assert_eq!(e, DmaError::ForeignTransfer);
    let (res, _) = b.poll_complete(xb).unwrap();
    assert_eq!(res, Ok(8));
    a.device_complete(0, 4, None).unwrap();
    let (res, _) = a.poll_complete(xa).unwrap();
    assert_eq!(res, Ok(4));
    // The slot is free again.
    let x = a.submit_rx(1, buf).unwrap();
    a.device_complete(1, 0, None).unwrap();
    a.poll_complete(x).unwrap();
    assert_eq!(ledger.total_leaked(), 0);
}

#[test]
fn unmap_requires_shootdown_before_reuse() {
    let ledger = Ledger::new();
    let mut alloc = FrameAllocator::new(0x80, 8, &ledger).unwrap();
    let mut pt = PageTable::new(7, &ledger);
    let mut smp = SmpIpi::new(Arch::Aarch64, 4);
    let f = alloc.alloc().unwrap();
    let pfn = f.pfn();
    pt.map(0x40_0000, f).unwrap();
    smp.note_cached(2, 7, 0x40_0000);
    assert_eq!(pt.translate(0x40_0123), Some(pfn * 4096 + 0x123));
    let sd = pt.unmap_leaf_4k(0x40_0000).unwrap();
    assert_eq!(pt.translate(0x40_0000), None);
    assert!(
        smp.is_cached_anywhere(7, 0x40_0000),
        "stale until shootdown"
    );
    let a = FrameAuditor::audit(&alloc, &[&pt], 0).unwrap();
    assert_eq!((a.pending_shootdown, a.held, a.mapped), (1, 0, 0));
    let frame = sd.flush_ipi_and_reclaim(&mut smp);
    assert!(!smp.is_cached_anywhere(7, 0x40_0000));
    let (_, asid, seq) = &smp.log()[0];
    assert_eq!(*asid, 7);
    assert_eq!(
        seq,
        &[
            TlbInst::DsbIshst,
            TlbInst::TlbiVae1is((7u64 << 48) | 0x400),
            TlbInst::DsbIsh,
            TlbInst::Isb
        ]
    );
    alloc.free(frame).unwrap();
    assert_eq!(FrameAuditor::audit(&alloc, &[&pt], 0).unwrap().free, 8);
    assert_eq!(
        tlb_invalidate_sequence(Arch::X86_64, 0x1000, 1),
        [TlbInst::Invlpg(0x1000)]
    );
    assert_eq!(
        tlb_invalidate_sequence(Arch::Riscv64, 0x1000, 3),
        [TlbInst::SfenceVma {
            vaddr: 0x1000,
            asid: 3
        }]
    );
    // Typed errors return the frame.
    let f = alloc.alloc().unwrap();
    let (f, e) = pt.map(0x123, f).unwrap_err();
    assert_eq!(e, VmError::Unaligned { vaddr: 0x123 });
    let other = Ledger::new();
    let mut foreign = FrameAllocator::new(0x80, 8, &other).unwrap();
    let (f, e) = foreign.free(f).unwrap_err();
    assert_eq!(e, FrameError::ForeignLedger);
    alloc.free(f).unwrap();
    assert!(matches!(
        pt.unmap_leaf_4k(0x9000),
        Err(VmError::NotMapped { .. })
    ));
    assert!(FrameAllocator::new(u64::MAX, 2, &ledger).is_err());
}

/// Deterministic xorshift so the property test is reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn randomized_map_unmap_dma_sequences_conserve_frames() {
    for seed in 1..=64u64 {
        let ledger = Ledger::new();
        let total = 24;
        let mut alloc = FrameAllocator::new(0x1000, total, &ledger).unwrap();
        let mut tables = [PageTable::new(1, &ledger), PageTable::new(2, &ledger)];
        let arch = [Arch::X86_64, Arch::Aarch64, Arch::Riscv64][(seed % 3) as usize];
        let mut smp = SmpIpi::new(arch, 4);
        let mut dma = DmaEngine::new(arch, 3, 16, 64, &ledger).unwrap();
        let mut held: Vec<PhysFrame4K> = Vec::new();
        let mut pending: Vec<PendingTlbShootdown> = Vec::new();
        let mut inflight: Vec<DmaTransfer<PhysFrame4K>> = Vec::new();
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        for _ in 0..600 {
            match rng.below(8) {
                0 => {
                    if let Some(f) = alloc.alloc() {
                        held.push(f);
                    }
                }
                1 if !held.is_empty() => {
                    let f = held.swap_remove(rng.below(held.len()));
                    alloc.free(f).unwrap();
                }
                2 if !held.is_empty() => {
                    let f = held.swap_remove(rng.below(held.len()));
                    let t = rng.below(2);
                    let va = (rng.below(32) as u64) * PAGE_BYTES;
                    if let Err((f, e)) = tables[t].map(va, f) {
                        assert_eq!(e, VmError::AlreadyMapped { vaddr: va });
                        held.push(f);
                    }
                }
                3 => {
                    let t = rng.below(2);
                    let va = (rng.below(32) as u64) * PAGE_BYTES;
                    match tables[t].unmap_leaf_4k(va) {
                        Ok(sd) => pending.push(sd),
                        Err(e) => assert_eq!(e, VmError::NotMapped { vaddr: va }),
                    }
                }
                4 if !pending.is_empty() => {
                    let sd = pending.swap_remove(rng.below(pending.len()));
                    held.push(sd.flush_ipi_and_reclaim(&mut smp));
                }
                5 if !held.is_empty() => {
                    let f = held.swap_remove(rng.below(held.len()));
                    let ch = rng.below(3) as u8;
                    match dma.submit_rx(ch, f) {
                        Ok(x) => inflight.push(x),
                        Err((f, e)) => {
                            assert_eq!(e, DmaError::ChannelBusy(ch));
                            held.push(f);
                        }
                    }
                }
                6 if !inflight.is_empty() => {
                    let ch = inflight[rng.below(inflight.len())].channel();
                    let _ = dma.device_complete(ch, (rng.below(5000)) as u32, None);
                }
                7 if !inflight.is_empty() => {
                    let x = inflight.swap_remove(rng.below(inflight.len()));
                    match dma.poll_complete(x) {
                        Ok((_, f)) => held.push(f),
                        Err((x, e)) => {
                            assert_eq!(e, DmaError::NotComplete);
                            inflight.push(x)
                        }
                    }
                }
                _ => {}
            }
            let a = FrameAuditor::audit(&alloc, &[&tables[0], &tables[1]], inflight.len() as u64)
                .unwrap_or_else(|e| panic!("seed {seed}: {e:?}"));
            assert_eq!(
                a.total,
                a.free + a.mapped + a.pending_shootdown + a.in_dma + a.held
            );
            assert_eq!(a.held, held.len() as u64, "seed {seed}");
            assert_eq!(a.pending_shootdown, pending.len() as u64);
            // No frame is owned twice.
            let mut pfns: Vec<u64> = held.iter().map(PhysFrame4K::pfn).collect();
            pfns.extend(pending.iter().map(PendingTlbShootdown::pfn));
            pfns.extend(tables.iter().flat_map(|t| t.mapped_pfns()));
            let n = pfns.len();
            pfns.sort_unstable();
            pfns.dedup();
            assert_eq!(pfns.len(), n, "seed {seed}: duplicate frame owner");
        }
        // Drain everything back; the allocator ends full with no leaks.
        for x in inflight.drain(..) {
            dma.device_complete(x.channel(), 0, None).ok();
            let (_, f) = dma.poll_complete(x).unwrap();
            held.push(f);
        }
        for sd in pending.drain(..) {
            held.push(sd.flush_ipi_and_reclaim(&mut smp));
        }
        for t in &mut tables {
            let vas: Vec<u64> = (0..32).map(|i| i * PAGE_BYTES).collect();
            for va in vas {
                if let Ok(sd) = t.unmap_leaf_4k(va) {
                    held.push(sd.flush_ipi_and_reclaim(&mut smp));
                }
            }
        }
        for f in held.drain(..) {
            alloc.free(f).unwrap();
        }
        let a = FrameAuditor::audit(&alloc, &[&tables[0], &tables[1]], 0).unwrap();
        assert_eq!(a.free, total);
        assert_eq!(ledger.total_leaked(), 0);
    }
}

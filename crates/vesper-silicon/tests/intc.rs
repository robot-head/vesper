//! PLIC and CLINT / ACLINT emulation configured from real devicetrees,
//! driving the IrqGuard `Cpu` model.

use std::cell::RefCell;
use std::rc::Rc;

use vesper_silicon::dtb::Platform;
use vesper_silicon::intc::{
    Delivery, IntcError, IntcStats, MEIP, MSIP, MTIP, Privilege, RiscvIntc, SEIP, SSIP,
    STORM_BUDGET, local_causes,
};
use vesper_silicon::irq::IsrSpinLock;
use vesper_silicon::ledger::Ledger;

const RV_VIRT: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt.dtb");
const ACLINT: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aclint.dtb");
const ARM_VIRT: &[u8] = include_bytes!("../fixtures/dtb/qemu-aarch64-virt-gicv3.dtb");
const UNLEASHED: &[u8] = include_bytes!("../fixtures/dtb/hifive-unleashed-a00.dtb");
const VF2: &[u8] = include_bytes!("../fixtures/dtb/jh7110-starfive-visionfive-2-v1.3b.dtb");

const UART: &str = "/soc/serial@10000000";
const PLIC: u64 = 0x0c00_0000;

fn ext(hart: u32, source: u32) -> Delivery {
    Delivery {
        hart,
        cause: MEIP,
        source,
    }
}

/// An ISR that records the source and drops the device's line.
fn acking(
    seen: &Rc<RefCell<Vec<(u32, u32)>>>,
) -> impl FnMut(&vesper_silicon::intc::IrqEvent<'_>) + use<> {
    let seen = seen.clone();
    move |ev| {
        seen.borrow_mut().push((ev.hart, ev.source));
        ev.intc.set_line(ev.source, false).unwrap();
    }
}

#[test]
fn qemu_virt_uart_through_the_plic_respects_the_irqguard() {
    let p = Platform::from_dtb(RV_VIRT).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    assert_eq!(intc.hart_ids(), [0, 1]);
    // QEMU virt: contexts (hart0 M, hart0 S, hart1 M, hart1 S).
    assert_eq!(intc.context_of(0).unwrap(), 0);
    assert_eq!(intc.context_of(1).unwrap(), 2);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let src = intc.install_isr(&p, UART, 0, 1, acking(&seen)).unwrap();
    assert_eq!(src, 10);
    // The enable went through the PLIC registers.
    assert_eq!(intc.read32(PLIC + 4 * 10).unwrap(), 1);
    assert_eq!(intc.read32(PLIC + 0x2000).unwrap(), 1 << 10);

    intc.set_device_line(&p, UART, true).unwrap();
    assert_eq!(*seen.borrow(), [(0, 10)]);
    assert_eq!(intc.deliveries(), [ext(0, 10)]);
    assert_eq!(intc.read32(PLIC + 0x1000).unwrap(), 0, "nothing pending");

    // Masked by an IrqGuard: the PLIC output stays asserted, the hart holds
    // MEIP pending until the guard is restored.
    let g = intc.cpu(0).unwrap().disable_local_save();
    intc.set_line(10, true).unwrap();
    assert_eq!(seen.borrow().len(), 1);
    assert_eq!(intc.cpu(0).unwrap().pending(), 1);
    assert_eq!(intc.read32(PLIC + 0x1000).unwrap(), 1 << 10);
    intc.cpu(0).unwrap().restore_local(g).unwrap();
    assert_eq!(*seen.borrow(), [(0, 10), (0, 10)]);
    assert_eq!(intc.cpu(0).unwrap().delivered(), [MEIP, MEIP]);
    assert_eq!(intc.stats(), IntcStats::default());
    assert_eq!(ledger.total_leaked(), 0);
}

#[test]
fn claim_order_threshold_and_isr_shared_locks() {
    let p = Platform::from_dtb(RV_VIRT).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    // virtio-mmio slot 0 is source 1; the UART source 10.
    let virtio = p
        .irq_lines
        .iter()
        .find(|l| l.number == 1)
        .unwrap()
        .device
        .clone();
    intc.install_isr(&p, UART, 0, 1, acking(&seen)).unwrap();
    intc.install_isr(&p, &virtio, 0, 5, acking(&seen)).unwrap();
    // Both raised while masked: the higher priority is claimed first, then
    // the lower one in the same delivery.
    let g = intc.cpu(0).unwrap().disable_local_save();
    intc.set_line(10, true).unwrap();
    intc.set_line(1, true).unwrap();
    intc.cpu(0).unwrap().restore_local(g).unwrap();
    assert_eq!(*seen.borrow(), [(0, 1), (0, 10)]);
    // Equal priorities go to the lower source id.
    intc.write32(PLIC + 4, 1).unwrap();
    let g = intc.cpu(0).unwrap().disable_local_save();
    intc.set_line(10, true).unwrap();
    intc.set_line(1, true).unwrap();
    intc.cpu(0).unwrap().restore_local(g).unwrap();
    assert_eq!(seen.borrow()[2..], [(0, 1), (0, 10)]);
    // Threshold 3 (context 0 = hart 0 M) blocks priority 1 and lets 5 in.
    intc.write32(PLIC + 4, 5).unwrap();
    intc.write32(PLIC + 0x20_0000, 3).unwrap();
    intc.set_line(10, true).unwrap();
    assert_eq!(seen.borrow().len(), 4);
    assert_eq!(
        intc.read32(PLIC + 0x1000).unwrap(),
        1 << 10,
        "still pending"
    );
    intc.set_line(1, true).unwrap();
    assert_eq!(seen.borrow()[4], (0, 1));
    // Dropping the threshold delivers the waiting UART.
    intc.write32(PLIC + 0x20_0000, 0).unwrap();
    assert_eq!(seen.borrow()[5], (0, 10));
    // The threshold gates notification, not claims: a driver that sets it
    // to 7 can still poll the claim register for a priority-1 source.
    intc.write32(PLIC + 4, 1).unwrap();
    intc.write32(PLIC + 0x20_0000, 7).unwrap();
    intc.set_line(1, true).unwrap();
    assert_eq!(seen.borrow().len(), 6, "no notification");
    assert_eq!(intc.read32(PLIC + 0x20_0004).unwrap(), 1, "but claimable");
    intc.handle().set_line(1, false).unwrap();
    intc.write32(PLIC + 0x20_0004, 1).unwrap();
    intc.write32(PLIC + 0x20_0000, 0).unwrap();
    // Priority 0 never interrupts and is never claimed.
    intc.write32(PLIC + 4, 0).unwrap();
    intc.set_line(1, true).unwrap();
    assert_eq!(intc.read32(PLIC + 0x20_0004).unwrap(), 0);
    intc.set_line(1, false).unwrap();

    // An ISR shares a counter with thread code through an IsrSpinLock: the
    // thread holds it under an IrqGuard while the device fires.
    let lock = Rc::new(IsrSpinLock::new(intc.cpu(0).unwrap(), 0u32));
    let l2 = lock.clone();
    intc.install_isr(&p, UART, 0, 2, move |ev| {
        *l2.lock_from_isr(ev.ctx).unwrap() += 1;
        ev.intc.set_line(ev.source, false).unwrap();
    })
    .unwrap();
    let g = intc.cpu(0).unwrap().disable_local_save();
    {
        let mut v = lock.lock(&g).unwrap();
        *v += 100;
        intc.set_line(10, true).unwrap();
    }
    intc.cpu(0).unwrap().restore_local(g).unwrap();
    let g = intc.cpu(0).unwrap().disable_local_save();
    assert_eq!(*lock.lock(&g).unwrap(), 101);
    intc.cpu(0).unwrap().restore_local(g).unwrap();
    assert_eq!(ledger.total_leaked(), 0);
}

#[test]
fn level_semantics_storms_spurious_claims_and_routing() {
    let p = Platform::from_dtb(RV_VIRT).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    // An ISR that never quiets its device: the line stays high, the source
    // is forwarded again after every completion.
    let runs = Rc::new(RefCell::new(0));
    let r2 = runs.clone();
    intc.install_isr(&p, UART, 1, 1, move |_| *r2.borrow_mut() += 1)
        .unwrap();
    intc.set_line(10, true).unwrap();
    assert_eq!(*runs.borrow(), STORM_BUDGET);
    assert_eq!(intc.stats().storms, 1);
    // Delivered on hart 1 only (enabled in context 2 alone).
    assert!(intc.deliveries().iter().all(|d| d.hart == 1));
    assert!(intc.cpu(0).unwrap().delivered().is_empty());
    intc.set_line(10, false).unwrap();

    // Enabled on both harts: the first claim wins, the second hart's claim
    // returns 0 (spurious), as on hardware.
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    intc.install_isr(&p, UART, 0, 1, acking(&seen)).unwrap();
    intc.install_isr(&p, UART, 1, 1, acking(&seen)).unwrap();
    intc.set_line(10, true).unwrap();
    assert_eq!(seen.borrow().len(), 1);
    assert_eq!(intc.stats().spurious, 1);

    // A completion for a source the context has not enabled is ignored:
    // the source stays in service and is not forwarded again.
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    intc.install_isr(&p, UART, 0, 1, |_| {}).unwrap();
    let h = intc.handle();
    let g = intc.cpu(0).unwrap().disable_local_save();
    intc.set_line(10, true).unwrap();
    assert_eq!(h.read32(PLIC + 0x20_0004).unwrap(), 10, "claimed by hand");
    h.write32(PLIC + 0x20_1004, 10).unwrap(); // context 1: not enabled there
    assert_eq!(h.read32(PLIC + 0x1000).unwrap(), 0);
    h.write32(PLIC + 0x20_0004, 10).unwrap(); // context 0: completes
    assert_eq!(h.read32(PLIC + 0x1000).unwrap(), 1 << 10, "line still high");
    h.set_line(10, false).unwrap();
    assert_eq!(h.read32(PLIC + 0x20_0004).unwrap(), 10);
    h.write32(PLIC + 0x20_0004, 10).unwrap();
    intc.cpu(0).unwrap().restore_local(g).unwrap();
    // MEIP was pending, but the source is gone: a spurious delivery.
    assert_eq!(intc.stats().spurious, 1);
    assert_eq!(ledger.total_leaked(), 0);
}

#[test]
fn plic_register_file_errors() {
    let p = Platform::from_dtb(RV_VIRT).unwrap();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &Ledger::new()).unwrap();
    // Priorities keep 3 bits; source 0 is hard-wired to 0.
    intc.write32(PLIC + 4 * 3, 0xff).unwrap();
    assert_eq!(intc.read32(PLIC + 4 * 3).unwrap(), 7);
    intc.write32(PLIC, 5).unwrap();
    assert_eq!(intc.read32(PLIC).unwrap(), 0);
    intc.write32(PLIC + 0x2000, u32::MAX).unwrap();
    assert_eq!(intc.read32(PLIC + 0x2000).unwrap(), !1);
    // ndev = 95: source 96's priority, pending word 3 and context 4 do
    // not exist.
    for addr in [
        PLIC + 4 * 96,
        PLIC + 0x1000 + 12,
        PLIC + 0x20_4000,
        PLIC + 0x2000 + 0x80 * 4,
    ] {
        assert_eq!(intc.read32(addr), Err(IntcError::Unmapped { addr }));
    }
    assert_eq!(
        intc.read32(PLIC + 2),
        Err(IntcError::Misaligned { addr: PLIC + 2 })
    );
    assert_eq!(
        intc.write32(PLIC + 0x1000, 1),
        Err(IntcError::ReadOnly {
            addr: PLIC + 0x1000
        })
    );
    assert_eq!(intc.set_line(96, true), Err(IntcError::NoSuchSource(96)));
    assert_eq!(intc.set_line(0, true), Err(IntcError::NoSuchSource(0)));
    assert!(matches!(
        intc.install_isr(&p, UART, 0, 8, |_| {}),
        Err(IntcError::PriorityTooHigh(8))
    ));
    assert!(matches!(
        intc.install_isr(&p, "/memory@80000000", 0, 1, |_| {}),
        Err(IntcError::NotOnPlic { .. })
    ));
    assert!(matches!(
        intc.install_isr(&p, UART, 7, 1, |_| {}),
        Err(IntcError::NoContext { hart: 7, .. })
    ));
    assert!(matches!(
        RiscvIntc::new(
            &Platform::from_dtb(ARM_VIRT).unwrap(),
            Privilege::Machine,
            &Ledger::new()
        ),
        Err(IntcError::NotRiscv(_))
    ));
}

#[test]
fn sifive_clint_timer_and_software_interrupts() {
    let p = Platform::from_dtb(RV_VIRT).unwrap();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &Ledger::new()).unwrap();
    assert_eq!(intc.mtime_addr().unwrap(), 0x200_bff8);
    let cmp0 = intc.local_addr(0, MTIP).unwrap();
    let cmp1 = intc.local_addr(1, MTIP).unwrap();
    assert_eq!((cmp0, cmp1), (0x200_4000, 0x200_4008));
    let msip1 = intc.local_addr(1, MSIP).unwrap();
    assert_eq!(msip1, 0x200_0004);

    // A periodic tick every 100 ticks on hart 0.
    let ticks = Rc::new(RefCell::new(Vec::new()));
    let t2 = ticks.clone();
    intc.on_timer(0, move |ev| {
        let now = ev.intc.mtime(ev.hart);
        t2.borrow_mut().push(now);
        let next = ev.intc.read64(cmp0).unwrap() + 100;
        ev.intc.write64(cmp0, next).unwrap();
    })
    .unwrap();
    // MTIMECMP starts at the maximum: nothing fires.
    intc.advance_mtime(1_000_000);
    assert!(ticks.borrow().is_empty());
    intc.write64(intc.mtime_addr().unwrap(), 0).unwrap();
    intc.write64(cmp0, 1000).unwrap();
    intc.advance_mtime(999);
    assert!(ticks.borrow().is_empty());
    intc.advance_mtime(1);
    assert_eq!(*ticks.borrow(), [1000]);
    intc.advance_mtime(99);
    assert_eq!(ticks.borrow().len(), 1);
    intc.advance_mtime(1);
    assert_eq!(*ticks.borrow(), [1000, 1100]);
    // Masked across two periods: one delivery on restore, which moves
    // MTIMECMP once; the level is still high (1300 >= 1200), so it runs
    // again until MTIMECMP passes MTIME.
    let g = intc.cpu(0).unwrap().disable_local_save();
    intc.advance_mtime(250);
    assert_eq!(ticks.borrow().len(), 2);
    intc.cpu(0).unwrap().restore_local(g).unwrap();
    assert_eq!(*ticks.borrow(), [1000, 1100, 1350, 1350]);
    assert_eq!(intc.read64(cmp0).unwrap(), 1400);
    // 32-bit halves of MTIME are visible separately.
    assert_eq!(intc.read32(0x200_bff8).unwrap(), 1350);
    assert_eq!(intc.read32(0x200_bffc).unwrap(), 0);

    // An inter-processor interrupt to hart 1 through MSIP.
    let ipis = Rc::new(RefCell::new(0));
    let i2 = ipis.clone();
    intc.on_software(1, move |ev| {
        *i2.borrow_mut() += 1;
        ev.intc.write32(msip1, 0).unwrap();
    })
    .unwrap();
    intc.write32(msip1, 1).unwrap();
    assert_eq!(*ipis.borrow(), 1);
    assert_eq!(intc.read32(msip1).unwrap(), 0);
    assert_eq!(
        intc.deliveries().last(),
        Some(&Delivery {
            hart: 1,
            cause: MSIP,
            source: MSIP
        })
    );
    assert_eq!(intc.stats().storms, 0);
    assert_eq!(local_causes(&p, 1), [3, 7, 9, 11]);
}

#[test]
fn qemu_aclint_mswi_mtimer_and_supervisor_sswi() {
    let p = Platform::from_dtb(ACLINT).unwrap();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &Ledger::new()).unwrap();
    // MTIMER reg: MTIME window first, then MTIMECMP (as QEMU emits it).
    assert_eq!(intc.mtime_addr().unwrap(), 0x200_bff8);
    assert_eq!(intc.local_addr(1, MTIP).unwrap(), 0x200_4008);
    assert_eq!(intc.local_addr(1, MSIP).unwrap(), 0x200_0004);
    let fired = Rc::new(RefCell::new(Vec::new()));
    let f2 = fired.clone();
    let cmp = intc.local_addr(1, MTIP).unwrap();
    intc.on_timer(1, move |ev| {
        f2.borrow_mut().push(ev.cause);
        ev.intc.write64(cmp, u64::MAX).unwrap();
    })
    .unwrap();
    intc.write64(cmp, 50).unwrap();
    intc.advance_mtime(50);
    assert_eq!(*fired.borrow(), [MTIP]);
    // The MTIME window has nothing above MTIME.
    assert!(matches!(
        intc.read32(0x200_c000),
        Err(IntcError::Unmapped { .. })
    ));

    // S-mode software: SSWI sets SSIP, the handler clears it; reads are 0.
    let mut s = RiscvIntc::new(&p, Privilege::Supervisor, &Ledger::new()).unwrap();
    let setssip = s.local_addr(1, SSIP).unwrap();
    assert_eq!(setssip, 0x2f0_0004);
    let n = Rc::new(RefCell::new(0));
    let n2 = n.clone();
    s.on_software(1, move |ev| {
        *n2.borrow_mut() += 1;
        ev.intc.clear_ssip(ev.hart);
    })
    .unwrap();
    s.write32(setssip, 1).unwrap();
    assert_eq!(*n.borrow(), 1);
    assert_eq!(s.read32(setssip).unwrap(), 0);
    s.write32(setssip, 0).unwrap();
    assert_eq!(*n.borrow(), 1);
    // S-mode external is the hart's SEIP context (1 on hart 0).
    assert_eq!(s.context_of(0).unwrap(), 1);
    assert_eq!(s.on_timer(0, |_| {}), Err(IntcError::TimerNeedsMachineMode));
    assert_eq!(local_causes(&p, 0), [SSIP, MSIP, MTIP, SEIP, MEIP]);
}

#[test]
fn sifive_unleashed_supervisor_contexts_skip_hidden_ones() {
    let p = Platform::from_dtb(UNLEASHED).unwrap();
    // The OS-visible PLIC contexts are the S-mode ones; M-mode ones exist
    // in hardware but the tree hides them.
    let m = RiscvIntc::new(&p, Privilege::Machine, &Ledger::new()).unwrap();
    assert!(matches!(
        m.context_of(1),
        Err(IntcError::NoContext {
            hart: 1,
            cause: MEIP
        })
    ));
    let mut s = RiscvIntc::new(&p, Privilege::Supervisor, &Ledger::new()).unwrap();
    assert_eq!(s.hart_ids(), [1, 2, 3, 4]);
    assert_eq!(
        (1..=4)
            .map(|h| s.context_of(h).unwrap())
            .collect::<Vec<_>>(),
        [2, 4, 6, 8]
    );
    let seen = Rc::new(RefCell::new(Vec::new()));
    let src = s
        .install_isr(&p, "/soc/serial@10010000", 3, 1, acking(&seen))
        .unwrap();
    assert_eq!(src, 4);
    // Context 6's enable word is at 0x2000 + 6 * 0x80.
    assert_eq!(s.read32(PLIC + 0x2000 + 6 * 0x80).unwrap(), 1 << 4);
    s.set_line(4, true).unwrap();
    assert_eq!(*seen.borrow(), [(3, 4)]);
    assert_eq!(
        s.deliveries(),
        [Delivery {
            hart: 3,
            cause: SEIP,
            source: 4
        }]
    );
    // No CLINT node in this tree: no local software interrupt source.
    assert!(matches!(
        s.on_software(1, |_| {}),
        Err(IntcError::NoLocalSource { .. })
    ));
}

#[test]
fn starfive_visionfive2_machine_mode_uart() {
    let p = Platform::from_dtb(VF2).unwrap();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &Ledger::new()).unwrap();
    assert_eq!(intc.hart_ids(), [1, 2, 3, 4]);
    // Context 0 is the S7 monitor hart's M context, then (M, S) per U74.
    assert_eq!(intc.context_of(2).unwrap(), 3);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let src = intc
        .install_isr(&p, "/soc/serial@10000000", 2, 3, acking(&seen))
        .unwrap();
    assert_eq!(src, 32);
    intc.set_line(32, true).unwrap();
    assert_eq!(*seen.borrow(), [(2, 32)]);
    // The CLINT gives hart 2 software (index 2) and timer interrupts.
    assert_eq!(intc.local_addr(2, MSIP).unwrap(), 0x200_0008);
    assert_eq!(intc.local_addr(2, MTIP).unwrap(), 0x200_4010);
}

fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

/// Random register traffic and line changes on every fixture: typed errors
/// or values, never a panic, and the claim/complete bookkeeping stays
/// within the budgets.
#[test]
fn random_register_traffic_never_panics() {
    for blob in [RV_VIRT, ACLINT, UNLEASHED, VF2] {
        let p = Platform::from_dtb(blob).unwrap();
        for privilege in [Privilege::Machine, Privilege::Supervisor] {
            let mut intc = RiscvIntc::new(&p, privilege, &Ledger::new()).unwrap();
            for h in intc.hart_ids() {
                let _ = intc.on_software(h, |ev| {
                    let _ = ev.intc.write32(0x200_0000 + 4 * u64::from(ev.hart), 0);
                    ev.intc.clear_ssip(ev.hart);
                });
                let _ = intc.on_timer(h, |_| {});
            }
            let _ = intc.install_isr(&p, UART, 1, 1, |ev| {
                let _ = ev.intc.set_line(ev.source, false);
            });
            let mut seed = 0x9e37_79b9_7f4a_7c15u64 ^ blob.len() as u64;
            let windows = [
                PLIC,
                PLIC + 0x20_0000,
                0x200_0000,
                0x200_4000,
                0x200_bff8,
                0x2f0_0000,
            ];
            for _ in 0..3000 {
                let base = windows[(rng(&mut seed) % windows.len() as u64) as usize];
                let addr = base
                    + (rng(&mut seed) % 0x2000)
                        * if rng(&mut seed).is_multiple_of(4) {
                            1
                        } else {
                            4
                        };
                let v = rng(&mut seed) as u32;
                match rng(&mut seed) % 5 {
                    0 => {
                        let _ = intc.read32(addr);
                    }
                    1 => {
                        let _ = intc.write32(addr, v);
                    }
                    2 => {
                        let _ = intc.set_line(v % 200, v & 1 == 0);
                    }
                    3 => intc.advance_mtime(u64::from(v % 1000)),
                    _ => {
                        if let Some(&h) = intc.hart_ids().first() {
                            let g = intc.cpu(h).unwrap().disable_local_save();
                            let _ = intc.set_line(v % 200, true);
                            intc.cpu(h).unwrap().restore_local(g).unwrap();
                            intc.sync();
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn separate_mtimer_blocks_keep_separate_counters() {
    // Split the QEMU ACLINT MTIMER into two blocks with their own MTIME
    // registers, one per hart (the ACLINT specification allows this).
    let mut p = Platform::from_dtb(ACLINT).unwrap();
    let i = p
        .clints
        .iter()
        .position(|c| c.kind == vesper_silicon::dtb::ClintKind::AclintMtimer)
        .unwrap();
    let mut second = p.clints[i].clone();
    p.clints[i].targets.truncate(1);
    second.targets.remove(0);
    second.regs[0].base = 0x300_bff8;
    second.regs[0].size = 8;
    second.regs[1].base = 0x300_4000;
    second.regs[1].size = 8;
    second.path.push_str("-split");
    p.clints.push(second);
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &Ledger::new()).unwrap();
    let fired = Rc::new(RefCell::new(Vec::new()));
    for h in [0, 1] {
        let f = fired.clone();
        let cmp = intc.local_addr(h, MTIP).unwrap();
        intc.on_timer(h, move |ev| {
            f.borrow_mut().push(ev.hart);
            ev.intc.write64(cmp, u64::MAX).unwrap();
        })
        .unwrap();
    }
    assert_eq!(intc.local_addr(1, MTIP).unwrap(), 0x300_4000);
    intc.write64(0x200_4000, 100).unwrap();
    intc.write64(0x300_4000, 100).unwrap();
    // Writing hart 0's MTIME leaves hart 1's counter alone.
    intc.write64(0x200_bff8, 100).unwrap();
    assert_eq!(*fired.borrow(), [0]);
    assert_eq!(intc.read64(0x300_bff8).unwrap(), 0);
    assert_eq!(intc.handle().mtime(1), 0);
    // Advancing time advances both.
    intc.advance_mtime(100);
    assert_eq!(*fired.borrow(), [0, 1]);
    assert_eq!(intc.handle().mtime(0), 200);
    assert_eq!(intc.handle().mtime(1), 100);
}

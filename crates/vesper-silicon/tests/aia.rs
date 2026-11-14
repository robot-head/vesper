//! RISC-V AIA (APLIC and IMSIC), MSIs, several PLICs, edge-triggered
//! sources and `interrupt-map-pass-thru`, on QEMU 8.2.2 `virt` dumps
//! (`aia=aplic`, `aia=aplic-imsic`, guests, two sockets), the Allwinner D1
//! Nezha tree and a hand-written board.

use std::cell::RefCell;
use std::rc::Rc;

use vesper_silicon::aia::{
    AiaError, AplicDomain, ISEL_EIDELIVERY, ISEL_EIE0, ISEL_EIP0, ISEL_EITHRESHOLD, ImsicFile, Msi,
    SourceMode,
};
use vesper_silicon::dtb::{
    AplicDelegation, ControllerKind, DtbError, HartTarget, MemRange, Platform, Trigger,
};
use vesper_silicon::intc::{
    Ctl, Delivery, IntcError, IrqEvent, MEIP, Privilege, RiscvIntc, SEIP, msi_addr_cfg,
};
use vesper_silicon::ledger::Ledger;

const APLIC: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aplic.dtb");
const IMSIC: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aplic-imsic.dtb");
const GUESTS: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aplic-imsic-guests.dtb");
const AIA2S: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aia-2socket.dtb");
const PLIC2S: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-2socket.dtb");
const EDGE: &[u8] = include_bytes!("../fixtures/dtb/vesper-aia-edge.dtb");
const NEZHA: &[u8] = include_bytes!("../fixtures/dtb/sun20i-d1-nezha.dtb");

const UART: &str = "/soc/serial@10000000";
const M_DOMAIN: u64 = 0x0c00_0000;
const S_DOMAIN: u64 = 0x0d00_0000;
const M_FILES: u64 = 0x2400_0000;
const S_FILES: u64 = 0x2800_0000;

fn t(hart: u32, cause: u32) -> HartTarget {
    HartTarget {
        hart,
        cause: Some(cause),
    }
}

type Seen = Rc<RefCell<Vec<(u32, u32, u32)>>>;

/// An ISR that records `(hart, cause, source)` and drops the device line.
fn acking(seen: &Seen, platform: &Platform, device: &str) -> impl FnMut(&IrqEvent<'_>) + use<> {
    let seen = seen.clone();
    let line = platform
        .irq_lines
        .iter()
        .find(|l| l.device == device)
        .unwrap()
        .clone();
    let domain = platform
        .aplics
        .iter()
        .position(|a| a.path == line.controller);
    let plic = platform
        .plics
        .iter()
        .position(|p| p.path == line.controller);
    move |ev| {
        seen.borrow_mut().push((ev.hart, ev.cause, ev.source));
        let ctl = match (domain, plic) {
            (Some(d), _) => Ctl::Aplic(d),
            (_, Some(p)) => Ctl::Plic(p),
            _ => unreachable!(),
        };
        ev.intc
            .set_controller_line(ctl, line.number, false)
            .unwrap();
    }
}

#[test]
fn qemu_aplic_imsic_tree_is_read() {
    let p = Platform::from_dtb(IMSIC).unwrap();
    assert!(p.plics.is_empty());
    let root = p.aplics.iter().find(|a| a.parent.is_none()).unwrap();
    assert_eq!(root.path, "/soc/aplic@c000000");
    assert_eq!((root.base, root.size, root.sources), (M_DOMAIN, 0x8000, 96));
    assert!(root.idcs.is_empty());
    assert_eq!(root.msi_parent.as_deref(), Some("/soc/imsics@24000000"));
    assert_eq!(root.children, ["/soc/aplic@d000000"]);
    assert_eq!(
        root.delegations,
        [AplicDelegation {
            child: "/soc/aplic@d000000".into(),
            first: 1,
            last: 96
        }]
    );
    let s = p.aplics.iter().find(|a| a.parent.is_some()).unwrap();
    assert_eq!(s.parent.as_deref(), Some("/soc/aplic@c000000"));
    assert_eq!(s.msi_parent.as_deref(), Some("/soc/imsics@28000000"));

    let m = p.imsics.iter().find(|m| !m.supervisor()).unwrap();
    assert_eq!(m.ids, 255);
    assert_eq!(m.targets, [t(0, MEIP), t(1, MEIP)]);
    assert_eq!(m.files, [M_FILES, M_FILES + 0x1000]);
    assert_eq!((m.hart_index_bits, m.guest_index_bits), (1, 0));
    let sf = p.imsics.iter().find(|m| m.supervisor()).unwrap();
    assert_eq!(sf.files, [S_FILES, S_FILES + 0x1000]);
    assert_eq!(sf.file_of(1, 0), Some(S_FILES + 0x1000));
    assert_eq!(sf.file_of(1, 1), None);

    assert!(
        p.controllers
            .iter()
            .any(|c| c.kind == ControllerKind::Aplic { sources: 96 })
    );
    assert!(
        p.controllers
            .iter()
            .any(|c| c.kind == ControllerKind::Imsic { ids: 255 })
    );
    // Every wired device goes to the S-level domain with a 2-cell
    // level-high specifier; the PCIe host's MSIs go to the S-level IMSIC.
    let uart = p.irq_lines.iter().find(|l| l.device == UART).unwrap();
    assert_eq!(uart.controller, "/soc/aplic@d000000");
    assert_eq!((uart.number, uart.specifier.clone()), (10, vec![10, 4]));
    assert_eq!(p.trigger(uart).unwrap(), Trigger::LevelHigh);
    assert_eq!(
        p.msi_parent("/soc/pci@30000000").unwrap().as_deref(),
        Some("/soc/imsics@28000000")
    );
    assert_eq!(p.msi_parent(UART).unwrap(), None);
}

#[test]
fn qemu_direct_mode_and_guest_file_trees_are_read() {
    let p = Platform::from_dtb(APLIC).unwrap();
    assert!(p.imsics.is_empty());
    let root = p.aplics.iter().find(|a| a.parent.is_none()).unwrap();
    assert_eq!(root.idcs, [t(0, MEIP), t(1, MEIP)]);
    assert_eq!(root.msi_parent, None);
    let s = p.aplics.iter().find(|a| a.parent.is_some()).unwrap();
    assert_eq!(s.idcs, [t(0, SEIP), t(1, SEIP)]);

    // aia-guests=2 rounds up to 2 guest-index bits: four pages per hart.
    let g = Platform::from_dtb(GUESTS).unwrap();
    let sf = g.imsics.iter().find(|m| m.supervisor()).unwrap();
    assert_eq!((sf.guest_index_bits, sf.hart_index_bits), (2, 2));
    assert_eq!(
        sf.files,
        [
            S_FILES,
            S_FILES + 0x4000,
            S_FILES + 0x8000,
            S_FILES + 0xc000
        ]
    );
    assert_eq!(sf.file_of(2, 3), Some(S_FILES + 0x8000 + 0x3000));
    assert_eq!(sf.file_of(2, 4), None);
    let mf = g.imsics.iter().find(|m| !m.supervisor()).unwrap();
    assert_eq!(mf.guest_index_bits, 0);
    assert_eq!(mf.files[3], M_FILES + 0x3000);
}

#[test]
fn two_socket_imsic_groups_give_the_msi_address_fields() {
    let p = Platform::from_dtb(AIA2S).unwrap();
    assert_eq!(p.aplics.len(), 4);
    let m = p.imsics.iter().find(|m| !m.supervisor()).unwrap();
    // Group index bit 24 selects the socket.
    assert_eq!(
        (m.group_index_bits, m.group_index_shift, m.hart_index_bits),
        (1, 24, 1)
    );
    assert_eq!(
        m.files,
        [M_FILES, M_FILES + 0x1000, 0x2500_0000, 0x2500_1000]
    );
    let s = p.imsics.iter().find(|m| m.supervisor()).unwrap();
    let cfg = msi_addr_cfg(m, Some(s)).unwrap();
    assert_eq!((cfg.hhxs, cfg.hhxw, cfg.lhxw, cfg.lhxs_m), (0, 1, 1, 0));
    assert_eq!((cfg.m_ppn, cfg.s_ppn), (0x24000, 0x28000));
    for (k, &f) in m.files.iter().enumerate() {
        assert_eq!(cfg.address(false, k as u32, 0), f);
    }
    for (k, &f) in s.files.iter().enumerate() {
        assert_eq!(cfg.address(true, k as u32, 0), f);
    }
    // Socket 1's devices are wired to socket 1's S-level domain.
    let v = p
        .irq_lines
        .iter()
        .find(|l| l.device == "/soc/virtio_mmio@10001000")
        .unwrap();
    assert_eq!(v.controller, "/soc/aplic@d008000");
}

#[test]
fn supervisor_uart_through_aplic_msi_and_imsic() {
    let p = Platform::from_dtb(IMSIC).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Supervisor, &ledger).unwrap();
    // The firmware's part: the MSI address configuration is programmed and
    // locked, and every source is delegated to the S-level domain.
    assert_eq!(intc.read32(M_DOMAIN + 0x1bc0).unwrap(), 0x24000);
    assert_eq!(intc.read32(M_DOMAIN + 0x1bc4).unwrap(), 0x8000_1000);
    assert_eq!(intc.read32(M_DOMAIN + 0x1bc8).unwrap(), 0x28000);
    intc.write32(M_DOMAIN + 0x1bc0, 0x1).unwrap();
    assert_eq!(intc.read32(M_DOMAIN + 0x1bc0).unwrap(), 0x24000);
    assert_eq!(intc.read32(M_DOMAIN + 4 * 10).unwrap(), 0x400);

    let seen = Seen::default();
    let src = intc
        .install_isr(&p, UART, 1, 7, acking(&seen, &p, UART))
        .unwrap();
    assert_eq!(src, 10);
    // The S domain: IE and DM, level-high source, MSI target hart index 1.
    assert_eq!(intc.read32(S_DOMAIN).unwrap(), 0x8000_0104);
    assert_eq!(intc.read32(S_DOMAIN + 4 * 10).unwrap(), 6);
    let target = intc.read32(S_DOMAIN + 0x3000 + 4 * 10).unwrap();
    assert_eq!(target >> 18, 1);
    let id = target & 0x7ff;
    // Priority 7 takes the lowest band.
    assert_eq!(id, 1);
    assert_eq!(
        intc.imsic_csr_read(1, Privilege::Supervisor, ISEL_EIDELIVERY)
            .unwrap(),
        1
    );
    assert_eq!(
        intc.imsic_csr_read(1, Privilege::Supervisor, ISEL_EIE0)
            .unwrap(),
        1 << id
    );

    intc.set_device_line(&p, UART, true).unwrap();
    assert_eq!(*seen.borrow(), [(1, SEIP, 10)]);
    assert_eq!(
        intc.deliveries(),
        [Delivery {
            hart: 1,
            cause: SEIP,
            source: 10
        }]
    );
    let ext = intc.external_deliveries();
    assert_eq!(ext[0].controller, "/soc/imsics@28000000");
    assert_eq!((ext[0].number, ext[0].source), (id, Some(10)));
    assert!(!intc.external_level(1, SEIP));
    assert_eq!(intc.stats().spurious, 0);
}

#[test]
fn level_sources_in_msi_mode_are_retriggered_until_the_device_quiets() {
    let p = Platform::from_dtb(IMSIC).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Supervisor, &ledger).unwrap();
    let runs = Rc::new(RefCell::new(0u32));
    let r = runs.clone();
    intc.install_isr(&p, UART, 0, 3, move |ev| {
        // The device needs three services before its line drops.
        *r.borrow_mut() += 1;
        if r.borrow().is_multiple_of(3) {
            ev.intc
                .set_controller_line(Ctl::Aplic(0), 10, false)
                .unwrap();
        }
    })
    .unwrap();
    intc.set_device_line(&p, UART, true).unwrap();
    // Each EOI wrote setipnum while the line was high (AIA §4.9.2).
    assert_eq!(*runs.borrow(), 3);
    assert!(!intc.external_level(0, SEIP));
    // A held line under a masked hart stays pending and is delivered on
    // restore.
    let guard = intc.cpu(0).unwrap().disable_local_save();
    intc.set_device_line(&p, UART, true).unwrap();
    assert_eq!(*runs.borrow(), 3);
    assert!(intc.external_level(0, SEIP));
    intc.cpu(0).unwrap().restore_local(guard).unwrap();
    intc.sync();
    assert_eq!(*runs.borrow(), 6);
    assert_eq!(intc.stats().storms, 0);
}

#[test]
fn machine_mode_uses_the_root_domain_and_m_files() {
    let p = Platform::from_dtb(IMSIC).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    // Nothing is delegated and the configuration is not locked.
    assert_eq!(intc.read32(M_DOMAIN + 4 * 10).unwrap(), 0);
    assert_eq!(intc.read32(M_DOMAIN + 0x1bc4).unwrap(), 0x1000);
    let seen = Seen::default();
    intc.install_isr(&p, UART, 0, 1, acking(&seen, &p, UART))
        .unwrap();
    assert_eq!(intc.read32(M_DOMAIN + 4 * 10).unwrap(), 6);
    // An undelegated source is inactive in the S domain: read-only zero.
    intc.write32(S_DOMAIN + 4 * 10, 6).unwrap();
    assert_eq!(intc.read32(S_DOMAIN + 4 * 10).unwrap(), 0);
    intc.set_device_line(&p, UART, true).unwrap();
    assert_eq!(*seen.borrow(), [(0, MEIP, 10)]);
    assert_eq!(
        intc.external_deliveries()[0].controller,
        "/soc/imsics@24000000"
    );
    // M-mode may repoint the MSI address configuration; MSIs to a page with
    // no interrupt file are lost (a detached source set by software).
    intc.write32(M_DOMAIN + 0x1bc0, 0x30000).unwrap();
    intc.write32(M_DOMAIN + 4 * 5, 1).unwrap();
    intc.write32(M_DOMAIN + 0x3000 + 4 * 5, 9).unwrap();
    intc.write32(M_DOMAIN + 0x1edc, 5).unwrap();
    intc.write32(M_DOMAIN + 0x1cdc, 5).unwrap();
    assert_eq!(
        intc.lost_msis(),
        [Msi {
            addr: 0x3000_0000,
            data: 9
        }]
    );
}

#[test]
fn direct_mode_idcs_order_by_priority_and_threshold() {
    let p = Platform::from_dtb(APLIC).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Supervisor, &ledger).unwrap();
    let seen = Seen::default();
    let rtc = "/soc/rtc@101000";
    intc.install_isr(&p, UART, 0, 2, acking(&seen, &p, UART))
        .unwrap();
    intc.install_isr(&p, rtc, 0, 6, acking(&seen, &p, rtc))
        .unwrap();
    assert_eq!(intc.read32(S_DOMAIN).unwrap(), 0x8000_0100);
    // IPRIO = 8 - priority: the RTC (6) is more urgent than the UART (2).
    assert_eq!(intc.read32(S_DOMAIN + 0x3000 + 4 * 10).unwrap(), 6);
    assert_eq!(intc.read32(S_DOMAIN + 0x3000 + 4 * 11).unwrap(), 2);
    let idc0 = S_DOMAIN + 0x4000;
    let guard = intc.cpu(0).unwrap().disable_local_save();
    intc.set_device_line(&p, UART, true).unwrap();
    intc.set_device_line(&p, rtc, true).unwrap();
    // topi: identity 11, priority 2.
    assert_eq!(intc.read32(idc0 + 0x18).unwrap(), (11 << 16) | 2);
    // A threshold of 2 masks both (priority must be below it).
    intc.write32(idc0 + 0x08, 2).unwrap();
    assert_eq!(intc.read32(idc0 + 0x18).unwrap(), 0);
    assert!(!intc.external_level(0, SEIP));
    intc.write32(idc0 + 0x08, 0).unwrap();
    intc.cpu(0).unwrap().restore_local(guard).unwrap();
    intc.sync();
    assert_eq!(*seen.borrow(), [(0, SEIP, 11), (0, SEIP, 10)]);
    assert_eq!(
        intc.external_deliveries()[0].controller,
        "/soc/aplic@d000000"
    );
    // iforce with nothing pending: a spurious delivery, cleared by claimi.
    intc.write32(idc0 + 0x04, 1).unwrap();
    assert_eq!(intc.stats().spurious, 1);
    assert_eq!(intc.read32(idc0 + 0x04).unwrap(), 0);
    assert!(!intc.external_level(0, SEIP));
}

#[test]
fn device_msis_reach_the_imsic_and_are_claimed_by_identity() {
    let p = Platform::from_dtb(IMSIC).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Supervisor, &ledger).unwrap();
    assert_eq!(intc.msi_target(1).unwrap(), S_FILES + 0x1000);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let (s1, s2) = (seen.clone(), seen.clone());
    let hi = intc
        .install_msi(1, 7, move |ev| s1.borrow_mut().push(ev.source))
        .unwrap();
    let lo = intc
        .install_msi(1, 1, move |ev| s2.borrow_mut().push(ev.source))
        .unwrap();
    assert_eq!(
        (hi, lo),
        (
            Msi {
                addr: S_FILES + 0x1000,
                data: 1
            },
            Msi {
                addr: S_FILES + 0x1000,
                data: 193
            }
        )
    );
    // Both arrive while masked; the lower identity is claimed first.
    let guard = intc.cpu(1).unwrap().disable_local_save();
    intc.msi_write(lo.addr, lo.data).unwrap();
    intc.msi_write(hi.addr, hi.data).unwrap();
    assert_eq!(intc.topei(1, Privilege::Supervisor).unwrap(), (1 << 16) | 1);
    intc.cpu(1).unwrap().restore_local(guard).unwrap();
    intc.sync();
    assert_eq!(*seen.borrow(), [1, 193]);
    // Big-endian seteipnum, an unimplemented identity (ignored) and an MSI
    // to no file.
    intc.msi_write(hi.addr + 4, 1u32.swap_bytes()).unwrap();
    assert_eq!(*seen.borrow(), [1, 193, 1]);
    intc.msi_write(hi.addr, 300).unwrap();
    assert_eq!(*seen.borrow(), [1, 193, 1]);
    assert_eq!(
        intc.msi_write(0x2700_0000, 1),
        Err(IntcError::Unmapped { addr: 0x2700_0000 })
    );
}

#[test]
fn guest_files_receive_msis_without_raising_the_hart() {
    let p = Platform::from_dtb(GUESTS).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Supervisor, &ledger).unwrap();
    let sf = p.imsics.iter().find(|m| m.supervisor()).unwrap();
    let g2 = sf.file_of(2, 2).unwrap();
    intc.msi_write(g2, 40).unwrap();
    let h = intc.handle();
    assert!(h.guest_eip(2, 2, 40).unwrap());
    assert!(!h.guest_eip(2, 1, 40).unwrap());
    assert!(!intc.external_level(2, SEIP));
    assert!(matches!(
        h.guest_eip(2, 4, 40),
        Err(IntcError::NoImsic { .. })
    ));
    // The APLIC's S-level MSI address puts guest g at page g of the hart.
    let m = p.imsics.iter().find(|m| !m.supervisor()).unwrap();
    let cfg = msi_addr_cfg(m, Some(sf)).unwrap();
    assert_eq!(cfg.lhxs_s, 2);
    assert_eq!(cfg.address(true, 2, 2), g2);
}

#[test]
fn two_sockets_have_independent_plics() {
    let p = Platform::from_dtb(PLIC2S).unwrap();
    assert_eq!(p.plics.len(), 2);
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    assert_eq!(intc.plic_bases(), [0x0c00_0000, 0x0c60_0000]);
    assert_eq!(intc.plic_context(0).unwrap(), (0, 0));
    assert_eq!(intc.plic_context(3).unwrap(), (1, 2));
    let seen = Seen::default();
    // The UART is on socket 0's PLIC: a socket-1 hart has no context there.
    assert!(matches!(
        intc.install_isr(&p, UART, 2, 1, |_| {}),
        Err(IntcError::NoContext { hart: 2, .. })
    ));
    intc.install_isr(&p, UART, 1, 1, acking(&seen, &p, UART))
        .unwrap();
    let virtio = "/soc/virtio_mmio@10001000";
    intc.install_isr(&p, virtio, 3, 2, acking(&seen, &p, virtio))
        .unwrap();
    // Each PLIC's registers live in its own window.
    assert_eq!(intc.read32(0x0c00_0000 + 4 * 10).unwrap(), 1);
    assert_eq!(intc.read32(0x0c60_0000 + 4).unwrap(), 2);
    assert_eq!(intc.read32(0x0c60_0000 + 4 * 10).unwrap(), 0);
    // Source 1 of socket 0's PLIC is a different line from source 1 of
    // socket 1's.
    intc.set_controller_line(Ctl::Plic(0), 1, true).unwrap();
    assert!(seen.borrow().is_empty());
    intc.set_device_line(&p, virtio, true).unwrap();
    intc.set_device_line(&p, UART, true).unwrap();
    assert_eq!(*seen.borrow(), [(3, MEIP, 1), (1, MEIP, 10)]);
    let ext = intc.external_deliveries();
    assert_eq!(ext[0].controller, "plic@c600000");
}

#[test]
fn two_socket_aia_routes_each_socket_through_its_own_domains() {
    let p = Platform::from_dtb(AIA2S).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Supervisor, &ledger).unwrap();
    let seen = Seen::default();
    let virtio = "/soc/virtio_mmio@10001000";
    intc.install_isr(&p, virtio, 3, 4, acking(&seen, &p, virtio))
        .unwrap();
    intc.install_isr(&p, UART, 2, 4, acking(&seen, &p, UART))
        .unwrap();
    // Hart 3 is group 1, hart 1 within it: hart index 3.
    assert_eq!(intc.read32(0x0d00_8000 + 0x3000 + 4).unwrap() >> 18, 3);
    intc.set_device_line(&p, virtio, true).unwrap();
    intc.set_device_line(&p, UART, true).unwrap();
    assert_eq!(*seen.borrow(), [(3, SEIP, 1), (2, SEIP, 10)]);
    assert_eq!(intc.lost_msis(), []);
}

#[test]
fn edge_triggered_plic_sources_latch_one_edge_while_in_service() {
    let p = Platform::from_dtb(EDGE).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    let gpio = "/soc/gpio@20001000";
    let runs = Rc::new(RefCell::new(0u32));
    let r = runs.clone();
    let src = intc
        .install_isr(&p, gpio, 0, 1, move |ev| {
            *r.borrow_mut() += 1;
            if *r.borrow() == 1 {
                // Two more edges arrive during the first service: one is
                // held, the other merges with it.
                for _ in 0..2 {
                    ev.intc.set_line(6, false).unwrap();
                    ev.intc.set_line(6, true).unwrap();
                }
            }
        })
        .unwrap();
    assert_eq!(src, 6);
    intc.set_line(6, true).unwrap();
    assert_eq!(*runs.borrow(), 2);
    // A line that stays high is not re-forwarded (unlike a level source).
    intc.set_line(6, true).unwrap();
    assert_eq!(*runs.borrow(), 2);
    intc.set_line(6, false).unwrap();
    intc.set_line(6, true).unwrap();
    assert_eq!(*runs.borrow(), 3);
    assert_eq!(intc.stats().storms, 0);

    // A level source on the same PLIC still repeats while high.
    let seen = Seen::default();
    intc.install_isr(
        &p,
        "/soc/uart@20000000",
        0,
        1,
        acking(&seen, &p, "/soc/uart@20000000"),
    )
    .unwrap();
    intc.set_line(5, true).unwrap();
    assert_eq!(seen.borrow().len(), 1);
    // A falling edge is not a PLIC gateway type.
    assert!(matches!(
        intc.install_isr(&p, "/soc/falling@20002000", 0, 1, |_| {}),
        Err(IntcError::Dtb(DtbError::BadInterruptSpecifier { .. }))
    ));
    // The second PLIC only reaches hart 1.
    assert!(matches!(
        intc.install_isr(&p, "/soc/timer@20003000", 0, 1, |_| {}),
        Err(IntcError::NoContext { hart: 0, .. })
    ));
    let seen2 = Seen::default();
    intc.install_isr(
        &p,
        "/soc/timer@20003000",
        1,
        1,
        acking(&seen2, &p, "/soc/timer@20003000"),
    )
    .unwrap();
    intc.set_device_line(&p, "/soc/timer@20003000", true)
        .unwrap();
    assert_eq!(*seen2.borrow(), [(1, MEIP, 3)]);
}

#[test]
fn interrupt_map_pass_thru_copies_the_flags_cell() {
    let p = Platform::from_dtb(EDGE).unwrap();
    let lines: Vec<_> = p
        .irq_lines
        .iter()
        .filter(|l| l.device == "/soc/button")
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        (lines[0].number, lines[0].specifier.clone()),
        (20, vec![20, 1])
    );
    assert_eq!(
        (lines[1].number, lines[1].specifier.clone()),
        (21, vec![21, 4])
    );
    assert_eq!(p.trigger(lines[0]).unwrap(), Trigger::EdgeRising);
    assert_eq!(p.trigger(lines[1]).unwrap(), Trigger::LevelHigh);
}

#[test]
fn allwinner_d1_two_cell_plic_is_read() {
    let p = Platform::from_dtb(NEZHA).unwrap();
    assert_eq!(p.plics.len(), 1);
    assert_eq!(p.plics[0].ndev, 175);
    assert_eq!(p.plics[0].contexts, [t(0, MEIP), t(0, SEIP)]);
    let uart = p
        .irq_lines
        .iter()
        .find(|l| l.device == "/soc/serial@2500000")
        .unwrap();
    assert_eq!(uart.specifier, [18, 4]);
    let plic_lines: Vec<_> = p
        .irq_lines
        .iter()
        .filter(|l| l.controller == p.plics[0].path)
        .collect();
    assert!(plic_lines.len() >= 20, "{}", plic_lines.len());
    assert!(
        plic_lines
            .iter()
            .all(|l| p.trigger(l).unwrap() == Trigger::LevelHigh)
    );
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Supervisor, &ledger).unwrap();
    let seen = Seen::default();
    intc.install_isr(
        &p,
        "/soc/serial@2500000",
        0,
        1,
        acking(&seen, &p, "/soc/serial@2500000"),
    )
    .unwrap();
    intc.set_device_line(&p, "/soc/serial@2500000", true)
        .unwrap();
    assert_eq!(*seen.borrow(), [(0, SEIP, 18)]);
}

fn window() -> MemRange {
    MemRange {
        base: 0x1000_0000,
        size: 0x8000,
    }
}

#[test]
fn aplic_source_modes_follow_the_pending_rules() {
    let base = 0x1000_0000u64;
    // A direct-mode root domain with one IDC and no children.
    let mut d = AplicDomain::new("/d", window(), 8, 1, false, true, false, 0).unwrap();
    let w = |d: &mut AplicDomain, off: u64, v: u32| d.write32(base + off, v).unwrap();
    let r = |d: &mut AplicDomain, off: u64| d.read32(base + off).unwrap();
    w(&mut d, 0, 0x104);
    // DM is read-only 0 (no msi-parent); IE set.
    assert_eq!(r(&mut d, 0), 0x8000_0100);
    // No children: D is read-only zero.
    w(&mut d, 4, 0x400 | 6);
    assert_eq!(r(&mut d, 4), 6);
    // Reserved modes are stored as Inactive.
    w(&mut d, 8, 2);
    assert_eq!(r(&mut d, 8), 0);

    // Level low (source 1): pending tracks the inverted input in direct
    // mode and cannot be set or cleared by software.
    w(&mut d, 4, 7);
    assert_eq!(d.mode(1), SourceMode::LevelLow);
    assert!(d.is_pending(1));
    d.set_wire(1, true);
    assert!(!d.is_pending(1));
    w(&mut d, 0x1cdc, 1);
    assert!(!d.is_pending(1));
    d.set_wire(1, false);
    w(&mut d, 0x1ddc, 1);
    assert!(d.is_pending(1));
    assert_eq!(r(&mut d, 0x1d00) & 2, 2);

    // Edge falling (source 2).
    w(&mut d, 8, 5);
    d.set_wire(2, true);
    assert!(!d.is_pending(2));
    d.set_wire(2, false);
    assert!(d.is_pending(2));
    w(&mut d, 0x1d00, 1 << 2);
    assert!(!d.is_pending(2));

    // Detached (source 3): the wire is ignored, software sets it.
    w(&mut d, 12, 1);
    d.set_wire(3, true);
    assert!(!d.is_pending(3));
    assert_eq!(r(&mut d, 0x1d00) & 8, 0);
    w(&mut d, 0x1c00, 1 << 3);
    assert!(d.is_pending(3));

    // Claims through the IDC: priority order, then the lowest identity.
    w(&mut d, 0x3004 + 4 * 2, 4); // source 3, IPRIO 4
    w(&mut d, 0x3004, 0); // source 1: IPRIO 0 reads as 1
    assert_eq!(r(&mut d, 0x3004), 1);
    w(&mut d, 0x1e00, 0b1010);
    w(&mut d, 0x4000, 1);
    assert!(d.idc_asserted(0));
    assert_eq!(r(&mut d, 0x4000 + 0x18), (1 << 16) | 1);
    // Claiming a level source whose input is still active leaves it pending.
    assert_eq!(r(&mut d, 0x4000 + 0x1c), (1 << 16) | 1);
    assert!(d.is_pending(1));
    w(&mut d, 0x1f00, 0b10);
    assert_eq!(r(&mut d, 0x4000 + 0x1c), (3 << 16) | 4);
    assert!(!d.is_pending(3));
    assert_eq!(r(&mut d, 0x4000 + 0x1c), 0);
    // Making a source inactive clears its pending, enable and target.
    w(&mut d, 4, 0);
    assert_eq!(r(&mut d, 0x1e00) & 2, 0);
    assert_eq!(r(&mut d, 0x3004), 0);
    // Errors.
    assert_eq!(
        d.read32(base + 2),
        Err(AiaError::Misaligned { addr: base + 2 })
    );
    assert_eq!(
        d.read32(base + 0x4020),
        Err(AiaError::Unmapped {
            addr: base + 0x4020
        })
    );
    assert!(matches!(
        AplicDomain::new("/x", window(), 0, 1, false, true, false, 0),
        Err(AiaError::BadSourceCount(0))
    ));
    assert!(matches!(
        AplicDomain::new("/x", window(), 8, 0, false, true, false, 0),
        Err(AiaError::NoDeliveryMode { .. })
    ));
    assert!(matches!(
        AplicDomain::new(
            "/x",
            MemRange { base, size: 0x2000 },
            8,
            1,
            false,
            true,
            false,
            0
        ),
        Err(AiaError::WindowTooSmall { .. })
    ));
}

#[test]
fn aplic_msi_mode_forwards_and_genmsi() {
    let base = 0x1000_0000u64;
    let mut d = AplicDomain::new("/d", window(), 8, 0, true, true, false, 0).unwrap();
    let w = |d: &mut AplicDomain, off: u64, v: u32| d.write32(base + off, v).unwrap();
    // MSI-only: DM reads 1 even when written 0.
    w(&mut d, 0, 0x100);
    assert_eq!(d.read32(base).unwrap(), 0x8000_0104);
    w(&mut d, 0x1bc0, 0x24000);
    w(&mut d, 0x1bc4, 1 << 12); // LHXW = 1
    w(&mut d, 4, 6); // source 1 level high
    w(&mut d, 0x3004, (1 << 18) | 33);
    w(&mut d, 0x1edc, 1);
    d.set_wire(1, true);
    assert_eq!(
        d.take_msis(),
        [Msi {
            addr: 0x2400_1000,
            data: 33
        }]
    );
    assert!(!d.is_pending(1));
    // setipnum while the level is high resends; while low it does not
    // (AIA §4.7; QEMU 8.2.2 resends anyway, see tests/aia_qemu.rs).
    w(&mut d, 0x2000, 1);
    assert_eq!(d.take_msis().len(), 1);
    d.set_wire(1, false);
    w(&mut d, 0x1cdc, 1);
    assert_eq!(d.take_msis(), []);
    // genmsi: an extempore MSI to hart index 0.
    w(&mut d, 0x3000, 77);
    assert_eq!(
        d.take_msis(),
        [Msi {
            addr: 0x2400_0000,
            data: 77
        }]
    );
    // The lock freezes the whole configuration.
    w(&mut d, 0x1bc4, (1 << 31) | (1 << 12));
    w(&mut d, 0x1bc0, 0);
    assert_eq!(d.read32(base + 0x1bc0).unwrap(), 0x24000);
    assert_eq!(d.msi_cfg().address(false, 1, 0), 0x2400_1000);
}

#[test]
fn imsic_file_registers() {
    let mut f = ImsicFile::new(63).unwrap();
    assert!(matches!(ImsicFile::new(64), Err(AiaError::BadIdCount(64))));
    assert!(matches!(
        ImsicFile::new(2048),
        Err(AiaError::BadIdCount(2048))
    ));
    f.csr_write(ISEL_EIE0, u64::MAX).unwrap();
    // Identity 0 is read-only zero.
    assert_eq!(f.csr_read(ISEL_EIE0).unwrap(), u64::MAX - 1);
    // RV64: odd eip/eie numbers and unknown selects are illegal.
    assert_eq!(f.csr_read(ISEL_EIP0 + 1), Err(AiaError::BadIselect(0x81)));
    assert_eq!(f.csr_write(0x71, 0), Err(AiaError::BadIselect(0x71)));
    // Registers beyond the implemented identities read zero.
    assert_eq!(f.csr_read(ISEL_EIP0 + 2).unwrap(), 0);
    f.seteipnum(9);
    f.seteipnum(0);
    f.seteipnum(64);
    assert_eq!(f.csr_read(ISEL_EIP0).unwrap(), 1 << 9);
    assert!(!f.asserted());
    f.csr_write(ISEL_EIDELIVERY, 3).unwrap();
    assert_eq!(f.csr_read(ISEL_EIDELIVERY).unwrap(), 1);
    assert!(f.asserted());
    // eithreshold keeps 6 bits for 63 identities; 9 is not below 9.
    f.csr_write(ISEL_EITHRESHOLD, 0x1c9).unwrap();
    assert_eq!(f.csr_read(ISEL_EITHRESHOLD).unwrap(), 9);
    assert_eq!(f.topei(), 0);
    f.csr_write(ISEL_EITHRESHOLD, 10).unwrap();
    assert_eq!(f.topei(), (9 << 16) | 9);
    assert_eq!(f.claim(), (9 << 16) | 9);
    assert_eq!(f.claim(), 0);
    // MMIO: seteipnum_be, and reserved offsets are ignored.
    f.mmio_write32(4, 5u32.swap_bytes());
    f.mmio_write32(8, 6);
    assert!(f.is_pending(5) && !f.is_pending(6));
}

#[test]
fn malformed_aia_nodes_are_typed_errors() {
    let find = |b: &[u8], pat: &[u8]| b.windows(pat.len()).position(|w| w == pat).unwrap();
    // riscv,num-ids = 0xff -> 0x100 (not one below a multiple of 64).
    let mut b = IMSIC.to_vec();
    let name_off = find(&b, b"riscv,num-ids\0");
    // The property value follows a 12-byte header that names the string
    // offset; patch every 0x000000ff value cell instead.
    let _ = name_off;
    let pat = [0, 0, 0, 0xffu8];
    let mut at = 0;
    while let Some(k) = b[at..].windows(4).position(|w| w == pat) {
        b[at + k + 2] = 1;
        b[at + k + 3] = 0;
        at += k + 4;
    }
    assert!(matches!(
        Platform::from_dtb(&b),
        Err(DtbError::BadProperty { .. })
    ));
    // Random corruption never panics.
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..400 {
        let mut b = IMSIC.to_vec();
        for _ in 0..4 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let k = (seed as usize) % b.len();
            b[k] = (seed >> 32) as u8;
        }
        if let Ok(p) = Platform::from_dtb(&b) {
            let ledger = Ledger::new();
            let _ = RiscvIntc::new(&p, Privilege::Supervisor, &ledger);
        }
    }
}

#[test]
fn random_register_traffic_never_panics() {
    for blob in [APLIC, IMSIC, GUESTS, AIA2S] {
        let p = Platform::from_dtb(blob).unwrap();
        for privilege in [Privilege::Machine, Privilege::Supervisor] {
            let ledger = Ledger::new();
            let mut intc = RiscvIntc::new(&p, privilege, &ledger).unwrap();
            let _ = intc.install_isr(&p, UART, 0, 3, |_| {});
            let mut seed = 0x1234_5678_9abc_def0u64;
            for _ in 0..3000 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let base = [M_DOMAIN, S_DOMAIN, M_FILES, S_FILES][(seed % 4) as usize];
                let addr = (base + ((seed >> 8) % 0x8000)) & !3;
                let v = (seed >> 24) as u32;
                match (seed >> 60) % 4 {
                    0 => {
                        let _ = intc.write32(addr, v);
                    }
                    1 => {
                        let _ = intc.read32(addr);
                    }
                    2 => {
                        let _ = intc.set_line((v % 100) + 1, v & 1 != 0);
                    }
                    _ => {
                        let isel = 0x70 + (v % 0x90);
                        let _ = intc.imsic_csr_write(0, privilege, isel, u64::from(v));
                        let _ = intc.claim_topei(0, privilege);
                    }
                }
            }
        }
    }
}

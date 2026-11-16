//! Wave-nine AIA: guest interrupt files and VS-level claims, `eidelivery`
//! 0x40000000 (the APLIC's direct outputs through the IMSIC's position),
//! and PCI `msi-map`/`msi-parent` routing with `dma-ranges`, on the
//! hand-written `vesper-aia-hyp` board, the QEMU 8.2.2 `virt` dumps and the
//! QEMU `virt` (AArch64, GICv3 ITS) dump. The register behaviour these
//! build on is cross-checked against QEMU in `tests/aia_qemu.rs`.

use std::cell::RefCell;
use std::rc::Rc;

use vesper_silicon::aia::{
    EIDELIVERY_EXTERNAL, EIDELIVERY_OFF, EIDELIVERY_ON, ISEL_EIDELIVERY, ISEL_EIE0, ISEL_EIP0,
    ISEL_EITHRESHOLD, ImsicFile,
};
use vesper_silicon::dtb::{DtbError, MsiRoute, Platform, requester_id};
use vesper_silicon::intc::{Ctl, IntcError, MEIP, Privilege, RiscvIntc, SEIP, SGEIP, VSEIP};
use vesper_silicon::ledger::Ledger;

const HYP: &[u8] = include_bytes!("../fixtures/dtb/vesper-aia-hyp.dtb");
const GUESTS: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aplic-imsic-guests.dtb");
const IMSIC: &[u8] = include_bytes!("../fixtures/dtb/qemu-riscv64-virt-aplic-imsic.dtb");
const ARM: &[u8] = include_bytes!("../fixtures/dtb/qemu-aarch64-virt-gicv3.dtb");

const M_DOMAIN: u64 = 0x0c00_0000;
const IDC0: u64 = M_DOMAIN + 0x4000;
const M_FILES: u64 = 0x2400_0000;
const S_FILES: u64 = 0x2800_0000;
const PCI0: &str = "/soc/pci@30000000";
const PCI1: &str = "/soc/pci@31000000";
const IMSIC_M: &str = "/soc/interrupt-controller@24000000";
const IMSIC_S: &str = "/soc/interrupt-controller@28000000";

fn hyp() -> Platform {
    Platform::from_dtb(HYP).unwrap()
}

#[test]
fn eidelivery_external_is_a_warl_value_of_wired_files_only() {
    let mut f = ImsicFile::with_external_delivery(63).unwrap();
    // Reset value when supported (§3.8.1).
    assert_eq!(f.csr_read(ISEL_EIDELIVERY).unwrap(), 0x4000_0000);
    assert!(f.delivers_external());
    for (w, r) in [
        (1, EIDELIVERY_ON),
        (0, EIDELIVERY_OFF),
        (0x4000_0000, EIDELIVERY_EXTERNAL),
        (0x4000_0001, EIDELIVERY_ON),
        (0xffff_fffe, EIDELIVERY_OFF),
    ] {
        f.csr_write(ISEL_EIDELIVERY, w).unwrap();
        assert_eq!(f.eidelivery(), r, "write {w:#x}");
    }
    // A file without the option (every guest file) keeps bit 0, as QEMU
    // 8.2.2's files do.
    let mut g = ImsicFile::new(63).unwrap();
    assert_eq!(g.eidelivery(), 0);
    g.csr_write(ISEL_EIDELIVERY, 0x4000_0000).unwrap();
    assert_eq!(g.eidelivery(), 0);
    // With 0x40000000 the file acts as though eidelivery were 0, and
    // *topei is unaffected (§3.8.1).
    f.csr_write(ISEL_EIDELIVERY, 0x4000_0000).unwrap();
    f.csr_write(ISEL_EIE0, 1 << 3).unwrap();
    f.seteipnum(3);
    assert!(!f.asserted());
    assert_eq!(f.topei(), 0x30003);
    f.csr_write(ISEL_EIDELIVERY, 1).unwrap();
    assert!(f.asserted());

    // Which files get the option: the hand-written board's M files share
    // their harts with the root APLIC's IDCs; its S files and every QEMU
    // file do not.
    let p = hyp();
    let ledger = Ledger::new();
    let intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    let h = intc.handle();
    for hart in 0..2 {
        assert_eq!(
            h.imsic_csr_read(hart, Privilege::Machine, ISEL_EIDELIVERY)
                .unwrap(),
            0x4000_0000
        );
        assert_eq!(
            h.imsic_csr_read(hart, Privilege::Supervisor, ISEL_EIDELIVERY)
                .unwrap(),
            0
        );
    }
    let q = Platform::from_dtb(IMSIC).unwrap();
    let qi = RiscvIntc::new(&q, Privilege::Machine, &ledger).unwrap();
    qi.handle()
        .imsic_csr_write(0, Privilege::Machine, ISEL_EIDELIVERY, 0x4000_0000)
        .unwrap();
    assert_eq!(
        qi.handle()
            .imsic_csr_read(0, Privilege::Machine, ISEL_EIDELIVERY)
            .unwrap(),
        0
    );
}

#[test]
fn eidelivery_selects_between_the_file_and_the_direct_aplic() {
    let p = hyp();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    // Root domain in direct mode (DM = 0, it has both modes), source 20
    // level-high to IDC 0 (hart 0) at IPRIO 1.
    intc.write32(M_DOMAIN, 0x100).unwrap();
    assert_eq!(intc.read32(M_DOMAIN).unwrap(), 0x8000_0100);
    intc.write32(M_DOMAIN + 4 * 20, 6).unwrap();
    intc.write32(M_DOMAIN + 0x3000 + 4 * 20, 1).unwrap();
    intc.write32(M_DOMAIN + 0x1edc, 20).unwrap();
    intc.write32(IDC0, 1).unwrap();
    intc.set_controller_line(Ctl::Aplic(0), 20, true).unwrap();
    assert_eq!(intc.read32(IDC0 + 0x18).unwrap(), (20 << 16) | 1);
    // eidelivery = 0x40000000 (reset): the IDC reaches MEIP.
    assert!(intc.external_level(0, MEIP));
    assert!(!intc.external_level(1, MEIP));
    // eidelivery = 1: the file supplies MEIP; the IDC is still asserted
    // but does not reach the hart.
    intc.imsic_csr_write(0, Privilege::Machine, ISEL_EIDELIVERY, 1)
        .unwrap();
    assert_eq!(intc.read32(IDC0 + 0x18).unwrap(), (20 << 16) | 1);
    assert!(!intc.external_level(0, MEIP));
    intc.imsic_csr_write(0, Privilege::Machine, ISEL_EIE0, 1 << 7)
        .unwrap();
    intc.msi_write(M_FILES, 7).unwrap();
    assert!(intc.external_level(0, MEIP));
    // eidelivery = 0: nothing.
    intc.imsic_csr_write(0, Privilege::Machine, ISEL_EIDELIVERY, 0)
        .unwrap();
    assert!(!intc.external_level(0, MEIP));
    // mtopei does not depend on eidelivery.
    assert_eq!(intc.topei(0, Privilege::Machine).unwrap(), 0x70007);
    intc.imsic_csr_write(0, Privilege::Machine, ISEL_EIDELIVERY, 0x4000_0000)
        .unwrap();
    assert!(intc.external_level(0, MEIP));
    assert_eq!(intc.read32(IDC0 + 0x1c).unwrap(), (20 << 16) | 1);
    intc.set_controller_line(Ctl::Aplic(0), 20, false).unwrap();
    assert!(!intc.external_level(0, MEIP));
}

#[test]
fn the_claim_loop_follows_eidelivery() {
    let p = hyp();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let s2 = seen.clone();
    // An MSI handler switches hart 0's M file to eidelivery = 1.
    let msi = intc
        .install_msi(0, 4, move |e| s2.borrow_mut().push(e.source))
        .unwrap();
    assert_eq!(
        intc.imsic_csr_read(0, Privilege::Machine, ISEL_EIDELIVERY)
            .unwrap(),
        1
    );
    intc.msi_write(msi.addr, msi.data).unwrap();
    assert_eq!(*seen.borrow(), vec![msi.data]);
    // A direct APLIC source on the same hart is held off while the file
    // delivers...
    intc.write32(M_DOMAIN, 0x100).unwrap();
    // Edge-triggered, so one claim retires it.
    intc.write32(M_DOMAIN + 4 * 21, 4).unwrap();
    intc.write32(M_DOMAIN + 0x3000 + 4 * 21, 2).unwrap();
    intc.write32(M_DOMAIN + 0x1edc, 21).unwrap();
    intc.write32(IDC0, 1).unwrap();
    intc.set_controller_line(Ctl::Aplic(0), 21, true).unwrap();
    assert_eq!(intc.stats().unhandled, 0);
    assert_eq!(intc.external_deliveries().len(), 1);
    // ...and claimed through claimi once software selects 0x40000000 (no
    // handler is installed for it, so it is counted unhandled).
    intc.imsic_csr_write(0, Privilege::Machine, ISEL_EIDELIVERY, 0x4000_0000)
        .unwrap();
    let ext = intc.external_deliveries();
    assert_eq!(ext.len(), 2);
    assert_eq!(ext[1].controller, "/soc/interrupt-controller@c000000");
    assert_eq!(ext[1].number, 21);
    assert_eq!(intc.stats().unhandled, 1);
    intc.set_controller_line(Ctl::Aplic(0), 21, false).unwrap();
}

#[test]
fn wired_routes_and_msis_share_a_level_through_eidelivery() {
    let p = hyp();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let s2 = seen.clone();
    // A PLIC device on hart 1: the route selects eidelivery = 0x40000000.
    intc.install_isr(&p, "/soc/gpio@10001000", 1, 3, move |e| {
        s2.borrow_mut().push(e.source);
        e.intc.set_controller_line(Ctl::Plic(0), 3, false).unwrap();
    })
    .unwrap();
    assert_eq!(
        intc.imsic_csr_read(1, Privilege::Machine, ISEL_EIDELIVERY)
            .unwrap(),
        0x4000_0000
    );
    intc.set_line(3, true).unwrap();
    assert_eq!(*seen.borrow(), vec![3]);
    // An MSI route on the same hart and level switches the file to
    // eidelivery = 1, after which the PLIC no longer reaches the hart...
    let msi = intc.install_msi(1, 3, |_| {}).unwrap();
    assert_eq!(
        intc.imsic_csr_read(1, Privilege::Machine, ISEL_EIDELIVERY)
            .unwrap(),
        1
    );
    intc.set_line(3, true).unwrap();
    assert_eq!(*seen.borrow(), vec![3]);
    assert!(!intc.external_level(1, MEIP));
    intc.set_line(3, false).unwrap();
    intc.msi_write(msi.addr, msi.data).unwrap();
    // ...and a new wired route is refused while the file delivers.
    assert_eq!(
        intc.install_isr(&p, "/soc/gpio@10001000", 1, 3, |_| {})
            .unwrap_err(),
        IntcError::ImsicOwnsLevel {
            hart: 1,
            cause: MEIP
        }
    );
}

#[test]
fn guest_csrs_follow_vgein_and_geilen() {
    let p = Platform::from_dtb(GUESTS).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Supervisor, &ledger).unwrap();
    // guest-index-bits 2: three guest files per hart by default.
    assert_eq!(intc.geilen(0), 3);
    intc.set_hgeie(0, u64::MAX);
    assert_eq!(intc.hgeie(0), 0b1110);
    intc.limit_guest_files(2).unwrap();
    assert_eq!(intc.geilen(0), 2);
    // Enable bits of guests that are no longer implemented read zero.
    assert_eq!(intc.hgeie(0), 0b110);
    intc.set_hgeie(0, u64::MAX);
    assert_eq!(intc.hgeie(0), 0b110);
    assert!(matches!(
        intc.limit_guest_files(5),
        Err(IntcError::NoSuchGuest { .. })
    ));
    // VGEIN 0, and 3 (past GEILEN), name no file.
    for v in [0, 3, 63] {
        intc.set_vgein(0, v);
        assert_eq!(
            intc.vs_imsic_csr_read(0, ISEL_EIDELIVERY),
            Err(IntcError::NoGuestFile { hart: 0, vgein: v })
        );
        assert!(intc.vstopei(0).is_err());
        assert!(intc.claim_vstopei(0).is_err());
    }
    // VGEIN is a 6-bit field.
    intc.set_vgein(0, 0x41);
    assert_eq!(intc.vgein(0), 1);
    intc.vs_imsic_csr_write(0, ISEL_EIDELIVERY, 1).unwrap();
    intc.vs_imsic_csr_write(0, ISEL_EIE0, 1 << 9).unwrap();
    // The page past the last implemented guest file decodes to nothing.
    assert!(intc.msi_write(S_FILES + 0x3000, 9).is_err());
    assert_eq!(intc.hgeip(0), 0);
    intc.msi_write(S_FILES + 0x1000, 9).unwrap();
    assert_eq!(intc.hgeip(0), 0b10);
    assert_eq!(intc.hyp_pending(0), (1 << SGEIP) | (1 << VSEIP));
    // A threshold at the identity silences hgeip; one above lets it pass.
    intc.vs_imsic_csr_write(0, ISEL_EITHRESHOLD, 9).unwrap();
    assert_eq!(intc.hgeip(0), 0);
    intc.vs_imsic_csr_write(0, ISEL_EITHRESHOLD, 10).unwrap();
    assert_eq!(intc.hgeip(0), 0b10);
    // hgeie gates SGEIP but not VSEIP.
    intc.set_hgeie(0, 0);
    assert_eq!(intc.hyp_pending(0), 1 << VSEIP);
    // Hart 1 and the hart's own S file are untouched.
    assert_eq!(intc.hgeip(1), 0);
    assert_eq!(intc.topei(0, Privilege::Supervisor).unwrap(), 0);
    assert_eq!(intc.vstopei(0).unwrap(), 0x90009);
    assert_eq!(intc.claim_vstopei(0).unwrap(), 0x90009);
    assert_eq!(intc.hgeip(0), 0);
    assert_eq!(
        intc.vs_imsic_csr_read(0, ISEL_EIP0).unwrap(),
        0,
        "the claim cleared the pending bit"
    );
    // A platform without guest files has GEILEN 0 and no writable hgeie.
    let q = Platform::from_dtb(IMSIC).unwrap();
    let mut qi = RiscvIntc::new(&q, Privilege::Supervisor, &ledger).unwrap();
    assert_eq!(qi.geilen(0), 0);
    qi.set_hgeie(0, u64::MAX);
    assert_eq!(qi.hgeie(0), 0);
    qi.set_vgein(0, 1);
    assert!(qi.vs_imsic_csr_read(0, ISEL_EIDELIVERY).is_err());
}

#[test]
fn vs_level_claims_run_the_guest_handlers() {
    let p = Platform::from_dtb(GUESTS).unwrap();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Supervisor, &ledger).unwrap();
    intc.limit_guest_files(2).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mk = |tag: u32| {
        let seen = seen.clone();
        move |e: &vesper_silicon::intc::IrqEvent<'_>| {
            seen.borrow_mut()
                .push((tag, e.hart, e.cause, e.source, e.intc.vgein(e.hart)));
        }
    };
    let a = intc.install_guest_msi(0, 1, mk(1)).unwrap();
    let b = intc.install_guest_msi(0, 2, mk(2)).unwrap();
    let c = intc.install_guest_msi(1, 1, mk(3)).unwrap();
    assert_eq!(a.addr, S_FILES + 0x1000);
    assert_eq!(b.addr, S_FILES + 0x2000);
    assert_eq!(c.addr, S_FILES + 0x4000 + 0x1000);
    assert_eq!((a.data, b.data, c.data), (1, 1, 1));
    assert_eq!(intc.hgeie(0), 0b110);
    // The hypervisor's own VGEIN survives the VS-level claims.
    intc.set_vgein(0, 2);
    intc.msi_write(b.addr, b.data).unwrap();
    intc.msi_write(a.addr, a.data).unwrap();
    intc.msi_write(c.addr, c.data).unwrap();
    assert_eq!(
        *seen.borrow(),
        vec![
            (2, 0, VSEIP, 1, 2),
            (1, 0, VSEIP, 1, 1),
            (3, 1, VSEIP, 1, 1)
        ]
    );
    assert_eq!(intc.vgein(0), 2);
    assert_eq!(intc.hgeip(0), 0);
    let log = intc.guest_deliveries();
    assert_eq!(log.len(), 3);
    assert_eq!((log[0].guest, log[1].guest, log[2].hart), (2, 1, 1));
    // While the hart's interrupts are masked, SGEIP waits; both guests
    // are served, lowest guest first, when it unmasks.
    seen.borrow_mut().clear();
    let guard = intc.cpu(0).unwrap().disable_local_save();
    intc.msi_write(b.addr, b.data).unwrap();
    intc.msi_write(a.addr, a.data).unwrap();
    assert!(seen.borrow().is_empty());
    assert_eq!(intc.hgeip(0), 0b110);
    intc.cpu(0).unwrap().restore_local(guard).unwrap();
    let tags: Vec<u32> = seen.borrow().iter().map(|s| s.0).collect();
    assert_eq!(tags, vec![1, 2]);
    // Errors: guest 0 and past GEILEN; M-mode software.
    assert_eq!(
        intc.install_guest_msi(0, 3, |_| {}).unwrap_err(),
        IntcError::NoSuchGuest { hart: 0, guest: 3 }
    );
    assert!(intc.install_guest_msi(0, 0, |_| {}).is_err());
    assert!(intc.install_guest_msi(9, 1, |_| {}).is_err());
    let mut m = RiscvIntc::new(&p, Privilege::Machine, &ledger).unwrap();
    assert_eq!(
        m.install_guest_msi(0, 1, |_| {}).unwrap_err(),
        IntcError::GuestNeedsSupervisor
    );
    // Identities in one guest file are allocated lowest first and run out.
    let mut n = 1;
    loop {
        match intc.install_guest_msi(1, 2, |_| {}) {
            Ok(msi) => {
                assert_eq!(msi.data, n);
                n += 1;
            }
            Err(e) => {
                assert_eq!(e, IntcError::NoFreeIdentity { hart: 1 });
                break;
            }
        }
    }
    assert_eq!(n - 1, 255);
}

#[test]
fn msi_map_routes_requester_ids() {
    let p = hyp();
    let route = |host: &str, rid: u32| p.pci_msi_route(host, rid);
    let r = |c: &str, id: u32, via: &str| MsiRoute {
        controller: c.to_string(),
        device_id: id,
        via: via.to_string(),
    };
    let rid = requester_id(0, 1, 0).unwrap();
    assert_eq!(rid, 0x08);
    assert_eq!(route(PCI0, rid).unwrap(), r(IMSIC_S, 0x108, PCI0));
    // msi-map-mask 0x7fff drops bit 15 before matching.
    assert_eq!(route(PCI0, 0x8008).unwrap(), r(IMSIC_S, 0x108, PCI0));
    let bus1 = requester_id(1, 0x1f, 7).unwrap();
    assert_eq!(bus1, 0x1ff);
    assert_eq!(route(PCI0, bus1).unwrap(), r(IMSIC_M, 0xff, PCI0));
    // Bus 2 has no entry: no MSI controller at all.
    assert_eq!(
        route(PCI0, 0x200).unwrap_err(),
        DtbError::NoMsiMapEntry {
            node: PCI0.to_string(),
            rid: 0x200
        }
    );
    // msi-parent passes the requester ID through.
    assert_eq!(route(PCI1, 0x1234).unwrap(), r(IMSIC_S, 0x1234, PCI1));
    assert_eq!(
        route("/soc/pci@32000000", 0).unwrap_err(),
        DtbError::NoMsiController {
            node: "/soc/pci@32000000".to_string()
        }
    );
    // Malformed maps are typed errors.
    for host in [
        "/soc/pci@33000000",
        "/soc/pci@34000000",
        "/soc/pci@35000000",
        "/soc/pci@37000000",
    ] {
        assert!(
            matches!(route(host, 0), Err(DtbError::BadProperty { .. })),
            "{host}: {:?}",
            route(host, 0)
        );
    }
    assert!(matches!(
        route("/soc/pci@36000000", 0),
        Err(DtbError::UnknownPhandle { phandle: 0x777, .. })
    ));
    // Bad inputs.
    assert!(route(PCI0, 0x1_0000).is_err());
    assert!(route("/soc/serial@10000000", 0).is_err());
    assert!(route("/nope", 0).is_err());
    assert!(requester_id(0, 32, 0).is_err());
    assert!(requester_id(0, 0, 8).is_err());
    let (entries, mask) = p.msi_map(PCI0).unwrap().unwrap();
    assert_eq!(mask, 0x7fff);
    assert_eq!(entries.len(), 2);
    assert_eq!(p.msi_map(PCI1).unwrap(), None);

    // QEMU 8.2.2 AArch64 virt: msi-map = <0 &its 0 0x10000> to the GICv3
    // ITS; the device ID is the requester ID.
    let a = Platform::from_dtb(ARM).unwrap();
    let host = a
        .tree()
        .nodes()
        .iter()
        .enumerate()
        .map(|(i, _)| a.tree().path(i))
        .find(|path| path.starts_with("/pcie@"))
        .unwrap();
    let its = a.pci_msi_route(&host, 0x0110).unwrap();
    assert!(its.controller.contains("its@"), "{its:?}");
    assert_eq!(its.device_id, 0x0110);
    // QEMU 8.2.2 riscv64 virt: msi-parent on the host to the S IMSIC.
    let q = Platform::from_dtb(IMSIC).unwrap();
    let s = q.pci_msi_route("/soc/pci@30000000", 0x08).unwrap();
    assert_eq!(s.controller, "/soc/imsics@28000000");
    assert_eq!(s.device_id, 0x08);
}

#[test]
fn pci_msis_go_through_dma_ranges_into_guest_files() {
    let p = hyp();
    let ledger = Ledger::new();
    let mut intc = RiscvIntc::new(&p, Privilege::Supervisor, &ledger).unwrap();
    // The host's dma-ranges: bus 0xf000_0000 is the S IMSIC; bus 0 DRAM.
    let w = p.pci_dma_windows(PCI0).unwrap();
    assert_eq!(w.len(), 2);
    assert_eq!(
        p.pci_cpu_address(PCI0, 0xf000_1000, 4).unwrap(),
        0x2800_1000
    );
    // A host without dma-ranges is identity.
    assert_eq!(p.pci_cpu_address(PCI1, S_FILES, 4).unwrap(), S_FILES);

    let seen = Rc::new(RefCell::new(Vec::new()));
    let s2 = seen.clone();
    let msi = intc
        .install_guest_msi(0, 1, move |e| s2.borrow_mut().push((e.cause, e.source)))
        .unwrap();
    assert_eq!(msi.addr, 0x2800_1000);
    let bus = 0xf000_0000 + (msi.addr - S_FILES);
    let rid = requester_id(0, 2, 0).unwrap();
    let rec = intc.pci_msi_write(&p, PCI0, rid, bus, msi.data).unwrap();
    assert_eq!(*seen.borrow(), vec![(VSEIP, msi.data)]);
    assert_eq!(rec.device_id, 0x110);
    assert_eq!(rec.cpu_addr, msi.addr);
    assert_eq!(rec.imsic.as_deref(), Some(IMSIC_S));
    assert_eq!((rec.hart, rec.guest), (Some(0), Some(1)));
    assert!(rec.in_parent);
    // A bus address outside both windows.
    assert!(matches!(
        intc.pci_msi_write(&p, PCI0, rid, 0x5000_0000, msi.data),
        Err(IntcError::Dtb(DtbError::NotDmaAddressable { .. }))
    ));
    // Bus 1 maps to the M IMSIC, but the write lands in an S file: it is
    // delivered (no IOMMU) and flagged.
    let stray = intc.pci_msi_write(&p, PCI0, 0x100, 0xf000_0000, 5).unwrap();
    assert_eq!(stray.controller, IMSIC_M);
    assert!(!stray.in_parent);
    // A write into DRAM is no MSI: refused and counted lost. The CPU
    // address of the guest file, used as a bus address, is such a write
    // (bus 0x2800_1000 is DRAM 0xa800_1000 here).
    let before = intc.lost_msis().len();
    assert!(matches!(
        intc.pci_msi_write(&p, PCI0, rid, msi.addr, 5),
        Err(IntcError::Unmapped { addr: 0xa800_1000 })
    ));
    assert!(intc.pci_msi_write(&p, PCI0, rid, 0x1000, 5).is_err());
    assert_eq!(intc.lost_msis().len(), before + 2);
    // Misaligned, unrouted and unmodelled controllers.
    assert!(matches!(
        intc.pci_msi_write(&p, PCI0, rid, 0xf000_0002, 5),
        Err(IntcError::Misaligned { .. })
    ));
    assert!(intc.pci_msi_write(&p, PCI0, 0x300, 0xf000_0000, 5).is_err());
    assert_eq!(intc.pci_msis().len(), 2);
    let a = Platform::from_dtb(ARM).unwrap();
    let host = a
        .tree()
        .nodes()
        .iter()
        .enumerate()
        .map(|(i, _)| a.tree().path(i))
        .find(|path| path.starts_with("/pcie@"))
        .unwrap();
    // The ITS is not modelled here (and the platform is not RISC-V).
    assert!(RiscvIntc::new(&a, Privilege::Supervisor, &ledger).is_err());
    let _ = host;
    // On the QEMU riscv64 dump the host's msi-parent is the S IMSIC.
    let q = Platform::from_dtb(IMSIC).unwrap();
    let mut qi = RiscvIntc::new(&q, Privilege::Supervisor, &ledger).unwrap();
    qi.imsic_csr_write(0, Privilege::Supervisor, ISEL_EIDELIVERY, 1)
        .unwrap();
    qi.imsic_csr_write(0, Privilege::Supervisor, ISEL_EIE0, 1 << 9)
        .unwrap();
    let rec = qi
        .pci_msi_write(&q, "/soc/pci@30000000", 0x08, S_FILES, 9)
        .unwrap();
    assert!(rec.in_parent);
    assert_eq!(qi.topei(0, Privilege::Supervisor).unwrap(), 0x90009);
    assert!(qi.external_level(0, SEIP));
}

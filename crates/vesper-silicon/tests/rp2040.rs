//! The vendor RP2040 SVD: parse, access classes, hazards (trace and
//! Cranelift IR) and the generated typestate accessors.

mod common;

use common::{Mmio, mmio_fn};
use vesper_silicon::hazard::{
    MmioOp, MmioPolicy, RULE_RC_READ, RULE_RMW_W1C, check_function, check_ops,
};
use vesper_silicon::mmio::{BusError, ConsumeWrite, Reg, SimBus};
use vesper_silicon::pac::{PacOptions, generate_rust};
use vesper_silicon::svd::{DeviceModel, FieldAccess, RegisterClass};

const SVD: &str = include_str!("../fixtures/rp2040/RP2040.svd");
const PAC: &str = include_str!("../fixtures/rp2040/pac.rs");

/// The checked-in accessors, compiled against this crate.
#[allow(dead_code)]
mod pac {
    include!("../fixtures/rp2040/pac.rs");
}

fn model() -> DeviceModel {
    DeviceModel::from_svd(SVD).expect("RP2040.svd parses")
}

fn pac_options() -> PacOptions {
    PacOptions {
        peripherals: Some(["ADC", "SIO", "TIMER", "UART0"].map(String::from).to_vec()),
        source: "Source: fixtures/rp2040/RP2040.svd (Raspberry Pi pico-sdk 079c6f3, BSD-3-Clause)."
            .to_string(),
        ..PacOptions::default()
    }
}

#[test]
fn the_whole_vendor_file_parses_into_the_model() {
    let m = model();
    assert_eq!(m.name, "RP2040");
    assert_eq!(m.peripherals.len(), 37);
    let regs: Vec<_> = m.peripherals.iter().flat_map(|p| &p.registers).collect();
    assert_eq!(regs.len(), 1181);
    // Every register has its own address (no aliasing in the SimBus).
    let addrs: std::collections::BTreeSet<_> = regs.iter().map(|r| r.address).collect();
    assert_eq!(addrs.len(), regs.len());
    let fields = || regs.iter().flat_map(|r| &r.fields);
    // 302 oneToClear fields in the XML, plus the copies in derived
    // peripherals (UART1, SPI1, I2C1, PIO1 ...) after expansion.
    let w1c = fields()
        .filter(|f| f.access == FieldAccess::Write1ToClear)
        .count();
    assert_eq!(w1c, 329);
    assert_eq!(fields().filter(|f| f.access.consuming_read()).count(), 53);
    assert!(!fields().any(|f| f.access == FieldAccess::ReadToClear));
    // derivedFrom: UART1 has UART0's layout at its own base.
    let u0 = m.peripheral("UART0").unwrap();
    let u1 = m.peripheral("UART1").unwrap();
    assert_eq!(u0.registers.len(), u1.registers.len());
    assert_eq!(m.register("UART1.UARTDR").unwrap().address, u1.base_address);
}

#[test]
fn access_classes_of_real_registers() {
    let m = model();
    let class = |p: &str| m.register(p).unwrap().class();
    // ADC FIFO control/status: EN/SHIFT/ERR are read-write, OVER/UNDER W1C.
    let fcs = m.register("ADC.FCS").unwrap();
    assert_eq!(
        fcs.field("OVER").unwrap().access,
        FieldAccess::Write1ToClear
    );
    assert_eq!(
        fcs.field("UNDER").unwrap().access,
        FieldAccess::Write1ToClear
    );
    assert_eq!(fcs.field("EN").unwrap().access, FieldAccess::ReadWrite);
    assert_eq!(class("ADC.FCS"), RegisterClass::WriteClear);
    // Reading the ADC FIFO pops it; it is read-only.
    assert_eq!(class("ADC.FIFO"), RegisterClass::ReadToClear);
    assert_eq!(class("SIO.FIFO_RD"), RegisterClass::ReadToClear);
    // TIMELR latches TIMEHR when read.
    assert_eq!(class("TIMER.TIMELR"), RegisterClass::ReadToClear);
    // UART/SPI data registers and hardware spinlocks: consuming read, plain write.
    assert_eq!(class("UART0.UARTDR"), RegisterClass::ConsumeWrite);
    assert_eq!(class("SPI0.SSPDR"), RegisterClass::ConsumeWrite);
    assert_eq!(class("SIO.SPINLOCK7"), RegisterClass::ConsumeWrite);
    assert_eq!(class("SIO.FIFO_WR"), RegisterClass::WriteOnly);
    assert_eq!(class("TIMER.INTR"), RegisterClass::WriteClear);
    assert_eq!(class("ADC.CS"), RegisterClass::WriteClear); // ERR_STICKY
    assert_eq!(class("ADC.DIV"), RegisterClass::ReadWrite);
}

#[test]
fn hazards_on_real_registers_in_traces() {
    let m = model();
    let ops = vec![
        // Setting EN by read-modify-write acknowledges pending OVER/UNDER.
        MmioOp::Modify {
            reg: "ADC.FCS".into(),
            clear: 0,
            set: 1,
        },
        // Peeking the FIFO pops a sample.
        MmioOp::Read {
            reg: "ADC.FIFO".into(),
        },
        MmioOp::Read {
            reg: "SIO.SPINLOCK0".into(),
        },
        // The accepted shapes.
        MmioOp::Consume {
            reg: "ADC.FIFO".into(),
        },
        MmioOp::Write {
            reg: "ADC.FCS".into(),
            value: 1 << 11,
        },
        MmioOp::Write {
            reg: "SIO.SPINLOCK0".into(),
            value: 1,
        },
    ];
    let f = check_ops(&m, "adc_drain", &ops);
    // Any write to the read-only FIFO, even of zero, is flagged.
    let w = check_ops(
        &m,
        "poke",
        &[MmioOp::Write {
            reg: "ADC.FIFO".into(),
            value: 0,
        }],
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].rule, vesper_silicon::hazard::RULE_WRITE_RO);
    let rules: Vec<_> = f.iter().map(|x| (x.inst, x.rule)).collect();
    assert_eq!(
        rules,
        [
            (Some(0), RULE_RMW_W1C),
            (Some(1), RULE_RC_READ),
            (Some(2), RULE_RC_READ)
        ],
        "{f:?}"
    );
    assert!(f[0].message.contains("OVER") && f[0].message.contains("UNDER"));
}

#[test]
fn hazards_on_real_registers_in_cranelift_ir() {
    let m = model();
    let fcs = m.register("ADC.FCS").unwrap().address;
    let fifo = m.register("ADC.FIFO").unwrap().address;
    let f = mmio_fn(
        "adc_irq",
        &[
            Mmio::Rmw(fcs, 1),
            Mmio::Load(fifo),
            Mmio::StoreConst(fcs, 1 << 10),
        ],
    );
    let r = check_function(&f, &m, &MmioPolicy::default());
    let rules: Vec<_> = r.findings.iter().map(|x| x.rule).collect();
    assert_eq!(rules, [RULE_RMW_W1C, RULE_RC_READ], "{:?}", r.findings);
    // Declaring the function a consumer of ADC.FIFO accepts the pop.
    let mut pol = MmioPolicy::default();
    pol.consumers
        .insert("ADC.FIFO".into(), vec!["adc_irq".into()]);
    let r = check_function(&f, &m, &pol);
    assert_eq!(r.findings.len(), 1, "{:?}", r.findings);
    assert_eq!(r.mmio_accesses, 4);
}

#[test]
fn generated_accessors_are_up_to_date() {
    let fresh = generate_rust(&model(), &pac_options()).unwrap();
    if std::env::var_os("VESPER_REGEN_PAC").is_some() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        std::fs::write(dir.join("fixtures/rp2040/pac.rs"), &fresh).unwrap();
        return;
    }
    assert_eq!(
        fresh, PAC,
        "fixtures/rp2040/pac.rs is stale; regenerate it with pac::generate_rust"
    );
    // The whole device generates without identifier collisions.
    let all = generate_rust(&model(), &PacOptions::default()).unwrap();
    assert_eq!(all.matches("\npub mod ").count(), 37);
}

#[test]
fn generated_accessors_drive_the_software_bus() {
    let bus = SimBus::new(&model());
    let adc = pac::adc::Adc::take(&bus).unwrap();
    assert!(matches!(
        pac::adc::Adc::take(&bus),
        Err(BusError::AlreadyTaken(_))
    ));
    // Both FIFO error flags pending plus EN set; acknowledge OVER only.
    adc.fcs.write_clear_mask(0).unwrap_err();
    bus.borrow_mut()
        .hw_set(
            pac::adc::fcs::PATH,
            pac::adc::fcs::OVER_MASK | pac::adc::fcs::UNDER_MASK | pac::adc::fcs::EN_MASK,
        )
        .unwrap();
    adc.fcs.write_clear_mask(pac::adc::fcs::OVER_MASK).unwrap();
    let v = adc.fcs.read();
    assert_eq!(v & pac::adc::fcs::OVER_MASK, 0);
    assert_ne!(v & pac::adc::fcs::UNDER_MASK, 0, "UNDER must survive");
    assert_ne!(v & pac::adc::fcs::EN_MASK, 0, "EN must survive");
    assert_eq!(pac::adc::fcs::OVER_SHIFT, 11);
    assert_eq!(pac::adc::BASE_ADDRESS, 0x4004_c000);

    let uart = pac::uart0::Uart0::take(&bus).unwrap();
    uart.uartdr.write(0x41).unwrap();
    let _ = uart.uartdr.consume();
    let sio = pac::sio::Sio::take(&bus).unwrap();
    let _claimed = sio.spinlock3.consume();
    sio.spinlock3.write(1).unwrap();
    let _ = sio.fifo_rd.consume();
    let timer = pac::timer::Timer::take(&bus).unwrap();
    let _ = timer.timelr.consume();
    let _ = timer.timehr.read();

    // Binding a register with the wrong class fails at run time too.
    assert!(matches!(
        Reg::<ConsumeWrite>::bind(&bus, "ADC.CS"),
        Err(BusError::ClassMismatch { .. })
    ));
}

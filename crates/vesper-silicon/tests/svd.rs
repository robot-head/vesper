//! SVD parsing, register model and MMIO hazard checks.

mod common;

use common::{Mmio, mmio_fn};
use vesper_silicon::hazard::*;
use vesper_silicon::svd::{DeviceModel, FieldAccess, RegisterClass, SvdError};

fn model() -> DeviceModel {
    DeviceModel::from_svd(include_str!("../fixtures/vsim32.svd")).expect("fixture parses")
}

#[test]
fn fixture_parses_into_access_kinds() {
    let m = model();
    assert_eq!(m.name, "VSIM32");
    let names: Vec<_> = m.peripherals.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["SPI1", "SPI2", "ETHMAC", "UART0"]);

    let sr = m.register("SPI1.SR").unwrap();
    assert_eq!(sr.address, 0x4001_3008);
    assert_eq!(sr.reset_value, 2);
    assert_eq!(sr.field("RXNE").unwrap().access, FieldAccess::ReadOnly);
    assert_eq!(
        sr.field("CRCERR").unwrap().access,
        FieldAccess::Write1ToClear
    );
    assert_eq!(sr.class(), RegisterClass::WriteClear);
    assert_eq!(m.register("SPI1.DR").unwrap().size_bits, 16);
    assert_eq!(
        m.register("SPI1.CR1").unwrap().field("BR").unwrap().mask(),
        0b111 << 3
    );

    // derivedFrom copies the register layout to the new base address.
    let spi2 = m.register("SPI2.SR").unwrap();
    assert_eq!(spi2.address, 0x4000_3808);
    assert_eq!(spi2.fields, sr.fields);

    // Register-level modifiedWriteValues applies to every field.
    let irq = m.register("ETHMAC.IRQ_STATUS").unwrap();
    assert!(
        irq.fields
            .iter()
            .all(|f| f.access == FieldAccess::Write1ToClear)
    );
    // A register without fields gets one implicit full-width field.
    let id = m.register("ETHMAC.MAC_ID").unwrap();
    assert_eq!(id.fields.len(), 1);
    assert_eq!(id.class(), RegisterClass::ReadOnly);

    let rx = m.register("UART0.RXDATA").unwrap();
    assert!(
        rx.fields
            .iter()
            .all(|f| f.access == FieldAccess::ReadToClear)
    );
    assert_eq!(rx.class(), RegisterClass::ReadToClear);
    assert_eq!(
        m.register("UART0.TXDATA").unwrap().class(),
        RegisterClass::WriteOnly
    );
    let ctrl = m.register("UART0.CTRL").unwrap();
    assert_eq!(
        ctrl.field("FLUSH").unwrap().access,
        FieldAccess::Write1ToSet
    );
    assert_eq!(ctrl.class(), RegisterClass::ReadWrite);
}

#[test]
fn malformed_and_unsupported_svd_is_rejected() {
    assert!(matches!(
        DeviceModel::from_svd("<device>"),
        Err(SvdError::Parse(_))
    ));
    assert!(matches!(DeviceModel::from_svd(""), Err(SvdError::Parse(_))));
    let base = include_str!("../fixtures/vsim32.svd");
    let zero_to_set = base.replacen(
        "<modifiedWriteValues>oneToSet</modifiedWriteValues>",
        "<modifiedWriteValues>zeroToSet</modifiedWriteValues>",
        1,
    );
    assert!(matches!(
        DeviceModel::from_svd(&zero_to_set),
        Err(SvdError::UnsupportedAccess { .. })
    ));
    let read_set = base.replacen(
        "<readAction>clear</readAction>",
        "<readAction>set</readAction>",
        1,
    );
    assert!(matches!(
        DeviceModel::from_svd(&read_set),
        Err(SvdError::UnsupportedAccess { .. })
    ));
    let overlap = base.replacen(
        "<field><name>CPOL</name><bitOffset>1</bitOffset>",
        "<field><name>CPOL</name><bitOffset>0</bitOffset>",
        1,
    );
    assert!(matches!(
        DeviceModel::from_svd(&overlap),
        Err(SvdError::FieldOverlap { .. })
    ));
}

fn op_rules(ops: &[MmioOp]) -> Vec<&'static str> {
    check_ops(&model(), "drv", ops)
        .into_iter()
        .map(|f| f.rule)
        .collect()
}

#[test]
fn trace_hazards() {
    let r = |s: &str| s.to_string();
    // Safe acknowledge of one pending bit.
    assert!(
        op_rules(&[MmioOp::Write {
            reg: r("ETHMAC.IRQ_STATUS"),
            value: 1
        }])
        .is_empty()
    );
    // RMW of a W1C register clears every pending sibling.
    assert_eq!(
        op_rules(&[MmioOp::Modify {
            reg: r("ETHMAC.IRQ_STATUS"),
            clear: 0,
            set: 1
        }]),
        [RULE_RMW_W1C]
    );
    // RMW of a plain register is fine.
    assert!(
        op_rules(&[MmioOp::Modify {
            reg: r("SPI1.CR1"),
            clear: 0,
            set: 1 << 6
        }])
        .is_empty()
    );
    // Read-to-clear must be consumed explicitly.
    assert_eq!(
        op_rules(&[MmioOp::Read {
            reg: r("UART0.RXDATA")
        }]),
        [RULE_RC_READ]
    );
    assert!(
        op_rules(&[MmioOp::Consume {
            reg: r("UART0.RXDATA")
        }])
        .is_empty()
    );
    // Writes to read-only bits / registers.
    assert_eq!(
        op_rules(&[MmioOp::Write {
            reg: r("ETHMAC.MAC_ID"),
            value: 0
        }]),
        [RULE_WRITE_RO]
    );
    assert_eq!(
        op_rules(&[MmioOp::Write {
            reg: r("UART0.CTRL"),
            value: 0x10
        }]),
        [RULE_WRITE_RO]
    );
    assert!(
        op_rules(&[MmioOp::Write {
            reg: r("UART0.CTRL"),
            value: 0x101
        }])
        .is_empty()
    );
    assert_eq!(
        op_rules(&[MmioOp::Read {
            reg: r("UART0.TXDATA")
        }]),
        [RULE_READ_WO]
    );
    assert_eq!(
        op_rules(&[MmioOp::Write {
            reg: r("SPI1.DR"),
            value: 0x1_0000
        }]),
        [RULE_RANGE]
    );
    assert_eq!(
        op_rules(&[MmioOp::Read { reg: r("NOPE.X") }]),
        [RULE_UNKNOWN]
    );
    let f = check_ops(
        &model(),
        "drv",
        &[
            MmioOp::Read { reg: r("SPI1.SR") },
            MmioOp::Modify {
                reg: r("SPI1.SR"),
                clear: 0,
                set: 0,
            },
        ],
    );
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].inst, Some(1));
}

#[test]
fn cranelift_mmio_pass() {
    let m = model();
    let pol = MmioPolicy {
        consumers: [("UART0.RXDATA".to_string(), vec!["uart_pop".to_string()])].into(),
    };
    let check = |name: &str, ops: &[Mmio]| -> Vec<&'static str> {
        let rep = check_function(&mmio_fn(name, ops), &m, &pol);
        rep.findings.into_iter().map(|f| f.rule).collect()
    };
    assert!(check("ack", &[Mmio::StoreConst(0x4002_8010, 1)]).is_empty());
    assert_eq!(
        check("ack_rmw", &[Mmio::Rmw(0x4002_8010, 1)]),
        [RULE_RMW_W1C]
    );
    assert!(check("enable", &[Mmio::Rmw(0x4001_3000, 1 << 6)]).is_empty());
    assert_eq!(check("peek", &[Mmio::Load(0x4000_4000)]), [RULE_RC_READ]);
    assert!(check("uart_pop", &[Mmio::Load(0x4000_4000)]).is_empty());
    assert_eq!(
        check("ro", &[Mmio::StoreConst(0x4002_8000, 7)]),
        [RULE_WRITE_RO]
    );
    assert_eq!(
        check("ro_bits", &[Mmio::StoreConst(0x4000_4008, 0x30)]),
        [RULE_WRITE_RO]
    );
    assert_eq!(
        check("wild", &[Mmio::StoreConst(0x5000_0000, 1)]),
        [RULE_UNKNOWN]
    );
    // Extending loads and truncating stores are MMIO accesses too.
    assert_eq!(check("peek8", &[Mmio::Uload8(0x4000_4000)]), [RULE_RC_READ]);
    assert_eq!(
        check("ro8", &[Mmio::Istore8(0x4002_8000, 1)]),
        [RULE_WRITE_RO]
    );
    assert!(check("ack8", &[Mmio::Istore8(0x4002_8010, 1)]).is_empty());
    // An RMW split across blocks through a block parameter is still an RMW.
    assert_eq!(
        check("split", &[Mmio::RmwAcrossBlocks(0x4002_8010, 1)]),
        [RULE_RMW_W1C]
    );
    assert!(check("split_ok", &[Mmio::RmwAcrossBlocks(0x4001_3000, 1 << 6)]).is_empty());
    // Load via base + immediate offset resolves the same register.
    let rep = check_function(&mmio_fn("sr", &[Mmio::Load(0x4001_3008)]), &m, &pol);
    assert_eq!(rep.mmio_accesses, 1);
    assert!(rep.findings.is_empty());
}

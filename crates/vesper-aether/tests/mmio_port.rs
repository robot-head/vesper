//! The Vesper virtio-mmio register file's status state machine and feature
//! negotiation against the rules of virtio-v1.2 §3.1.1 and §4.2.2 (the Rust
//! transport it replaced is compared in src/port_reference_tests/mmio.rs).

use vesper_aether::devices::block::{BlockDevice, SECTOR_BYTES};
use vesper_aether::devices::mmio::{MmioError, MmioTransport, VIRTIO_F_VERSION_1, reg, status};
use vesper_aether::guest_mem::GuestMemory;

fn make_transport() -> (GuestMemory, MmioTransport<BlockDevice>) {
    let mem = GuestMemory::new(&[(0, 64 * 1024)]).unwrap();
    let dev = BlockDevice::new(vec![0u8; 16 * SECTOR_BYTES as usize], false, "mmio-port").unwrap();
    (mem, MmioTransport::new(dev, 0))
}

fn w(
    t: &mut MmioTransport<BlockDevice>,
    m: &mut GuestMemory,
    off: u64,
    v: u32,
) -> Result<(), MmioError> {
    t.write(m, off, &v.to_le_bytes())
}

#[test]
fn feature_negotiation_matches_rust() {
    // Missing VIRTIO_F_VERSION_1 -> FEATURES_OK must be cleared by device.
    let (mut mem, mut t) = make_transport();
    w(
        &mut t,
        &mut mem,
        reg::STATUS,
        status::ACKNOWLEDGE | status::DRIVER,
    )
    .unwrap();
    w(&mut t, &mut mem, reg::DRIVER_FEATURES_SEL, 0).unwrap();
    w(&mut t, &mut mem, reg::DRIVER_FEATURES, 0).unwrap();
    w(&mut t, &mut mem, reg::DRIVER_FEATURES_SEL, 1).unwrap();
    w(&mut t, &mut mem, reg::DRIVER_FEATURES, 0).unwrap();
    let ok = status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK;
    w(&mut t, &mut mem, reg::STATUS, ok).unwrap();
    assert_eq!(t.status(), status::ACKNOWLEDGE | status::DRIVER);
    assert_eq!(t.stats().rejected_feature_sets, 1);
    assert_eq!(t.negotiated_features(), 0);

    // Now negotiate VIRTIO_F_VERSION_1 -> FEATURES_OK and DRIVER_OK succeed.
    let (mut mem2, mut t2) = make_transport();
    w(
        &mut t2,
        &mut mem2,
        reg::STATUS,
        status::ACKNOWLEDGE | status::DRIVER,
    )
    .unwrap();
    w(&mut t2, &mut mem2, reg::DRIVER_FEATURES_SEL, 1).unwrap();
    w(
        &mut t2,
        &mut mem2,
        reg::DRIVER_FEATURES,
        (VIRTIO_F_VERSION_1 >> 32) as u32,
    )
    .unwrap();
    let ok_bits = ok | status::DRIVER_OK;
    w(&mut t2, &mut mem2, reg::STATUS, ok_bits).unwrap();
    assert_eq!(t2.status(), ok_bits);
    assert_eq!(t2.negotiated_features(), VIRTIO_F_VERSION_1);
    assert_eq!(t2.stats().rejected_feature_sets, 0);
    assert_eq!(
        w(&mut t2, &mut mem2, reg::DRIVER_FEATURES, 0),
        Err(MmioError::FeaturesAfterFeaturesOk)
    );
}

#[test]
fn status_errors_and_reset_match_rust() {
    let (mut mem, mut t) = make_transport();

    // Driver cannot set DEVICE_NEEDS_RESET.
    assert_eq!(
        w(&mut t, &mut mem, reg::STATUS, status::DEVICE_NEEDS_RESET),
        Err(MmioError::DriverSetNeedsReset)
    );
    // Driver cannot set DRIVER_OK without FEATURES_OK.
    assert_eq!(
        w(&mut t, &mut mem, reg::STATUS, status::DRIVER_OK),
        Err(MmioError::DriverOkWithoutFeaturesOk)
    );

    // Status regression is rejected; 0 resets.
    let ad = status::ACKNOWLEDGE | status::DRIVER;
    w(&mut t, &mut mem, reg::STATUS, ad).unwrap();
    assert_eq!(
        w(&mut t, &mut mem, reg::STATUS, status::ACKNOWLEDGE),
        Err(MmioError::StatusRegression {
            from: ad,
            to: status::ACKNOWLEDGE
        })
    );
    assert_eq!(t.status(), ad);
    w(&mut t, &mut mem, reg::STATUS, 0).unwrap();
    assert_eq!(t.status(), 0);
}

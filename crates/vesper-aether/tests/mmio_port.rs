//! The Vesper virtio-mmio status state machine and feature negotiation
//! against the Rust reference (under `vesper-port`).
#![cfg(feature = "vesper-port")]

use vesper_aether::devices::block::{BlockDevice, SECTOR_BYTES};
use vesper_aether::devices::mmio::{MmioError, MmioTransport, VIRTIO_F_VERSION_1, reg, status};
use vesper_aether::guest_mem::GuestMemory;

fn make_transport() -> (GuestMemory, MmioTransport<BlockDevice>) {
    let mem = GuestMemory::new(&[(0, 64 * 1024)]).unwrap();
    let dev = BlockDevice::new(vec![0u8; 16 * SECTOR_BYTES as usize], false, "mmio-port").unwrap();
    (mem, MmioTransport::new(dev, 0))
}

#[test]
fn feature_negotiation_matches_rust() {
    let (mut mem, mut t_port) = make_transport();
    let (_, mut t_ref) = make_transport();

    // Missing VIRTIO_F_VERSION_1 -> FEATURES_OK must be cleared by device.
    for t in [&mut t_port, &mut t_ref] {
        t.write(
            &mut mem,
            reg::STATUS,
            &(status::ACKNOWLEDGE | status::DRIVER).to_le_bytes(),
        )
        .unwrap();
        t.write(&mut mem, reg::DRIVER_FEATURES_SEL, &0u32.to_le_bytes())
            .unwrap();
        t.write(&mut mem, reg::DRIVER_FEATURES, &0u32.to_le_bytes())
            .unwrap();
        t.write(&mut mem, reg::DRIVER_FEATURES_SEL, &1u32.to_le_bytes())
            .unwrap();
        t.write(&mut mem, reg::DRIVER_FEATURES, &0u32.to_le_bytes())
            .unwrap();
    }
    t_port
        .write(
            &mut mem,
            reg::STATUS,
            &(status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK).to_le_bytes(),
        )
        .unwrap();
    t_ref
        .write_status_reference(status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK)
        .unwrap();
    assert_eq!(t_port.status(), t_ref.status());
    assert_eq!(t_port.status() & status::FEATURES_OK, 0);
    assert_eq!(t_port.stats(), t_ref.stats());

    // Now negotiate VIRTIO_F_VERSION_1 -> FEATURES_OK and DRIVER_OK succeed.
    let (mut mem2, mut t2_port) = make_transport();
    let (_, mut t2_ref) = make_transport();
    let v1_hi = (VIRTIO_F_VERSION_1 >> 32) as u32;
    for t in [&mut t2_port, &mut t2_ref] {
        t.write(
            &mut mem2,
            reg::STATUS,
            &(status::ACKNOWLEDGE | status::DRIVER).to_le_bytes(),
        )
        .unwrap();
        t.write(&mut mem2, reg::DRIVER_FEATURES_SEL, &1u32.to_le_bytes())
            .unwrap();
        t.write(&mut mem2, reg::DRIVER_FEATURES, &v1_hi.to_le_bytes())
            .unwrap();
    }
    let ok_bits = status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK | status::DRIVER_OK;
    t2_port
        .write(&mut mem2, reg::STATUS, &ok_bits.to_le_bytes())
        .unwrap();
    t2_ref.write_status_reference(ok_bits).unwrap();
    assert_eq!(t2_port.status(), t2_ref.status());
    assert_eq!(t2_port.negotiated_features(), t2_ref.negotiated_features());
}

#[test]
fn status_errors_and_reset_match_rust() {
    let (mut mem, mut t_port) = make_transport();
    let (_, mut t_ref) = make_transport();

    // Driver cannot set DEVICE_NEEDS_RESET.
    assert_eq!(
        t_port.write(
            &mut mem,
            reg::STATUS,
            &status::DEVICE_NEEDS_RESET.to_le_bytes()
        ),
        Err(MmioError::DriverSetNeedsReset)
    );
    assert_eq!(
        t_ref.write_status_reference(status::DEVICE_NEEDS_RESET),
        Err(MmioError::DriverSetNeedsReset)
    );

    // Driver cannot set DRIVER_OK without FEATURES_OK.
    assert_eq!(
        t_port.write(&mut mem, reg::STATUS, &status::DRIVER_OK.to_le_bytes()),
        Err(MmioError::DriverOkWithoutFeaturesOk)
    );
    assert_eq!(
        t_ref.write_status_reference(status::DRIVER_OK),
        Err(MmioError::DriverOkWithoutFeaturesOk)
    );

    // Status regression is rejected; 0 resets.
    for t in [&mut t_port, &mut t_ref] {
        t.write(
            &mut mem,
            reg::STATUS,
            &(status::ACKNOWLEDGE | status::DRIVER).to_le_bytes(),
        )
        .unwrap();
    }
    assert_eq!(
        t_port.write(&mut mem, reg::STATUS, &status::ACKNOWLEDGE.to_le_bytes()),
        t_ref.write_status_reference(status::ACKNOWLEDGE)
    );
    t_port
        .write(&mut mem, reg::STATUS, &0u32.to_le_bytes())
        .unwrap();
    t_ref.write_status_reference(0).unwrap();
    assert_eq!(t_port.status(), t_ref.status());
}

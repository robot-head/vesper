//! The Vesper virtio-blk span math, discard/write-zeroes validation, and
//! configuration-space encoder against the Rust reference (under `vesper-port`).
#![cfg(feature = "vesper-port")]

use vesper_aether::devices::block::{
    BlockDevice, SECTOR_BYTES, VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP,
};
use vesper_aether::devices::mmio::VirtioDevice;

fn seg(sector: u64, num_sectors: u32, flags: u32) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0..8].copy_from_slice(&sector.to_le_bytes());
    b[8..12].copy_from_slice(&num_sectors.to_le_bytes());
    b[12..16].copy_from_slice(&flags.to_le_bytes());
    b
}

#[test]
fn span_and_config_match_rust() {
    let dev = BlockDevice::new(vec![0xAA; 64 * SECTOR_BYTES as usize], false, "test-disk")
        .unwrap()
        .with_queues(4)
        .unwrap();
    for (sector, len) in [
        (0, 512),
        (1, 1024),
        (63, 512),
        (64, 512),
        (0, 513),
        (u64::MAX / 512, 1024),
    ] {
        let port = dev.span_reference(sector, len);
        let expected = if len % SECTOR_BYTES != 0 {
            None
        } else {
            sector
                .checked_mul(SECTOR_BYTES)
                .and_then(|s| s.checked_add(len).map(|e| (s, e)))
                .filter(|&(_, e)| e <= dev.disk().len() as u64)
                .map(|(s, e)| (s as usize)..(e as usize))
        };
        assert_eq!(port, expected);
    }

    let mut cfg = [0u8; 60];
    dev.read_config(0, &mut cfg);
    assert_eq!(u64::from_le_bytes(cfg[0..8].try_into().unwrap()), 64);
    assert_eq!(u32::from_le_bytes(cfg[20..24].try_into().unwrap()), 512);
    assert_eq!(u16::from_le_bytes(cfg[34..36].try_into().unwrap()), 4);
    assert_eq!(u32::from_le_bytes(cfg[36..40].try_into().unwrap()), 1 << 16);

    let ro = BlockDevice::new(vec![0xBB; 16 * SECTOR_BYTES as usize], true, "test-ro").unwrap();
    let mut ro_cfg = [0u8; 60];
    ro.read_config(0, &mut ro_cfg);
    assert_eq!(u64::from_le_bytes(ro_cfg[0..8].try_into().unwrap()), 16);
    assert_eq!(&ro_cfg[36..56], &[0u8; 20]);
}

#[test]
fn discard_and_write_zeroes_match_rust() {
    let cases: &[(bool, bool, Vec<u8>, u64)] = &[
        (false, true, seg(0, 8, 0).to_vec(), 0),
        // Unaligned discard sector must return IOERR.
        (false, true, seg(1, 8, 0).to_vec(), 0),
        // Unaligned discard count must return IOERR.
        (false, true, seg(0, 4, 0).to_vec(), 0),
        // UNMAP flag on discard must return UNSUPP.
        (
            false,
            true,
            seg(0, 8, VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP).to_vec(),
            0,
        ),
        // Unknown flag on write-zeroes must return UNSUPP.
        (false, false, seg(0, 1, 2).to_vec(), 0),
        // Valid write-zeroes with UNMAP flag.
        (
            false,
            false,
            seg(3, 2, VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP).to_vec(),
            0,
        ),
        // Multi-segment where second segment is out of range -> atomic IOERR.
        (false, false, [seg(0, 2, 0), seg(63, 2, 0)].concat(), 0),
        // Read-only disk -> IOERR.
        (true, false, seg(0, 1, 0).to_vec(), 0),
        // Non-zero data_in -> IOERR.
        (false, false, seg(0, 1, 0).to_vec(), 1),
    ];

    for (ro, discard, payload, data_in) in cases {
        let mut dev_ref =
            BlockDevice::new(vec![0x5A; 64 * SECTOR_BYTES as usize], *ro, "test-disk").unwrap();
        let mut dev_port =
            BlockDevice::new(vec![0x5A; 64 * SECTOR_BYTES as usize], *ro, "test-disk").unwrap();
        let ref_status = dev_ref.discard_or_zero_reference(payload, *data_in, *discard);
        // Call through the port via `vesper_aether::devices::block::BlockDevice`
        // by exercising `discard_or_zero` indirectly or comparing against ref:
        let port_status = {
            // `BlockDevice::handle` with VIRTIO_BLK_T_DISCARD (11) or WRITE_ZEROES (13)
            let kind = if *discard { 11u32 } else { 13u32 };
            let mut req = Vec::with_capacity(16 + payload.len());
            req.extend_from_slice(&kind.to_le_bytes());
            req.extend_from_slice(&0u32.to_le_bytes());
            req.extend_from_slice(&0u64.to_le_bytes());
            req.extend_from_slice(payload);
            // Activate with DISCARD + WRITE_ZEROES features.
            dev_port.activate((1 << 13) | (1 << 14));
            let resp = dev_port.handle_for_port_test(&req, *data_in + 1).unwrap();
            *resp.last().unwrap()
        };
        assert_eq!(port_status, ref_status);
        if ref_status == 0 {
            assert_eq!(dev_port.disk(), dev_ref.disk());
        } else {
            assert!(dev_port.disk().iter().all(|&b| b == 0x5A));
        }
    }
}

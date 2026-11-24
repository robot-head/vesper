//! The Vesper guest-RAM layout planner and GPA resolver against the Rust
//! reference (both builds are in the crate under `vesper-port`).
#![cfg(feature = "vesper-port")]

use vesper_aether::guest_mem::{GuestMemory, MAX_GUEST_RAM_BYTES, MAX_GUEST_REGIONS};

#[test]
fn layout_and_locate_match_rust() {
    let valid_layouts: &[&[(u64, u64)]] = &[
        &[(0, 4096)],
        &[(0x1000_0000, 8192), (0, 4096)],
        // Touching regions must be accepted as non-overlapping.
        &[(0, 4096), (4096, 8192), (16384, 4096)],
    ];
    for layout in valid_layouts {
        let port = GuestMemory::new(layout).unwrap();
        let reference = GuestMemory::new_reference(layout).unwrap();
        assert_eq!(port.layout(), reference.layout());
        for (gpa, len) in port.layout() {
            assert!(port.contains(gpa, len));
            assert!(port.locate_reference(gpa, len).is_ok());
            assert!(port.contains(gpa + 16, 64));
        }
    }

    let invalid_layouts: Vec<Vec<(u64, u64)>> = vec![
        vec![],
        vec![(0, 4096); MAX_GUEST_REGIONS + 1],
        vec![(0, 0)],
        vec![(128, 4096)],
        vec![(0, 4097)],
        vec![(u64::MAX - 4095, 8192)],
        vec![(0, 8192), (4096, 4096)],
        vec![(0, MAX_GUEST_RAM_BYTES), (MAX_GUEST_RAM_BYTES, 4096)],
    ];
    for layout in &invalid_layouts {
        assert_eq!(
            GuestMemory::new(layout).err(),
            GuestMemory::new_reference(layout).err()
        );
    }
}

#[test]
fn out_of_bounds_and_overflow_match_rust() {
    let mem = GuestMemory::new(&[(0x1000, 0x2000), (0x8000, 0x1000)]).unwrap();
    let probes = [
        (0x0, 0x1000),
        (0x1000, 0x2000),
        (0x1000, 0x2001),
        (0x2ff0, 0x20),
        (0x3000, 0x5000),
        (0x8000, 0x1000),
        (0x8ff0, 0x20),
        (u64::MAX - 4, 16),
    ];
    for (gpa, len) in probes {
        assert_eq!(
            mem.contains(gpa, len),
            mem.locate_reference(gpa, len).is_ok()
        );
    }
}

//! KVM slot-plan construction and verification (§37.5).
use vesper_aether::slot_plan::*;
use vesper_aether::*;
use vesper_loom_cas::{Domain, blob_id};

const P: u64 = PAGE_SIZE_4K;

fn region(slot: u32, gpa: u64, size: u64, hva: u64, flags: u32) -> PlannedRegion {
    PlannedRegion {
        slot,
        flags,
        guest_phys_addr: gpa,
        memory_size: size,
        userspace_addr: hva,
    }
}
fn verify(regions: Vec<PlannedRegion>) -> Result<VerifiedKvmSlotPlan, PlanError> {
    KvmSlotPlan::from_regions(7, regions)?.verify(&KvmHostLimits::default())
}

#[test]
fn accepts_adjacent_aligned_slots() {
    let plan = verify(vec![
        region(0, 0, 2 * P, 0x10_0000, 0),
        region(1, 2 * P, P, 0x10_0000 + 2 * P, KVM_MEM_READONLY),
        region(2, 0x10_0000, P, 0x20_0000, KVM_MEM_LOG_DIRTY_PAGES),
    ])
    .unwrap();
    assert_eq!(plan.vm_id(), 7);
    assert_eq!(plan.regions().len(), 3);
    assert!(plan.regions()[1].read_only());
}

#[test]
fn rejects_each_malformed_region() {
    let ok = region(3, P, P, 0x40_0000, 0);
    let cases: Vec<(PlannedRegion, PlanError)> = vec![
        (
            PlannedRegion {
                memory_size: 0,
                ..ok
            },
            PlanError::ZeroSized { slot: 3 },
        ),
        (
            PlannedRegion {
                guest_phys_addr: P + 1,
                ..ok
            },
            PlanError::Unaligned { slot: 3 },
        ),
        (
            PlannedRegion {
                userspace_addr: 0x40_0008,
                ..ok
            },
            PlanError::Unaligned { slot: 3 },
        ),
        (
            PlannedRegion {
                memory_size: P + 512,
                ..ok
            },
            PlanError::Unaligned { slot: 3 },
        ),
        (
            PlannedRegion {
                guest_phys_addr: u64::MAX - P + 1,
                ..ok
            },
            PlanError::Overflow { slot: 3 },
        ),
        (
            PlannedRegion {
                userspace_addr: u64::MAX - P + 1,
                ..ok
            },
            PlanError::Overflow { slot: 3 },
        ),
        (
            PlannedRegion {
                userspace_addr: 0,
                ..ok
            },
            PlanError::NullHost { slot: 3 },
        ),
        (
            PlannedRegion { flags: 4, ..ok },
            PlanError::UnknownFlags { slot: 3, flags: 4 },
        ),
        (
            PlannedRegion {
                flags: KVM_MEM_READONLY | KVM_MEM_LOG_DIRTY_PAGES,
                ..ok
            },
            PlanError::DirtyLogReadOnly { slot: 3 },
        ),
        (
            PlannedRegion { slot: 32, ..ok },
            PlanError::SlotIdOutOfRange { slot: 32, max: 32 },
        ),
        (
            PlannedRegion {
                guest_phys_addr: (1 << 36) - P,
                memory_size: 2 * P,
                ..ok
            },
            PlanError::GuestAddressTooWide { slot: 3, bits: 36 },
        ),
    ];
    for (bad, expected) in cases {
        assert_eq!(verify(vec![bad]).unwrap_err(), expected, "{bad:?}");
    }
    // The exact top of the guest address width is accepted.
    verify(vec![PlannedRegion {
        guest_phys_addr: (1 << 36) - P,
        ..ok
    }])
    .unwrap();
}

#[test]
fn rejects_overlaps_duplicates_and_capability_gaps() {
    let a = region(0, 0, 4 * P, 0x10_0000, 0);
    // Guest overlap that only a running-maximum check catches: b is nested in
    // a, and c starts after b ends but still inside a.
    let b = region(1, P, P, 0x20_0000, 0);
    let c = region(2, 3 * P, 2 * P, 0x30_0000, 0);
    assert_eq!(
        verify(vec![b, c, a]).unwrap_err(),
        PlanError::GuestOverlap { a: 0, b: 1 }
    );
    assert_eq!(
        verify(vec![
            region(0, 0, P, 0x10_0000, 0),
            region(1, 8 * P, P, 0x10_0000, 0)
        ])
        .unwrap_err(),
        PlanError::HostOverlap { a: 0, b: 1 }
    );
    assert_eq!(
        verify(vec![
            region(4, 0, P, 0x10_0000, 0),
            region(4, 8 * P, P, 0x20_0000, 0)
        ])
        .unwrap_err(),
        PlanError::DuplicateSlot { slot: 4 }
    );
    let no_ro = KvmHostLimits {
        readonly_mem: false,
        ..KvmHostLimits::default()
    };
    assert_eq!(
        KvmSlotPlan::from_regions(1, vec![region(0, 0, P, 0x10_0000, KVM_MEM_READONLY)])
            .unwrap()
            .verify(&no_ro)
            .unwrap_err(),
        PlanError::ReadOnlyUnsupported { slot: 0 }
    );
    let two = KvmHostLimits {
        max_slots: 2,
        ..KvmHostLimits::default()
    };
    let three: Vec<_> = (0..3)
        .map(|i| region(i, u64::from(i) * P, P, 0x10_0000 + u64::from(i) * P, 0))
        .collect();
    assert_eq!(
        KvmSlotPlan::from_regions(1, three)
            .unwrap()
            .verify(&two)
            .unwrap_err(),
        PlanError::TooManyRegions { count: 3, max: 2 }
    );
    assert_eq!(
        KvmSlotPlan::from_regions(1, vec![]).unwrap_err(),
        PlanError::Empty
    );
    let bad_limits = KvmHostLimits {
        guest_phys_bits: 65,
        ..KvmHostLimits::default()
    };
    assert_eq!(
        KvmSlotPlan::from_regions(1, vec![a])
            .unwrap()
            .verify(&bad_limits)
            .unwrap_err(),
        PlanError::InvalidLimits
    );
}

/// Exhaustive differential against the quadratic definition of overlap on a
/// small grid: the O(n log n) verifier must agree exactly.
#[test]
fn overlap_check_matches_pairwise_definition() {
    let spans: Vec<(u64, u64)> = (0..4u64)
        .flat_map(|s| (1..=3u64).map(move |l| (s, l)))
        .collect();
    let mut checked = 0;
    for (i, a) in spans.iter().enumerate() {
        for (j, b) in spans.iter().enumerate() {
            for c in &spans {
                let set = [*a, *b, *c];
                let pairwise = (0..3).any(|x| {
                    (x + 1..3).any(|y| {
                        let (s1, l1) = set[x];
                        let (s2, l2) = set[y];
                        s1 < s2 + l2 && s2 < s1 + l1
                    })
                });
                let regions: Vec<_> = set
                    .iter()
                    .enumerate()
                    .map(|(k, (s, l))| {
                        region(k as u32, s * P, l * P, 0x100_0000 * (k as u64 + 1), 0)
                    })
                    .collect();
                let result = verify(regions);
                assert_eq!(
                    matches!(result, Err(PlanError::GuestOverlap { .. })),
                    pairwise,
                    "{set:?} {i} {j}"
                );
                if !pairwise {
                    assert!(result.is_ok());
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 12 * 12 * 12);
}

#[test]
fn cross_vm_sharing_only_for_identical_read_only_ranges() {
    let shared = region(1, 0x10_0000, P, 0x80_0000, KVM_MEM_READONLY);
    let vm = |id: u64, extra: PlannedRegion| {
        KvmSlotPlan::from_regions(id, vec![region(0, 0, P, 0x10_0000 * (id + 1), 0), extra])
            .unwrap()
            .verify(&KvmHostLimits::default())
            .unwrap()
    };
    let a = vm(1, shared);
    let b = vm(2, shared);
    verify_cross_vm(&[&a, &b]).unwrap();
    assert_eq!(
        verify_cross_vm(&[&a, &a]).unwrap_err(),
        PlanError::DuplicateVm { vm_id: 1 }
    );
    let writable = vm(3, PlannedRegion { flags: 0, ..shared });
    assert!(matches!(
        verify_cross_vm(&[&a, &writable]).unwrap_err(),
        PlanError::CrossVmHostOverlap { .. }
    ));
    let partial = vm(
        4,
        PlannedRegion {
            memory_size: 2 * P,
            ..shared
        },
    );
    assert!(matches!(
        verify_cross_vm(&[&a, &partial]).unwrap_err(),
        PlanError::CrossVmHostOverlap { .. }
    ));
    // Private RAM of one VM aliasing another VM's private RAM.
    let alias = KvmSlotPlan::from_regions(5, vec![region(0, 0, P, 0x20_0000, 0)])
        .unwrap()
        .verify(&KvmHostLimits::default())
        .unwrap();
    assert_eq!(
        verify_cross_vm(&[&a, &alias]).unwrap_err(),
        PlanError::CrossVmHostOverlap {
            vm_a: 1,
            slot_a: 0,
            vm_b: 5,
            slot_b: 0
        }
    );
}

#[test]
fn snapshot_map_becomes_read_only_cas_and_private_slots() {
    let bytes = [3u8; PAGE_BYTES];
    let object = blob_id(Domain::Artifact, &bytes);
    let cas = CasPage {
        object,
        object_len: P,
        offset: 0,
    };
    let entries = vec![
        SlotSnapshot {
            slot: MemorySlot {
                vm_id: 9,
                slot_id: 0,
                guest_start: 0,
                host_virtual_start: 0x100_0000,
                byte_len: 2 * P,
                permission: Permission::Private,
            },
            pages: vec![PageMapping::Zero, PageMapping::PrivateCow(cas)],
        },
        SlotSnapshot {
            slot: MemorySlot {
                vm_id: 9,
                slot_id: 1,
                guest_start: 0x10_0000,
                host_virtual_start: 0x200_0000,
                byte_len: P,
                permission: Permission::ReadOnlyCas {
                    object,
                    object_len: P,
                    offset: 0,
                },
            },
            pages: vec![PageMapping::ReadOnlyCas(cas)],
        },
    ];
    let map = SnapshotMap::validate(&entries, Limits::default()).unwrap();
    let plan = map.kvm_slot_plan(9, true).unwrap();
    assert_eq!(
        plan.regions(),
        &[
            region(0, 0, 2 * P, 0x100_0000, KVM_MEM_LOG_DIRTY_PAGES),
            region(1, 0x10_0000, P, 0x200_0000, KVM_MEM_READONLY),
        ]
    );
    plan.verify(&KvmHostLimits::default()).unwrap();
    let plain = map.kvm_slot_plan(9, false).unwrap();
    assert_eq!(plain.regions()[0].flags, 0);
    assert_eq!(
        map.kvm_slot_plan(10, false).unwrap_err(),
        PlanError::UnknownVm { vm_id: 10 }
    );
}

//! The Vesper slot-plan verifier against the Rust one, on random plans that
//! hit every rule (both builds are in the crate under `vesper-port`).
#![cfg(feature = "vesper-port")]

use vesper_aether::slot_plan::{
    KVM_MEM_LOG_DIRTY_PAGES, KVM_MEM_READONLY, KvmHostLimits, KvmSlotPlan, PlannedRegion,
    verify_cross_vm, verify_cross_vm_reference,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Mostly well-formed, page-sized regions in a small space so overlaps,
/// duplicates and adjacency are common, with a few broken fields.
fn region(rng: &mut Rng) -> PlannedRegion {
    let page = 4096;
    let mut r = PlannedRegion {
        slot: rng.below(12) as u32,
        flags: match rng.below(6) {
            0 => KVM_MEM_READONLY,
            1 => KVM_MEM_LOG_DIRTY_PAGES,
            _ => 0,
        },
        guest_phys_addr: rng.below(32) * page,
        memory_size: (1 + rng.below(4)) * page,
        userspace_addr: (1 + rng.below(32)) * page,
    };
    match rng.below(40) {
        0 => r.memory_size = 0,
        1 => r.guest_phys_addr += 8,
        2 => r.userspace_addr = 0,
        3 => r.flags = 4,
        4 => r.flags = KVM_MEM_READONLY | KVM_MEM_LOG_DIRTY_PAGES,
        5 => r.guest_phys_addr = u64::MAX - page + 1,
        6 => r.userspace_addr = u64::MAX - page + 1,
        7 => r.guest_phys_addr = 1 << 40,
        8 => r.slot = 40,
        _ => {}
    }
    r
}

fn limits(rng: &mut Rng) -> KvmHostLimits {
    match rng.below(10) {
        0 => KvmHostLimits {
            max_slots: 4,
            readonly_mem: true,
            guest_phys_bits: 36,
        },
        1 => KvmHostLimits {
            max_slots: 32,
            readonly_mem: false,
            guest_phys_bits: 36,
        },
        2 => KvmHostLimits {
            max_slots: 32,
            readonly_mem: true,
            guest_phys_bits: 64,
        },
        3 => KvmHostLimits {
            max_slots: 32,
            readonly_mem: true,
            guest_phys_bits: [0, 65, 17][rng.below(3) as usize],
        },
        _ => KvmHostLimits::default(),
    }
}

#[test]
fn verify_matches_the_rust_verifier() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut accepted = 0;
    for case in 0..20_000 {
        let n = 1 + rng.below(6) as usize;
        let regions: Vec<PlannedRegion> = (0..n).map(|_| region(&mut rng)).collect();
        let lim = limits(&mut rng);
        let plan = KvmSlotPlan::from_regions(case, regions).unwrap();
        let port = plan.clone().verify(&lim);
        let reference = plan.verify_reference(&lim);
        assert_eq!(port, reference, "case {case}");
        accepted += usize::from(port.is_ok());
    }
    assert!(accepted > 500, "too few accepted plans: {accepted}");
}

#[test]
fn cross_vm_matches_the_rust_check() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut checked = 0;
    let mut refused = 0;
    for case in 0..20_000u64 {
        let vms = 1 + rng.below(3);
        let mut plans = Vec::new();
        for v in 0..vms {
            // A few distinct, aligned, non-overlapping regions per VM.
            let n = 1 + rng.below(3);
            let regions: Vec<PlannedRegion> = (0..n)
                .map(|i| PlannedRegion {
                    slot: i as u32,
                    flags: if rng.below(2) == 0 {
                        KVM_MEM_READONLY
                    } else {
                        0
                    },
                    guest_phys_addr: i * 0x10_0000,
                    memory_size: (1 + rng.below(2)) * 4096,
                    userspace_addr: (1 + 4 * i + rng.below(2)) * 4096 + v * rng.below(2) * 4096,
                })
                .collect();
            let id = if rng.below(20) == 0 { 0 } else { case * 8 + v };
            if let Ok(p) = KvmSlotPlan::from_regions(id, regions)
                .unwrap()
                .verify(&KvmHostLimits::default())
            {
                plans.push(p);
            }
        }
        let refs: Vec<_> = plans.iter().collect();
        let port = verify_cross_vm(&refs);
        let reference = verify_cross_vm_reference(&refs);
        assert_eq!(port, reference, "case {case}");
        checked += 1;
        refused += usize::from(port.is_err());
    }
    assert!(
        checked > 10_000 && refused > 1_000,
        "{checked} checked, {refused} refused"
    );
}

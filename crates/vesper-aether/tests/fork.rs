//! Copy-on-write snapshot fork and per-clone nonce reseeding (§37.3).
use std::collections::HashSet;
use vesper_aether::entropy::CloneEntropy;
use vesper_aether::fork::*;
use vesper_aether::page_source::PageSource;
use vesper_aether::{Error, PAGE_BYTES};

const GENID: VmGenIdLocation = VmGenIdLocation {
    page: 1,
    offset: 64,
};

fn memory(pages: usize) -> QuiescedMemory {
    QuiescedMemory {
        pages: (0..pages)
            .map(|i| {
                if i == 2 {
                    None
                } else {
                    Some(Box::new([i as u8 + 1; PAGE_BYTES]))
                }
            })
            .collect(),
        genid: GENID,
        host_tsc_khz: 2_400_000,
        virtio_mmio_count: 3,
    }
}
fn golden(auth: &mut ForkAuthority, root: &RootKey) -> SnapshotImage {
    let mut sealer = auth.boot_sealer(root).unwrap();
    sealer.next_nonce().unwrap();
    quiesce_and_snapshot(sealer, memory(4)).unwrap()
}

#[test]
fn genid_wire_is_24_le_bytes() {
    let w = VmGenIdWire {
        vm_gen_lo: 0x0102_0304_0506_0708,
        vm_gen_hi: 0x1112_1314_1516_1718,
        host_tsc_khz: 0x2122_2324,
        virtio_mmio_count: 0x3132_3334,
    };
    let b = w.encode();
    assert_eq!(b.len(), VMGENID_WIRE_BYTES);
    assert_eq!(&b[..8], &[8, 7, 6, 5, 4, 3, 2, 1]);
    assert_eq!(&b[16..20], &[0x24, 0x23, 0x22, 0x21]);
    assert_eq!(VmGenIdWire::decode(&b).unwrap(), w);
    assert_eq!(VmGenIdWire::decode(&b[..23]), Err(ForkError::WrongLength));
    assert_eq!(VmGenIdWire::decode(&[0; 25]), Err(ForkError::WrongLength));
}

#[test]
fn clones_share_pages_until_written() {
    let root = RootKey::new([5; 32]);
    let mut auth = ForkAuthority::new(0xfeed);
    let image = golden(&mut auth, &root);
    let (mut a, ta) = image.fork(&mut auth).unwrap();
    let (mut b, tb) = image.fork(&mut auth).unwrap();
    assert!(a.shares_page_with(&b, 0));
    assert!(a.shares_page_with(&b, 3));
    a.write(10, b"hello").unwrap();
    assert!(!a.is_shared(0));
    assert!(b.is_shared(0));
    assert!(!a.shares_page_with(&b, 0));
    let mut out = [0u8; 5];
    a.read(10, &mut out).unwrap();
    assert_eq!(&out, b"hello");
    b.read(10, &mut out).unwrap();
    assert_eq!(out, [1; 5]);
    // Writes spanning a page boundary copy both pages, and zero pages read as 0.
    b.write(3 * PAGE_BYTES - 2, &[9, 9, 9, 9]).unwrap();
    assert_eq!(b.private_page_count(), 2);
    let mut span = [0u8; 4];
    b.read(3 * PAGE_BYTES - 2, &mut span).unwrap();
    assert_eq!(span, [9; 4]);
    let mut z = [1u8; 8];
    a.read(2 * PAGE_BYTES, &mut z).unwrap();
    assert_eq!(z, [0; 8]);
    // Out-of-range spans fail before any page is copied.
    let before = a.private_page_count();
    assert_eq!(
        a.write(4 * PAGE_BYTES - 1, &[0, 0]),
        Err(ForkError::OutOfRange)
    );
    assert_eq!(a.write(usize::MAX, &[0]), Err(ForkError::OutOfRange));
    assert_eq!(a.private_page_count(), before);
    let ra = ta
        .reseed_after_fork(&root, &mut a, CloneEntropy::gather().unwrap())
        .unwrap();
    let rb = tb
        .reseed_after_fork(&root, &mut b, CloneEntropy::gather().unwrap())
        .unwrap();
    ra.fresh_sealer.retire();
    rb.fresh_sealer.retire();
}

#[test]
fn reseeding_writes_generation_and_derives_distinct_keys() {
    let root = RootKey::new([7; 32]);
    let mut auth = ForkAuthority::new(0xabcd);
    let image = golden(&mut auth, &root);
    let parent = image.snapshot_generation();
    let (mut clone, ticket) = image.fork(&mut auth).unwrap();
    assert!(!clone.is_reseeded());
    assert_eq!(clone.fill(0).unwrap_err(), Error::NotReseeded);
    let generation = ticket.generation();
    assert_ne!(generation, parent);
    let epoch = ticket
        .reseed_after_fork(&root, &mut clone, CloneEntropy::gather().unwrap())
        .unwrap();
    assert!(clone.is_reseeded());
    assert_eq!(epoch.fork_epoch_id, generation.lo);
    assert_eq!(
        clone.read_genid().unwrap(),
        VmGenIdWire {
            vm_gen_lo: generation.lo,
            vm_gen_hi: 0xabcd,
            host_tsc_khz: 2_400_000,
            virtio_mmio_count: 3,
        }
    );
    // Only the generation page was copied.
    assert_eq!(clone.private_page_count(), 1);
    assert!(!clone.is_shared(GENID.page));
    let (mut other, other_ticket) = image.fork(&mut auth).unwrap();
    let other_epoch = other_ticket
        .reseed_after_fork(&root, &mut other, CloneEntropy::gather().unwrap())
        .unwrap();
    assert_ne!(
        epoch.fresh_sealer.key_id(),
        other_epoch.fresh_sealer.key_id()
    );
    assert_ne!(other.read_genid().unwrap(), clone.read_genid().unwrap());
    epoch.fresh_sealer.retire();
    other_epoch.fresh_sealer.retire();
}

/// The acceptance property: no two sealers of one authority (the booted VM
/// and every clone of every generation) ever emit the same nonce.
#[test]
fn nonces_are_unique_across_clones_and_generations() {
    let root = RootKey::new([9; 32]);
    let mut auth = ForkAuthority::new(1);
    let mut seen: HashSet<[u8; 12]> = HashSet::new();
    let mut keys: HashSet<[u8; 32]> = HashSet::new();
    let mut sealer = auth.boot_sealer(&root).unwrap();
    assert!(keys.insert(sealer.key_id()));
    let mut emitted = 0;
    for generation in 0..4 {
        for _ in 0..16 {
            assert!(seen.insert(sealer.next_nonce().unwrap()));
            emitted += 1;
        }
        let image = quiesce_and_snapshot(sealer, memory(3)).unwrap();
        let mut clones = Vec::new();
        for _ in 0..32 {
            let (mut c, t) = image.fork(&mut auth).unwrap();
            let mut fresh = t
                .reseed_after_fork(&root, &mut c, CloneEntropy::gather().unwrap())
                .unwrap()
                .fresh_sealer;
            for _ in 0..16 {
                assert!(seen.insert(fresh.next_nonce().unwrap()));
                emitted += 1;
            }
            assert!(keys.insert(fresh.key_id()));
            clones.push(fresh);
        }
        // The last clone keeps running and is snapshotted next generation.
        sealer = clones.pop().unwrap();
        for s in clones {
            s.retire();
        }
        assert!(generation < 4);
    }
    sealer.retire();
    assert_eq!(seen.len(), emitted);
    assert_eq!(keys.len(), 1 + 4 * 32);
}

#[test]
fn tickets_bind_to_their_clone_and_epochs_run_out() {
    let root = RootKey::new([1; 32]);
    let mut auth = ForkAuthority::with_epoch_limit(2, 3);
    let image = golden(&mut auth, &root);
    let (mut a, ta) = image.fork(&mut auth).unwrap();
    let (mut b, tb) = image.fork(&mut auth).unwrap();
    assert_eq!(
        image.fork(&mut auth).unwrap_err(),
        ForkError::EpochExhausted
    );
    assert_eq!(
        auth.boot_sealer(&root).unwrap_err(),
        ForkError::EpochExhausted
    );
    let refused = *tb
        .reseed_after_fork(&root, &mut a, CloneEntropy::gather().unwrap())
        .unwrap_err();
    assert_eq!(refused.0, ForkError::TicketMismatch);
    assert!(!a.is_reseeded());
    let tb = refused.1;
    let sa = ta
        .reseed_after_fork(&root, &mut a, CloneEntropy::gather().unwrap())
        .unwrap();
    let sb = tb
        .reseed_after_fork(&root, &mut b, CloneEntropy::gather().unwrap())
        .unwrap();
    sa.fresh_sealer.retire();
    sb.fresh_sealer.retire();
}

#[test]
fn malformed_snapshot_layout_returns_the_sealer() {
    let root = RootKey::new([2; 32]);
    let mut auth = ForkAuthority::new(3);
    for (pages, genid, expected) in [
        (0, GENID, ForkError::EmptyImage),
        (1, GENID, ForkError::InvalidLayout),
        (
            2,
            VmGenIdLocation {
                page: 0,
                offset: PAGE_BYTES - VMGENID_WIRE_BYTES + 1,
            },
            ForkError::InvalidLayout,
        ),
        (
            2,
            VmGenIdLocation {
                page: 0,
                offset: usize::MAX,
            },
            ForkError::InvalidLayout,
        ),
    ] {
        let sealer = auth.boot_sealer(&root).unwrap();
        let mut m = memory(pages);
        m.genid = genid;
        let (error, sealer) = *quiesce_and_snapshot(sealer, m).unwrap_err();
        assert_eq!(error, expected);
        sealer.retire();
    }
}

#[test]
fn dropped_obligations_are_counted() {
    let root = RootKey::new([4; 32]);
    let mut auth = ForkAuthority::new(4);
    let image = golden(&mut auth, &root);
    let before = leaked_obligations();
    {
        let (_clone, _ticket) = image.fork(&mut auth).unwrap();
        let _sealer = auth.boot_sealer(&root).unwrap();
    }
    let after = leaked_obligations();
    assert!(after.tickets > before.tickets);
    assert!(after.sealers > before.sealers);
}

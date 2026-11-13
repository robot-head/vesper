//! Software RDMA model: registration, leases, use-after-deregister.

use vesper_chorus::rdma::{Access, RdmaError, RdmaFabric, WcStatus};

const RW: Access = Access { remote_write: true };
const RO: Access = Access {
    remote_write: false,
};

#[test]
fn host_buffer_is_immutable_until_the_completion() {
    let fabric = RdmaFabric::new(2);
    let src = fabric.register(0, b"batch-0001".to_vec(), RO).unwrap();
    let dst = fabric.register(1, vec![0; 16], RW).unwrap();
    let lease = fabric
        .post_write_with_imm(src.local_key(), 0, 10, dst.grant(), 3, 0xabcd)
        .unwrap();
    // Neither mutation nor deregistration while in flight.
    assert_eq!(
        fabric.write_local(&src, 0, b"XXXX"),
        Err(RdmaError::RegionBusy { in_flight: 1 })
    );
    let (err, src) = fabric.deregister(src).err().unwrap();
    assert_eq!(err, RdmaError::InFlight { in_flight: 1 });
    // Not complete until the RNIC runs.
    let lease = fabric.poll_wc(lease).err().unwrap();
    assert_eq!(fabric.progress(16), 1);
    let wc = fabric.poll_wc(lease).ok().unwrap().unwrap();
    assert_eq!((wc.status, wc.bytes), (WcStatus::Success, 10));
    let remote = fabric.read_remote(dst.grant()).unwrap();
    assert_eq!(&remote[3..13], b"batch-0001");
    let imm = fabric.poll_recv(1).unwrap();
    assert_eq!((imm.from, imm.imm, imm.bytes), (0, 0xabcd, 10));
    // Now the host owns the buffer again.
    fabric.write_local(&src, 0, b"batch-0002").unwrap();
    assert_eq!(fabric.deregister(src).unwrap(), b"batch-0002".to_vec());
    fabric.deregister(dst).unwrap();
    assert_eq!(fabric.leaks(), (0, 0));
}

#[test]
fn use_after_deregister_is_a_typed_error() {
    let fabric = RdmaFabric::new(2);
    let src = fabric.register(0, vec![7; 8], RO).unwrap();
    let key = src.local_key();
    fabric.deregister(src).unwrap();
    let dst = fabric.register(1, vec![0; 8], RW).unwrap();
    assert_eq!(
        fabric
            .post_write_with_imm(key, 0, 8, dst.grant(), 0, 0)
            .err(),
        Some(RdmaError::UseAfterDeregister)
    );
    // A stale remote grant: the target deregistered (and a new region took
    // its slot) before the write executed.
    let src = fabric.register(0, vec![7; 8], RO).unwrap();
    let stale = dst.grant();
    let lease = fabric
        .post_write_with_imm(src.local_key(), 0, 8, stale, 0, 0)
        .unwrap();
    fabric.deregister(dst).unwrap();
    let fresh = fabric.register(1, vec![0; 8], RW).unwrap();
    assert_eq!(
        fabric.await_wc(lease),
        Err(RdmaError::Completion(WcStatus::RemoteInvalidKey))
    );
    assert_eq!(
        fabric.read_remote(fresh.grant()).unwrap(),
        vec![0; 8],
        "new region untouched"
    );
    assert!(fabric.poll_recv(1).is_none());
    // The failed lease still unpinned the source.
    fabric.deregister(src).unwrap();
    fabric.deregister(fresh).unwrap();
    assert_eq!(fabric.leaks(), (0, 0));
}

#[test]
fn bounds_and_access_are_checked() {
    let fabric = RdmaFabric::new(2);
    let src = fabric.register(0, vec![1; 8], RO).unwrap();
    let ro = fabric.register(1, vec![0; 8], RO).unwrap();
    let small = fabric.register(1, vec![0; 4], RW).unwrap();
    assert!(matches!(
        fabric.post_write_with_imm(src.local_key(), 4, 8, small.grant(), 0, 0),
        Err(RdmaError::OutOfBounds { .. })
    ));
    assert!(matches!(
        fabric.post_write_with_imm(src.local_key(), u64::MAX, 2, small.grant(), 0, 0),
        Err(RdmaError::OutOfBounds { .. })
    ));
    let l = fabric
        .post_write_with_imm(src.local_key(), 0, 8, ro.grant(), 0, 0)
        .unwrap();
    assert_eq!(
        fabric.await_wc(l),
        Err(RdmaError::Completion(WcStatus::RemoteAccess))
    );
    let l = fabric
        .post_write_with_imm(src.local_key(), 0, 8, small.grant(), 0, 0)
        .unwrap();
    assert_eq!(
        fabric.await_wc(l),
        Err(RdmaError::Completion(WcStatus::RemoteAccess))
    );
    let l = fabric
        .post_write_with_imm(src.local_key(), 0, 4, small.grant(), u64::MAX, 0)
        .unwrap();
    assert_eq!(
        fabric.await_wc(l),
        Err(RdmaError::Completion(WcStatus::RemoteAccess))
    );
    assert!(matches!(
        fabric.write_local(&src, 6, &[0; 4]),
        Err(RdmaError::OutOfBounds { .. })
    ));
    assert_eq!(
        fabric.register(9, vec![], RW).err(),
        Some(RdmaError::UnknownNode(9))
    );
    for r in [src, ro, small] {
        fabric.deregister(r).unwrap();
    }
}

#[test]
fn dropped_leases_and_regions_are_counted_leaks() {
    let fabric = RdmaFabric::new(2);
    let src = fabric.register(0, vec![1; 8], RO).unwrap();
    let dst = fabric.register(1, vec![0; 8], RW).unwrap();
    let lease = fabric
        .post_write_with_imm(src.local_key(), 0, 8, dst.grant(), 0, 0)
        .unwrap();
    drop(lease);
    assert_eq!(fabric.leaks(), (1, 0));
    // The source stays pinned forever: it can never be deregistered or
    // mutated again.
    fabric.progress(16);
    assert!(matches!(
        fabric.write_local(&src, 0, &[2]),
        Err(RdmaError::RegionBusy { .. })
    ));
    let (_, src) = fabric.deregister(src).err().unwrap();
    drop(src);
    drop(dst);
    assert_eq!(fabric.leaks(), (1, 2));
}

#[test]
fn leases_are_bound_to_their_fabric() {
    let a = RdmaFabric::new(1);
    let b = RdmaFabric::new(1);
    let src = a.register(0, vec![1; 4], RW).unwrap();
    let lease = a
        .post_write_with_imm(src.local_key(), 0, 4, src.grant(), 0, 0)
        .unwrap();
    assert_eq!(b.await_wc(lease), Err(RdmaError::ForeignLease));
    // The foreign attempt consumed nothing on `a`, but the lease is gone:
    // counted as a leak on `a`.
    assert_eq!(a.leaks().0, 1);
    let (_, src) = a.deregister(src).err().unwrap();
    drop(src);
}

#[test]
fn handles_from_another_fabric_are_rejected() {
    let a = RdmaFabric::new(1);
    let b = RdmaFabric::new(1);
    let ra = a.register(0, vec![1; 4], RW).unwrap();
    let rb = b.register(0, vec![2; 4], RW).unwrap();
    // Same node, slot and generation on both fabrics: identity must decide.
    assert_eq!(a.write_local(&rb, 0, &[9]), Err(RdmaError::ForeignHandle));
    assert_eq!(a.read_local(&rb), Err(RdmaError::ForeignHandle));
    assert_eq!(
        a.post_write_with_imm(rb.local_key(), 0, 4, ra.grant(), 0, 0)
            .err(),
        Some(RdmaError::ForeignHandle)
    );
    assert_eq!(
        a.post_write_with_imm(ra.local_key(), 0, 4, rb.grant(), 0, 0)
            .err(),
        Some(RdmaError::ForeignHandle)
    );
    assert!(a.read_remote(rb.grant()).is_none());
    let (err, rb) = a.deregister(rb).err().unwrap();
    assert_eq!(err, RdmaError::ForeignHandle);
    assert_eq!(a.read_local(&ra).unwrap(), vec![1; 4], "A untouched");
    assert_eq!(b.deregister(rb).unwrap(), vec![2; 4], "B still registered");
    a.deregister(ra).unwrap();
    assert_eq!((a.leaks(), b.leaks()), ((0, 0), (0, 0)));
}

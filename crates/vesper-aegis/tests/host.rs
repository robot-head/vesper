//! Iago validators and bounce-grant discipline.

use vesper_aegis::host::*;
use vesper_aegis::*;

#[test]
fn length_and_byte_count_validators() {
    assert_eq!(UntrustedHostVal::from_host(64u32).validate_len(64), Ok(64));
    assert_eq!(
        UntrustedHostVal::from_host(65u32).validate_len(64),
        Err(TeeError::HostLengthOutOfRange {
            reported: 65,
            cap: 64
        })
    );
    assert_eq!(
        UntrustedHostVal::from_host(u32::MAX).validate_len(4096),
        Err(TeeError::HostLengthOutOfRange {
            reported: u64::from(u32::MAX),
            cap: 4096
        })
    );
    assert_eq!(
        UntrustedHostVal::from_host(10u64).validate_byte_count(10),
        Ok(10)
    );
    assert_eq!(
        UntrustedHostVal::from_host(11u64).validate_byte_count(10),
        Err(TeeError::HostByteCountExceedsRequest {
            reported: 11,
            requested: 10
        })
    );
    let even: Result<u32, ()> =
        UntrustedHostVal::from_host(7u32).validate(|v| if v % 2 == 0 { Ok(v) } else { Err(()) });
    assert_eq!(even, Err(()));
}

#[test]
fn clock_validator_rejects_backwards_and_jumps() {
    let mut c = HostClockValidator::new(1_000);
    assert_eq!(c.accept(UntrustedHostVal::from_host(5_000)), Ok(5_000));
    assert_eq!(c.accept(UntrustedHostVal::from_host(5_000)), Ok(5_000));
    assert_eq!(
        c.accept(UntrustedHostVal::from_host(4_999)),
        Err(TeeError::HostTimeNonMonotonic {
            previous: 5_000,
            reported: 4_999
        })
    );
    assert_eq!(
        c.accept(UntrustedHostVal::from_host(6_001)),
        Err(TeeError::HostTimeJump {
            previous: 5_000,
            reported: 6_001,
            max_step: 1_000
        })
    );
    assert_eq!(c.accept(UntrustedHostVal::from_host(6_000)), Ok(6_000));
}

#[test]
fn mmap_validator_rejects_overlap_and_invalid_replies() {
    let mut t = HostMappingTable::new(0x1000_0000, 0x2000_0000);
    let m = |addr, len| UntrustedHostVal::from_host(HostMmapReply { addr, len });
    let a = t.accept_mmap(m(0x1000_0000, 0x4000), 0x4000).unwrap();
    assert_eq!(
        t.accept_mmap(m(0x1000_3000, 0x2000), 0x2000),
        Err(TeeError::HostMmapOverlap {
            addr: 0x1000_3000,
            len: 0x2000
        })
    );
    for (addr, len, req) in [
        (0x1000_8001, 0x1000, 0x1000), // unaligned
        (0x1000_8000, 0, 0),           // empty
        (0x1000_8000, 0x2000, 0x1000), // not what was requested
        (0x0FFF_F000, 0x2000, 0x2000), // below window
        (0x1FFF_F000, 0x2000, 0x2000), // past window end
        (!0xFFF, 0x2000, 0x2000),      // overflows
    ] {
        assert!(
            matches!(
                t.accept_mmap(m(addr, len), req),
                Err(TeeError::HostMmapInvalid { .. })
            ),
            "{addr:#x}+{len:#x}"
        );
    }
    t.unmap(a);
    t.accept_mmap(m(0x1000_3000, 0x2000), 0x2000).unwrap();
}

#[test]
fn bounce_grants_are_linear_and_audited() {
    let mut ap = BounceAperture::new(2, 64);
    let g = ap.grant(0).unwrap();
    assert_eq!(
        ap.grant(0).unwrap_err(),
        TeeError::BadBounceSlot { slot_idx: 0 }
    );
    assert_eq!(
        ap.grant(9).unwrap_err(),
        TeeError::BadBounceSlot { slot_idx: 9 }
    );
    assert!(ap.is_granted(0));
    ap.abort_and_scrub_bounce(g);
    assert!(!ap.is_granted(0));
    {
        let _dropped = ap.grant(1).unwrap();
    }
    assert_eq!(ap.leaked_grants(), 1);

    // A grant from another aperture is refused, not honoured.
    let mut other = BounceAperture::new(1, 64);
    let foreign = other.grant(0).unwrap();
    let mut region = PrivateRegion::with_capacity(64);
    assert_eq!(
        ap.snapshot_from_host_bounce(&foreign, UntrustedHostVal::from_host(4), &mut region)
            .unwrap_err(),
        TeeError::ForeignGrant
    );
    other.abort_and_scrub_bounce(foreign);
    assert_eq!(other.leaked_grants(), 0);
}

#[test]
fn snapshot_copies_validated_length_into_private_region() {
    let mut ap = BounceAperture::new(1, 32);
    let host = ap.host_side();
    host.write(0, 0, b"hello, enclave");
    let g = ap.grant(0).unwrap();
    let mut region = PrivateRegion::with_capacity(16);
    let snap = ap
        .snapshot_from_host_bounce(&g, UntrustedHostVal::from_host(5), &mut region)
        .unwrap();
    assert_eq!(snap.as_slice(), b"hello");
    // Host length beyond the slot.
    let mut region = PrivateRegion::with_capacity(64);
    assert_eq!(
        ap.snapshot_from_host_bounce(&g, UntrustedHostVal::from_host(33), &mut region)
            .unwrap_err(),
        TeeError::HostLengthOutOfRange {
            reported: 33,
            cap: 32
        }
    );
    // Private region too small.
    let mut tiny = PrivateRegion::with_capacity(4);
    assert_eq!(
        ap.snapshot_from_host_bounce(&g, UntrustedHostVal::from_host(5), &mut tiny)
            .unwrap_err(),
        TeeError::PrivateRegionExhausted { need: 5, free: 4 }
    );
    ap.abort_and_scrub_bounce(g);
    let mut out = [1u8; 32];
    host.read(0, 0, &mut out);
    assert_eq!(out, [0u8; 32], "scrub clears the slot");
}

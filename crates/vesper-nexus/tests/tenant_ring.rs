//! One ring per tenant: a tenant's filter cannot read, write or address
//! another tenant's slots, and the host refuses cross-tenant leases. The
//! same hostile filters succeed against the shared broker, which is why the
//! shared ring is for mutually trusting tenants only.

use vesper_nexus::ring::{
    FILTER_EVEN_DOUBLE_WAT, FILTER_THRESHOLD_WAT, FilterError, RECORD_BYTES, RingError,
    StreamFilterBroker,
};
use vesper_nexus::{TenantRingError, TenantRings};

const A: u64 = 1;
const B: u64 = 2;
const SLOTS: u32 = 4;
const SLOT_BYTES: u32 = 1024;
const FUEL: u64 = 1_000_000;
const SECRET: u64 = 0xDEAD_BEEF_CAFE_F00D;

/// Scans every byte of the memory it can address for `SECRET`; returns 16
/// (one record) if found, 0 otherwise. Writes nothing.
const SPY_WAT: &str = r#"
(module
  (import "nexus" "ring" (memory $ring 1))
  (func (export "filter") (param $grant i32) (result i32)
    (local $p i32) (local $end i32)
    (local.set $end (i32.sub (i32.mul (memory.size) (i32.const 65536)) (i32.const 8)))
    (block $done
      (loop $next
        (br_if $done (i32.gt_u (local.get $p) (local.get $end)))
        (if (i64.eq (i64.load (local.get $p)) (i64.const 0xDEADBEEFCAFEF00D))
          (then (return (i32.const 16))))
        (local.set $p (i32.add (local.get $p) (i32.const 8)))
        (br $next)))
    (i32.const 0))
)
"#;

/// Overwrites every word of the memory it can address with a marker.
const SCRIBBLE_WAT: &str = r#"
(module
  (import "nexus" "ring" (memory $ring 1))
  (func (export "filter") (param $grant i32) (result i32)
    (local $p i32) (local $end i32)
    (local.set $end (i32.mul (memory.size) (i32.const 65536)))
    (block $done
      (loop $next
        (br_if $done (i32.ge_u (local.get $p) (local.get $end)))
        (i64.store (local.get $p) (i64.const 0x5555555555555555))
        (local.set $p (i32.add (local.get $p) (i32.const 8)))
        (br $next)))
    (i32.const 0))
)
"#;

/// Stores one byte past the end of the memory it was given.
const OUT_OF_BOUNDS_WAT: &str = r#"
(module
  (import "nexus" "ring" (memory $ring 1))
  (func (export "filter") (param $grant i32) (result i32)
    (i32.store8 (i32.mul (memory.size) (i32.const 65536)) (i32.const 1))
    (i32.const 0))
)
"#;

fn records(pairs: &[(u64, u64)]) -> Vec<u8> {
    pairs
        .iter()
        .flat_map(|(k, v)| k.to_le_bytes().into_iter().chain(v.to_le_bytes()))
        .collect()
}

fn decode(bytes: &[u8]) -> Vec<(u64, u64)> {
    let (chunks, rest) = bytes.as_chunks::<{ RECORD_BYTES as usize }>();
    assert!(rest.is_empty());
    chunks
        .iter()
        .map(|r| {
            (
                u64::from_le_bytes(r[..8].try_into().unwrap()),
                u64::from_le_bytes(r[8..].try_into().unwrap()),
            )
        })
        .collect()
}

fn rings(a_filter: &str, b_filter: &str) -> TenantRings {
    let mut r = TenantRings::new().unwrap();
    r.add_tenant(A, SLOTS, SLOT_BYTES, a_filter.as_bytes(), FUEL)
        .unwrap();
    r.add_tenant(B, SLOTS, SLOT_BYTES, b_filter.as_bytes(), FUEL)
        .unwrap();
    r
}

#[test]
fn each_tenant_filters_its_own_ring_in_place() {
    let mut r = rings(FILTER_THRESHOLD_WAT, FILTER_EVEN_DOUBLE_WAT);
    let input = records(&[(1, 50), (2, 150), (3, 300), (4, 7)]);
    let len = input.len() as u32;
    for (tenant, expected) in [(A, vec![(2, 150), (3, 300)]), (B, vec![(2, 300), (4, 14)])] {
        let lease = r.lease(tenant).unwrap();
        let (base, _) = r.ring_span(tenant).unwrap();
        r.payload_mut(tenant, &lease).unwrap()[..input.len()].copy_from_slice(&input);
        let written = r.apply(tenant, &lease, len, 0x0201, 99).unwrap();
        let out = r.payload(tenant, &lease, written).unwrap();
        assert_eq!(decode(out), expected);
        // In place: the payload lives at the grant's offset in this
        // tenant's ring, which did not move.
        let grant = r.read_grant(tenant, &lease).unwrap();
        assert_eq!(grant.byte_len, len);
        assert_eq!(out.as_ptr(), base.wrapping_add(grant.ring_offset as usize));
        r.release(tenant, lease).unwrap();
    }
    assert_eq!(r.metrics(A).unwrap().udf_invocations, 1);
    assert_eq!(r.metrics(B).unwrap().udf_invocations, 1);
    assert_eq!(r.leaked_leases(), 0);
}

#[test]
fn tenant_rings_are_disjoint_memories() {
    let r = rings(FILTER_THRESHOLD_WAT, FILTER_THRESHOLD_WAT);
    let (a, a_len) = r.ring_span(A).unwrap();
    let (b, b_len) = r.ring_span(B).unwrap();
    let (a, b) = (a as usize, b as usize);
    assert!(
        a + a_len <= b || b + b_len <= a,
        "{a:#x}+{a_len} vs {b:#x}+{b_len}"
    );
}

/// The hazard: on the shared broker, tenant A's filter sees B's secret and
/// can overwrite B's slot.
#[test]
fn shared_broker_lets_a_hostile_filter_reach_other_slots() {
    let mut shared = StreamFilterBroker::new(SLOTS, SLOT_BYTES).unwrap();
    shared.add_tenant(A, SPY_WAT.as_bytes(), FUEL).unwrap();
    shared
        .add_tenant(B, FILTER_THRESHOLD_WAT.as_bytes(), FUEL)
        .unwrap();
    let secret = records(&[(SECRET, SECRET)]);
    let b_lease = shared.lease(B).unwrap();
    shared.payload_mut(&b_lease).unwrap()[..16].copy_from_slice(&secret);
    let a_lease = shared.lease(A).unwrap();
    assert_eq!(
        shared.apply(&a_lease, 16, 1, 0),
        Ok(16),
        "the spy found B's data"
    );
    shared.release(a_lease).unwrap();
    shared.release(b_lease).unwrap();

    let mut shared = StreamFilterBroker::new(SLOTS, SLOT_BYTES).unwrap();
    shared.add_tenant(A, SCRIBBLE_WAT.as_bytes(), FUEL).unwrap();
    shared
        .add_tenant(B, FILTER_THRESHOLD_WAT.as_bytes(), FUEL)
        .unwrap();
    let b_lease = shared.lease(B).unwrap();
    shared.payload_mut(&b_lease).unwrap()[..16].copy_from_slice(&secret);
    let a_lease = shared.lease(A).unwrap();
    shared.apply(&a_lease, 16, 1, 0).unwrap();
    assert_ne!(
        shared.payload(&b_lease, 16).unwrap(),
        &secret[..],
        "the scribbler overwrote B's slot"
    );
    shared.release(a_lease).unwrap();
    shared.release(b_lease).unwrap();
}

#[test]
fn a_tenant_filter_cannot_read_another_tenants_ring() {
    let mut r = rings(SPY_WAT, FILTER_THRESHOLD_WAT);
    let secret = records(&[(SECRET, SECRET)]);
    let b_lease = r.lease(B).unwrap();
    r.payload_mut(B, &b_lease).unwrap()[..16].copy_from_slice(&secret);
    // A's spy scans every byte of every slot it can address.
    let mut a_leases = Vec::new();
    for _ in 0..SLOTS {
        a_leases.push(r.lease(A).unwrap());
    }
    for l in &a_leases {
        assert_eq!(r.apply(A, l, 16, 1, 0), Ok(0), "B's secret is unreachable");
    }
    // Sanity: the spy does find the secret when it is in A's own ring.
    r.payload_mut(A, &a_leases[0]).unwrap()[..16].copy_from_slice(&secret);
    assert_eq!(r.apply(A, &a_leases[0], 16, 1, 0), Ok(16));
    for l in a_leases {
        r.release(A, l).unwrap();
    }
    r.release(B, b_lease).unwrap();
}

#[test]
fn a_tenant_filter_cannot_write_another_tenants_ring() {
    let mut r = rings(SCRIBBLE_WAT, FILTER_THRESHOLD_WAT);
    let input = records(&[(1, 500), (2, 5), (3, 700)]);
    let mut b_leases = Vec::new();
    for _ in 0..SLOTS {
        let l = r.lease(B).unwrap();
        r.payload_mut(B, &l).unwrap()[..input.len()].copy_from_slice(&input);
        b_leases.push(l);
    }
    let a_lease = r.lease(A).unwrap();
    r.apply(A, &a_lease, 16, 1, 0).unwrap();
    // A destroyed its own ring...
    assert!(
        r.payload(A, &a_lease, 16)
            .unwrap()
            .iter()
            .all(|&b| b == 0x55)
    );
    // ...and every one of B's slots is untouched and still filters.
    for l in &b_leases {
        assert_eq!(r.payload(B, l, input.len() as u32).unwrap(), &input[..]);
    }
    let written = r.apply(B, &b_leases[0], input.len() as u32, 1, 0).unwrap();
    assert_eq!(
        decode(r.payload(B, &b_leases[0], written).unwrap()),
        vec![(1, 500), (3, 700)]
    );
    for l in b_leases {
        r.release(B, l).unwrap();
    }
    r.release(A, a_lease).unwrap();
}

#[test]
fn addressing_past_the_tenants_ring_traps_without_touching_others() {
    let mut r = rings(OUT_OF_BOUNDS_WAT, FILTER_THRESHOLD_WAT);
    let input = records(&[(9, 900)]);
    let b_lease = r.lease(B).unwrap();
    r.payload_mut(B, &b_lease).unwrap()[..16].copy_from_slice(&input);
    let a_lease = r.lease(A).unwrap();
    for _ in 0..3 {
        assert!(matches!(
            r.apply(A, &a_lease, 16, 1, 0),
            Err(TenantRingError::Filter(FilterError::GuestTrapped(_)))
        ));
    }
    let m = r.metrics(A).unwrap();
    assert_eq!((m.udf_invocations, m.udf_traps), (0, 3));
    assert_eq!(r.payload(B, &b_lease, 16).unwrap(), &input[..]);
    assert_eq!(r.apply(B, &b_lease, 16, 1, 0), Ok(16));
    assert_eq!(r.metrics(B).unwrap().udf_traps, 0);
    r.release(A, a_lease).unwrap();
    r.release(B, b_lease).unwrap();
}

#[test]
fn the_host_refuses_cross_tenant_leases() {
    let mut r = rings(FILTER_THRESHOLD_WAT, FILTER_THRESHOLD_WAT);
    let a_lease = r.lease(A).unwrap();
    let cross = TenantRingError::CrossTenantLease {
        acting: B,
        owner: A,
    };
    assert_eq!(r.payload_mut(B, &a_lease).err(), Some(cross.clone()));
    assert_eq!(r.payload(B, &a_lease, 0).err(), Some(cross.clone()));
    assert_eq!(r.read_grant(B, &a_lease).err(), Some(cross.clone()));
    assert_eq!(r.grant_offset(B, &a_lease).err(), Some(cross.clone()));
    assert_eq!(r.apply(B, &a_lease, 16, 1, 0).err(), Some(cross.clone()));
    // A rejected release hands the lease back; the owner can still release.
    let (a_lease, e) = r.release(B, a_lease).unwrap_err();
    assert_eq!(e, cross);
    assert_eq!(r.metrics(B).unwrap(), Default::default());
    r.release(A, a_lease).unwrap();
    assert_eq!(r.leaked_leases(), 0);
}

#[test]
fn a_lease_from_another_broker_is_foreign() {
    let mut r1 = rings(FILTER_THRESHOLD_WAT, FILTER_THRESHOLD_WAT);
    let mut r2 = rings(FILTER_THRESHOLD_WAT, FILTER_THRESHOLD_WAT);
    let l1 = r1.lease(A).unwrap();
    let foreign = TenantRingError::Filter(FilterError::Ring(RingError::ForeignLease));
    assert_eq!(r2.apply(A, &l1, 16, 1, 0).err(), Some(foreign.clone()));
    assert_eq!(r2.payload_mut(A, &l1).err(), Some(foreign.clone()));
    let (l1, e) = r2.release(A, l1).unwrap_err();
    assert_eq!(e, foreign);
    r1.release(A, l1).unwrap();
}

#[test]
fn slots_are_per_tenant_and_tenants_are_checked() {
    let mut r = rings(FILTER_THRESHOLD_WAT, FILTER_THRESHOLD_WAT);
    let held: Vec<_> = (0..SLOTS).map(|_| r.lease(A).unwrap()).collect();
    assert_eq!(
        r.lease(A).err(),
        Some(TenantRingError::Filter(FilterError::Ring(
            RingError::Exhausted
        )))
    );
    // A exhausting its ring does not consume B's slots.
    assert_eq!(r.free_slots(B), Some(SLOTS as usize));
    let b = r.lease(B).unwrap();
    r.release(B, b).unwrap();
    for l in held {
        r.release(A, l).unwrap();
    }
    assert_eq!(r.lease(9).err(), Some(TenantRingError::UnknownTenant(9)));
    assert_eq!(
        r.add_tenant(A, 1, 64, FILTER_THRESHOLD_WAT.as_bytes(), FUEL),
        Err(TenantRingError::DuplicateTenant(A))
    );
    assert_eq!(r.tenants().collect::<Vec<_>>(), vec![A, B]);
    // Bad geometry and a bad module are typed errors and add no tenant.
    assert!(
        r.add_tenant(3, 0, 64, FILTER_THRESHOLD_WAT.as_bytes(), FUEL)
            .is_err()
    );
    assert!(matches!(
        r.add_tenant(4, 1, 64, b"(module", FUEL),
        Err(TenantRingError::Filter(FilterError::InitFailed(_)))
    ));
    assert_eq!(r.tenants().count(), 2);
}

#[test]
fn dropped_leases_are_counted() {
    let mut r = rings(FILTER_THRESHOLD_WAT, FILTER_THRESHOLD_WAT);
    drop(r.lease(B).unwrap());
    assert_eq!(r.leaked_leases(), 1);
    assert_eq!(r.free_slots(B), Some(SLOTS as usize - 1));
}

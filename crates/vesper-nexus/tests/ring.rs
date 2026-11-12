//! SharedPluginRing: two tenants' filter modules over one ring, records
//! transformed in place, and a host-side zero-allocation steady state
//! measured with a counting global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use vesper_nexus::ring::{
    FILTER_EVEN_DOUBLE_WAT, FILTER_THRESHOLD_WAT, FilterError, GRANT_BYTES, RECORD_BYTES,
    StreamFilterBroker, grant_layout,
};

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
thread_local! {
    // Count only on the measuring thread; other tests run concurrently.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}
fn counting() -> bool {
    COUNTING.try_with(Cell::get).unwrap_or(false)
}
// SAFETY: forwards to the system allocator; only counts calls.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if counting() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        if counting() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

const A: u64 = 1;
const B: u64 = 2;

fn broker() -> StreamFilterBroker {
    let mut b = StreamFilterBroker::new(8, 4096).unwrap();
    b.add_tenant(A, FILTER_THRESHOLD_WAT.as_bytes(), 1_000_000)
        .unwrap();
    b.add_tenant(B, FILTER_EVEN_DOUBLE_WAT.as_bytes(), 1_000_000)
        .unwrap();
    b
}

fn fill(buf: &mut [u8], records: &[(u64, u64)]) -> u32 {
    for (i, (k, v)) in records.iter().enumerate() {
        let at = i * RECORD_BYTES as usize;
        buf[at..at + 8].copy_from_slice(&k.to_le_bytes());
        buf[at + 8..at + 16].copy_from_slice(&v.to_le_bytes());
    }
    (records.len() as u32) * RECORD_BYTES
}

fn decode(bytes: &[u8]) -> Vec<(u64, u64)> {
    bytes
        .chunks_exact(16)
        .map(|c| {
            (
                u64::from_le_bytes(c[..8].try_into().unwrap()),
                u64::from_le_bytes(c[8..].try_into().unwrap()),
            )
        })
        .collect()
}

#[test]
fn grant_wire_layout_matches_section_30_2() {
    let l = grant_layout();
    assert_eq!((l.size(), l.alignment()), (GRANT_BYTES as usize, 1));
    let fields: Vec<_> = l
        .fields()
        .iter()
        .map(|f| (f.name().to_string(), f.offset(), f.size()))
        .collect();
    assert_eq!(
        fields,
        [
            ("ring_offset".into(), 0, 4),
            ("byte_len".into(), 4, 4),
            ("schema_id".into(), 8, 4),
            ("tenant_flags".into(), 12, 4),
            ("record_timestamp_ns".into(), 16, 8),
        ]
    );
}

#[test]
fn two_tenants_transform_records_in_place() {
    let mut b = broker();
    let base = b.ring_base();
    let records: Vec<(u64, u64)> = (0..100).map(|i| (i, (i * 37) % 250)).collect();
    let la = b.lease(A).unwrap();
    let lb = b.lease(B).unwrap();
    assert_ne!(la.slot(), lb.slot());
    let na = fill(b.payload_mut(&la).unwrap(), &records);
    let nb = fill(b.payload_mut(&lb).unwrap(), &records);
    let pa = b.payload(&la, na).unwrap().as_ptr();
    let wa = b.apply(&la, na, 0x0201, 11).unwrap();
    let wb = b.apply(&lb, nb, 0x0201, 12).unwrap();
    // The guest rewrote the very bytes the producer wrote: same address,
    // same memory, no copy into a guest-owned buffer.
    assert_eq!(b.payload(&la, wa).unwrap().as_ptr(), pa);
    assert_eq!(b.ring_base(), base);
    let want_a: Vec<_> = records.iter().copied().filter(|(_, v)| *v >= 100).collect();
    let want_b: Vec<_> = records
        .iter()
        .filter(|(k, _)| k % 2 == 0)
        .map(|(k, v)| (*k, v * 2))
        .collect();
    assert_eq!(decode(b.payload(&la, wa).unwrap()), want_a);
    assert_eq!(decode(b.payload(&lb, wb).unwrap()), want_b);
    let g = b.read_grant(&la).unwrap();
    assert_eq!(
        (
            g.byte_len,
            g.schema_id,
            g.tenant_flags,
            g.record_timestamp_ns
        ),
        (na, 0x0201, 1, 11)
    );
    assert_eq!(g.ring_offset, la.slot() * 4096 + GRANT_BYTES);
    let ma = b.metrics(A).unwrap();
    assert_eq!(
        (ma.udf_invocations, ma.udf_traps, ma.bytes_in),
        (1, 0, u64::from(na))
    );
    assert!(ma.fuel_consumed > 0);
    b.release(la).unwrap();
    b.release(lb).unwrap();
    assert_eq!(b.ring().leaked_leases(), 0);
}

#[test]
fn over_budget_and_contract_violations_are_typed_errors() {
    let mut b = StreamFilterBroker::new(2, 1 << 16).unwrap();
    b.add_tenant(A, FILTER_THRESHOLD_WAT.as_bytes(), 500)
        .unwrap();
    // Returns a length past the grant.
    b.add_tenant(
        B,
        br#"(module (import "nexus" "ring" (memory 1))
              (func (export "filter") (param i32) (result i32) (i32.const 999999)))"#,
        1_000,
    )
    .unwrap();
    let records: Vec<(u64, u64)> = (0..2000).map(|i| (i, i)).collect();
    let la = b.lease(A).unwrap();
    let n = fill(b.payload_mut(&la).unwrap(), &records);
    assert_eq!(b.apply(&la, n, 1, 0), Err(FilterError::OutOfFuel));
    let lb = b.lease(B).unwrap();
    assert_eq!(
        b.apply(&lb, 16, 1, 0),
        Err(FilterError::GuestContract {
            returned: 999_999,
            byte_len: 16
        })
    );
    assert!(matches!(
        b.lease(A),
        Err(FilterError::Ring(vesper_nexus::ring::RingError::Exhausted))
    ));
    let cap = b.ring().payload_capacity();
    assert!(matches!(
        b.apply(&la, cap + 1, 1, 0),
        Err(FilterError::Ring(
            vesper_nexus::ring::RingError::PayloadTooLarge { .. }
        ))
    ));
    assert_eq!(b.metrics(A).unwrap().udf_traps, 1);
    assert_eq!(b.metrics(B).unwrap().udf_traps, 1);
    b.release(la).unwrap();
    b.release(lb).unwrap();
}

#[test]
fn foreign_and_leaked_leases() {
    let mut b1 = broker();
    let mut b2 = broker();
    let l = b1.lease(A).unwrap();
    assert!(b2.payload_mut(&l).is_err());
    let (l, e) = b2.release(l).unwrap_err();
    assert!(matches!(
        e,
        FilterError::Ring(vesper_nexus::ring::RingError::ForeignLease)
    ));
    b1.release(l).unwrap();
    let free = b1.ring().free_slots();
    drop(b1.lease(B).unwrap());
    assert_eq!(b1.ring().leaked_leases(), 1);
    assert_eq!(
        b1.ring().free_slots(),
        free - 1,
        "a leaked slot is not recycled"
    );
}

#[test]
fn host_steady_state_allocates_nothing() {
    let mut b = broker();
    let records: Vec<(u64, u64)> = (0..64).map(|i| (i, i * 3)).collect();
    let step = |b: &mut StreamFilterBroker, i: u64| {
        let tenant = if i.is_multiple_of(2) { A } else { B };
        let lease = b.lease(tenant).unwrap();
        let n = fill(b.payload_mut(&lease).unwrap(), &records);
        let w = b.apply(&lease, n, 0x0201, i).unwrap();
        let out = b.payload(&lease, w).unwrap();
        let first = u64::from_le_bytes(out[..8].try_into().unwrap());
        b.release(lease).unwrap();
        first
    };
    // Warm up (lazy Wasmtime state, first-call paths).
    for i in 0..64 {
        step(&mut b, i);
    }
    // The counter sees allocations on this thread.
    COUNTING.with(|c| c.set(true));
    drop(std::hint::black_box(Box::new(7u64)));
    COUNTING.with(|c| c.set(false));
    assert_eq!(ALLOCS.swap(0, Ordering::Relaxed), 1);
    COUNTING.with(|c| c.set(true));
    let mut sink = 0u64;
    for i in 0..10_000 {
        sink = sink.wrapping_add(step(&mut b, i));
    }
    COUNTING.with(|c| c.set(false));
    let allocs = ALLOCS.load(Ordering::Relaxed);
    assert!(sink > 0);
    assert_eq!(
        allocs, 0,
        "host-side steady state allocated {allocs} times in 10000 calls"
    );
    let (ma, mb) = (b.metrics(A).unwrap(), b.metrics(B).unwrap());
    assert_eq!(ma.udf_invocations + mb.udf_invocations, 10_064);
}

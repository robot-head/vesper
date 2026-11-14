//! `ConnTable::rekey` (wave seven lane 7): a retired local CID stops
//! routing, the new one routes to the same slot, both lookup paths agree,
//! and the table is unchanged after every rejected call.

use vesper_warp_shard::{ConnState, ConnTable, ConnectionId, IsaPath, RekeyError, TableConfig};

fn cid(v: u64) -> ConnectionId {
    ConnectionId::from_u64(v)
}

fn table(isa: IsaPath) -> ConnTable {
    ConnTable::new(TableConfig::new(64, 8).with_isa(isa)).expect("table")
}

fn paths() -> Vec<IsaPath> {
    [IsaPath::Scalar, IsaPath::detect()]
        .into_iter()
        .filter(|p| p.is_supported())
        .collect()
}

#[test]
fn rekey_moves_routing_to_the_new_cid() {
    for isa in paths() {
        let mut t = table(isa);
        let state = ConnState::new(cid(0xAA), 12_000, 77);
        let h = t.insert(&cid(1), &state).expect("insert");
        let other = t.insert(&cid(2), &state).expect("insert");
        assert_eq!(t.rekey(h, &cid(100)), Ok(cid(1)));
        for bytes in [cid(1), cid(100), cid(2)] {
            let a = t.lookup(bytes.as_bytes()).expect("lookup");
            let b = t.lookup_cid_simd(bytes.as_bytes()).expect("simd");
            let c = t.lookup_cid_scalar(bytes.as_bytes()).expect("scalar");
            assert_eq!(a, b);
            assert_eq!(a, c);
        }
        assert_eq!(t.lookup(cid(1).as_bytes()), Ok(None), "retired CID routes");
        assert_eq!(t.lookup(cid(100).as_bytes()), Ok(Some(h)));
        assert_eq!(t.lookup(cid(2).as_bytes()), Ok(Some(other)));
        assert_eq!(t.local_cid(h), Ok(cid(100)));
        // Handle and state survive.
        assert_eq!(t.get(h).expect("live").idle_deadline_ns, 77);
        assert_eq!(t.len(), 2);
        // Rekeying again and removing still keep the index consistent.
        assert_eq!(t.rekey(h, &cid(101)), Ok(cid(100)));
        assert_eq!(t.rekey(h, &cid(101)), Ok(cid(101)), "same CID is a no-op");
        assert_eq!(t.remove(h).map(|(c, _)| c), Ok(cid(101)));
        assert_eq!(t.lookup(cid(101).as_bytes()), Ok(None));
        let again = t.insert(&cid(1), &state).expect("old CID reusable");
        assert_eq!(t.lookup(cid(1).as_bytes()), Ok(Some(again)));
    }
}

#[test]
fn rejected_rekeys_leave_the_table_unchanged() {
    let mut t = table(IsaPath::Scalar);
    let state = ConnState::new(cid(0xAA), 12_000, 0);
    let a = t.insert(&cid(1), &state).expect("insert");
    let _b = t.insert(&cid(2), &state).expect("insert");
    assert_eq!(t.rekey(a, &cid(2)), Err(RekeyError::DuplicateCid));
    let short = ConnectionId::from_slice(&[1, 2, 3]).expect("cid");
    assert_eq!(
        t.rekey(a, &short),
        Err(RekeyError::CidLength {
            expected: 8,
            got: 3
        })
    );
    assert_eq!(t.lookup(cid(1).as_bytes()), Ok(Some(a)));
    let (_, _) = t.remove(a).expect("remove");
    assert_eq!(t.rekey(a, &cid(9)), Err(RekeyError::Stale));
    assert_eq!(t.lookup(cid(9).as_bytes()), Ok(None));
}

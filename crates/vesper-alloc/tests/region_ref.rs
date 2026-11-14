//! RegionRef: copyable checked references for shared and cyclic region graphs.
use vesper_alloc::{HandleError, RefPairError, Region, RegionConfig, RegionRef};

struct Node {
    value: u32,
    next: Option<RegionRef<Node>>,
}

#[test]
fn a_cycle_is_built_and_walked_through_the_region() {
    let mut r = Region::new(RegionConfig::bounded(64 * 1024)).unwrap();
    let boxes: Vec<_> = (0..5)
        .map(|i| {
            r.alloc_value(Node {
                value: i,
                next: None,
            })
            .ok()
            .unwrap()
        })
        .collect();
    let refs: Vec<_> = boxes.iter().map(|b| r.box_ref(b).unwrap()).collect();
    for i in 0..5 {
        r.ref_get_mut(refs[i]).unwrap().next = Some(refs[(i + 1) % 5]);
    }
    let mut at = refs[2];
    let mut seen = Vec::new();
    for _ in 0..10 {
        let n = r.ref_get(at).unwrap();
        seen.push(n.value);
        at = n.next.unwrap();
    }
    assert_eq!(seen, [2, 3, 4, 0, 1, 2, 3, 4, 0, 1]);
    // Mutate both ends of an edge at once.
    let (a, b) = r.ref_get_disjoint_mut(refs[0], refs[1]).unwrap();
    std::mem::swap(&mut a.value, &mut b.value);
    assert_eq!(r.ref_get(refs[0]).unwrap().value, 1);
    assert_eq!(
        r.ref_get_disjoint_mut(refs[3], refs[3]).err(),
        Some(RefPairError::Aliased)
    );
    // The box owner still decides the payload's fate.
    let first = boxes.into_iter().next().unwrap();
    let taken = r.take(first).unwrap();
    assert_eq!(taken.value, 1);
    assert_eq!(r.ref_get(refs[0]).err(), Some(HandleError::Moved));
    assert_eq!(
        r.ref_get_disjoint_mut(refs[0], refs[1]).err(),
        Some(RefPairError::Handle(HandleError::Moved))
    );
}

#[test]
fn rewind_and_reset_make_every_copy_stale() {
    let mut r = Region::new(RegionConfig::bounded(64 * 1024)).unwrap();
    let keep = r.alloc_value(7u64).unwrap();
    let keep_ref = r.box_ref(&keep).unwrap();
    let mark = r.checkpoint().unwrap();
    let temp = r.alloc_value(8u64).unwrap();
    let temp_ref = r.box_ref(&temp).unwrap();
    let copy = temp_ref;
    r.rewind(mark).unwrap();
    assert_eq!(r.ref_get(copy).err(), Some(HandleError::Stale));
    assert_eq!(*r.ref_get(keep_ref).unwrap(), 7);
    // A new allocation in the same slot does not revive the old reference.
    let again = r.alloc_value(9u64).unwrap();
    assert_eq!(r.ref_get(temp_ref).err(), Some(HandleError::Stale));
    assert_eq!(*r.get(&again).unwrap(), 9);
    assert_eq!(r.box_ref(&temp).err(), Some(HandleError::Stale));
    r.reset().unwrap();
    assert_eq!(r.ref_get(keep_ref).err(), Some(HandleError::Stale));
}

#[test]
fn references_are_branded_by_their_region() {
    let mut a = Region::new(RegionConfig::bounded(4096)).unwrap();
    let b = Region::new(RegionConfig::bounded(4096)).unwrap();
    let x = a.alloc_value(1u8).unwrap();
    let xr = a.box_ref(&x).unwrap();
    assert_eq!(xr.region(), a.id());
    assert!(matches!(
        b.ref_get(xr),
        Err(HandleError::CrossRegion { .. })
    ));
    assert!(matches!(
        b.box_ref(&x),
        Err(HandleError::CrossRegion { .. })
    ));
}

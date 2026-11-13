//! Bounded cyclic graph traversal using weak, runtime-checked arena keys.
use vesper_alloc::{ArenaError, ArenaKey, Region, RegionConfig};

struct Node {
    name: String,
    weight: u64,
    next: Option<ArenaKey<Node>>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut region = Region::new(RegionConfig::bounded(64 * 1024))?;
    let mut graph = region.arena_with_capacity(3)?;
    let a = region.arena_insert(
        &mut graph,
        Node {
            name: "a".into(),
            weight: 1,
            next: None,
        },
    )?;
    let b = region.arena_insert(
        &mut graph,
        Node {
            name: "b".into(),
            weight: 2,
            next: None,
        },
    )?;
    let c = region.arena_insert(
        &mut graph,
        Node {
            name: "c".into(),
            weight: 3,
            next: None,
        },
    )?;
    region.arena_get_mut(&graph, a)?.next = Some(b);
    region.arena_get_mut(&graph, b)?.next = Some(c);
    region.arena_get_mut(&graph, c)?.next = Some(a);

    // The explicit step bound terminates traversal of this actual cycle.
    let mut current = a;
    let mut initial = 0;
    for _ in 0..6 {
        let node = region.arena_get(&graph, current)?;
        initial += node.weight;
        current = node.next.expect("three nodes form a cycle");
    }
    assert_eq!(initial, 12);
    // Request reversed slot order; the two mutable references remain disjoint.
    let (last, first) = region.arena_get_disjoint_mut(&graph, c, a)?;
    last.weight += 30;
    first.weight += 10;
    let mut updated = 0;
    for key in [a, b, c] {
        updated += region.arena_get(&graph, key)?.weight;
    }
    assert_eq!(updated, 46);

    let removed = region.arena_remove(&mut graph, b)?;
    assert_eq!(removed.name, "b");
    let stale_edge = region.arena_get(&graph, a)?.next.unwrap();
    assert!(matches!(
        region.arena_get(&graph, stale_edge),
        Err(ArenaError::StaleKey)
    ));
    region.arena_get_mut(&graph, a)?.next = Some(c);
    current = a;
    let mut repaired = 0;
    for _ in 0..2 {
        let node = region.arena_get(&graph, current)?;
        repaired += node.weight;
        current = node.next.expect("repair leaves the two-node cycle");
    }
    assert_eq!(current, a);
    assert_eq!(repaired, 44);
    assert_eq!(region.arena_len(&graph)?, 2);
    region.arena_release(graph)?;
    // Removal transferred this String-owning value to the caller, independent
    // of arena cleanup. Its allocation was supplied by this example's caller.
    assert_eq!(removed.name, "b");
    drop(removed);
    println!(
        "nodes=2 initial_cycle_sum={initial} updated_cycle_sum={updated} repaired_cycle_sum={repaired} stale_edge=rejected"
    );
    Ok(())
}

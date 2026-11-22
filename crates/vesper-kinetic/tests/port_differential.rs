//! Differential checks of the Vesper port against the frozen Rust seed copy
//! (`vesper-kinetic-seed`, here `vesper_kinetic_rust`) that the flip gate
//! runs (crates/vesper-kinetic/flip-gate.toml, SH6 lane 9, #1404). Only the
//! port-backed package runs this file; the seed has no Rust build to compare.

use vesper_kinetic::{Colorer, ConstraintBodies};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

/// The port's colourings (`Colorer`, `MultiColorer`) against the frozen Rust
/// build, visiting order, colours and layers, from small graphs (the
/// comparison sort) to large ones (the port's radix sort of packed keys) and
/// body counts too wide to pack (SH6 lane 9, #1404).
#[test]
fn colourings_match_the_rust_build() {
    use vesper_kinetic_rust as rust;
    let mut rng = Lcg(7);
    let mut differences = 0usize;
    let mut cases = 0usize;
    for round in 0..300u32 {
        let bodies = match round % 4 {
            0 => 2 + rng.next() % 30,
            1 => 64 + rng.next() % 4096,
            2 => 1 << (12 + rng.next() % 9),
            _ => 3_000_000 + rng.next() % 1_000_000,
        };
        let count = match round % 3 {
            0 => rng.next() % 300,
            1 => 256 + rng.next() % 2048,
            _ => rng.next() % 5000,
        } as usize;
        let mut cs = Vec::with_capacity(count);
        for _ in 0..count {
            let a = rng.next() % bodies;
            let b = match rng.next() % 4 {
                0 => None,
                1 => Some((a + 1) % bodies),
                _ => Some(rng.next() % bodies),
            };
            let b = b.filter(|&b| b != a);
            cs.push(ConstraintBodies { a, b });
        }
        let rust_cs: Vec<_> = cs
            .iter()
            .map(|c| rust::ConstraintBodies { a: c.a, b: c.b })
            .collect();
        let mut p = Colorer::new(bodies as usize, count);
        let mut r = rust::Colorer::new(bodies as usize, count);
        let (pr, rr) = (p.color(&cs), r.color(&rust_cs));
        cases += 1;
        assert_eq!(pr.is_ok(), rr.is_ok(), "round {round}: {pr:?} / {rr:?}");
        if pr.is_ok()
            && (p.visiting_order() != r.visiting_order()
                || p.layer_count() != r.layer_count()
                || (0..count).any(|i| p.color_of(i) != r.color_of(i))
                || (0..p.layer_count()).any(|l| p.layer(l) != r.layer(l)))
        {
            differences += 1;
        }

        // MultiColorer over sets of one to four distinct bodies.
        let mut flat = Vec::new();
        let mut offsets = vec![0u32];
        for _ in 0..count {
            let arity = 1 + rng.next() % 4;
            let start = flat.len();
            while flat.len() - start < arity as usize {
                let body = rng.next() % bodies;
                if !flat[start..].contains(&body) {
                    flat.push(body);
                }
                if bodies < 4 && flat.len() - start == bodies as usize {
                    break;
                }
            }
            offsets.push(flat.len() as u32);
        }
        let mut pm = vesper_kinetic::MultiColorer::new(bodies as usize, count);
        let mut rm = rust::MultiColorer::new(bodies as usize, count);
        let (pr, rr) = (pm.color(&flat, &offsets), rm.color(&flat, &offsets));
        cases += 1;
        assert_eq!(pr.is_ok(), rr.is_ok(), "round {round}: {pr:?} / {rr:?}");
        if pr.is_ok()
            && (pm.visiting_order() != rm.visiting_order()
                || pm.layer_count() != rm.layer_count()
                || (0..count).any(|i| pm.color_of(i) != rm.color_of(i))
                || (0..pm.layer_count()).any(|l| pm.layer(l) != rm.layer(l)))
        {
            differences += 1;
        }
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

//! A differential check of the Z-set kernels and the frontier (port wave P3b
//! lane 7, #1069): thousands of seeded consolidations, sums, circuit ticks
//! and frontier operations under tight limits, including weights at the i64
//! edges, zero weights, repeated rows and exhausted budgets. Every result or
//! error is printed with `Debug` and folded into one FNV-1a digest, so the
//! Rust build and the Vesper build must agree byte for byte on every value
//! and on which error comes first. The goldens were recorded from the Rust
//! build.
use vesper_flux::{
    CircuitBuilder, Error, Field, Frontier, FrontierTicket, Limits, NodeId, Predicate, TickInput,
    ZSet,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // SplitMix64.
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn weight(&mut self) -> i64 {
        match self.below(10) {
            0 => i64::MAX,
            1 => i64::MIN,
            2 => 0,
            3 => i64::MAX - 1,
            4 => -(1 << 32),
            5 => 1 << 32,
            _ => self.below(7) as i64 - 3,
        }
    }
}

struct Digest(u64);
impl Digest {
    /// With `VESPER_FLUX_KERNELS_DUMP=<dir>`, every value is also appended
    /// to `<dir>/<test name>.txt`, so two builds' dumps can be diffed.
    fn add(&mut self, value: impl std::fmt::Debug) {
        let text = format!("{value:?};");
        if let Ok(dir) = std::env::var("VESPER_FLUX_KERNELS_DUMP") {
            use std::io::Write;
            let name = std::thread::current()
                .name()
                .unwrap_or("main")
                .replace("::", "_");
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(format!("{dir}/{name}.txt"))
                .unwrap();
            writeln!(f, "{text}").unwrap();
        }
        for b in text.bytes() {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

fn limits(rng: &mut Rng) -> Limits {
    Limits {
        max_columns: 1 + rng.below(4) as usize,
        max_input_entries: 1 + rng.below(12) as usize,
        max_rows: 1 + rng.below(8) as usize,
        max_nodes: 4 + rng.below(12) as usize,
        max_depth: 2 + rng.below(8) as usize,
        max_work: rng.below(400) as usize,
        max_product: rng.below(40) as usize,
        max_output: 1 + rng.below(8) as usize,
    }
}

fn entries(rng: &mut Rng, width: usize, n: usize) -> Vec<(Vec<i64>, i64)> {
    (0..n)
        .map(|_| {
            // Now and then a row of the wrong width.
            let w = if rng.below(25) == 0 { width + 1 } else { width };
            let row = (0..w).map(|_| rng.below(3) as i64 - 1).collect();
            (row, rng.weight())
        })
        .collect()
}

/// A valid set of fewer than `max` entries (generous limits), or the empty set.
fn set(rng: &mut Rng, width: usize, max: u64) -> ZSet {
    let n = rng.below(max) as usize;
    let wide = Limits::default();
    let raw: Vec<_> = entries(rng, width, n)
        .into_iter()
        .filter(|(r, _)| r.len() == width)
        .collect();
    ZSet::consolidate(width, &raw, wide).unwrap_or_else(|_| ZSet::empty(width, wide).unwrap())
}

#[test]
fn consolidation_and_sums_match_the_rust_build() {
    let mut rng = Rng(1069);
    let mut d = Digest(0xcbf2_9ce4_8422_2325);
    for _ in 0..4000 {
        let l = limits(&mut rng);
        let width = rng.below(3) as usize;
        let n = rng.below(14) as usize;
        let raw = entries(&mut rng, width, n);
        let z = ZSet::consolidate(width, &raw, l);
        d.add(&z);
        if let Ok(z) = &z {
            d.add(z.len());
            d.add(z.entries().collect::<Vec<_>>());
        }
        let a = set(&mut rng, width, 8);
        let b = set(&mut rng, width, 8);
        d.add(a.plus(&b, l));
        let c = set(&mut rng, width + 1, 3);
        d.add(a.plus(&c, l));
    }
    assert_eq!(d.0, GOLDEN_SETS, "digest {:#018x}", d.0);
}

fn random_circuit(rng: &mut Rng, l: Limits) -> Result<(vesper_flux::Circuit, [usize; 2]), Error> {
    let mut b = CircuitBuilder::new(l)?;
    let widths = [1 + rng.below(2) as usize, 1 + rng.below(2) as usize];
    let mut nodes: Vec<(NodeId, usize)> = Vec::new();
    for w in widths {
        nodes.push((b.input(w)?, w));
    }
    for _ in 0..rng.below(5) {
        let (x, wx) = nodes[rng.below(nodes.len() as u64) as usize];
        let (y, wy) = nodes[rng.below(nodes.len() as u64) as usize];
        let made = match rng.below(5) {
            0 => {
                let column = rng.below(wx as u64) as usize;
                let value = rng.below(3) as i64 - 1;
                let p = if rng.below(2) == 0 {
                    Predicate::Equal { column, value }
                } else {
                    Predicate::AtLeast { column, value }
                };
                b.filter(x, p).map(|id| (id, wx))
            }
            1 => {
                let fields: Vec<Field> = (0..1 + rng.below(3))
                    .map(|_| {
                        if rng.below(3) == 0 {
                            Field::Constant(rng.below(3) as i64)
                        } else {
                            Field::Column(rng.below(wx as u64) as usize)
                        }
                    })
                    .collect();
                let n = fields.len();
                b.map(x, fields).map(|id| (id, n))
            }
            2 if wx == wy => b.union(x, y).map(|id| (id, wx)),
            _ => {
                let ka = rng.below(wx as u64) as usize;
                let kb = rng.below(wy as u64) as usize;
                b.equijoin(x, y, ka, kb).map(|id| (id, wx + wy))
            }
        };
        match made {
            Ok(node) => nodes.push(node),
            Err(_) => break,
        }
    }
    let out = nodes[rng.below(nodes.len() as u64) as usize].0;
    Ok((b.finish(out)?, widths))
}

#[test]
fn circuit_ticks_and_batches_match_the_rust_build() {
    let mut rng = Rng(7);
    let mut d = Digest(0xcbf2_9ce4_8422_2325);
    for _ in 0..1500 {
        let l = limits(&mut rng);
        let l = Limits {
            max_work: l.max_work * 4,
            max_product: l.max_product * 2,
            ..l
        };
        let (circuit, widths) = match random_circuit(&mut rng, l) {
            Ok(c) => c,
            Err(e) => {
                d.add(e);
                continue;
            }
        };
        d.add(circuit.node_count());
        d.add(circuit.differentiated_node_count());
        let mut exec = circuit.clone().executor();
        let mut inputs = [
            ZSet::empty(widths[0], l).unwrap(),
            ZSet::empty(widths[1], l).unwrap(),
        ];
        for _ in 0..4 {
            let deltas = vec![set(&mut rng, widths[0], 4), set(&mut rng, widths[1], 4)];
            let out = exec.tick(&TickInput {
                deltas: deltas.clone(),
            });
            d.add(&out);
            d.add(exec.ticks());
            d.add(exec.materialized());
            if out.is_ok() {
                for (i, delta) in deltas.iter().enumerate() {
                    if let Ok(sum) = inputs[i].plus(delta, Limits::default()) {
                        inputs[i] = sum;
                    }
                }
            }
            d.add(circuit.batch(&inputs));
        }
    }
    assert_eq!(d.0, GOLDEN_CIRCUITS, "digest {:#018x}", d.0);
}

#[test]
fn frontier_operations_match_the_rust_build() {
    let mut rng = Rng(42);
    let mut d = Digest(0xcbf2_9ce4_8422_2325);
    for _ in 0..300 {
        let mut f = Frontier::new(rng.below(5) as i64, 1 + rng.below(4) as usize).unwrap();
        let mut tickets: Vec<FrontierTicket> = Vec::new();
        for _ in 0..30 {
            match rng.below(4) {
                0 => match f.acquire(rng.below(8) as i64) {
                    Ok(t) => {
                        d.add(t.timestamp());
                        tickets.push(t);
                    }
                    Err(e) => d.add(e),
                },
                1 if !tickets.is_empty() => {
                    let t = tickets.swap_remove(rng.below(tickets.len() as u64) as usize);
                    match f.advance(t, rng.below(10) as i64) {
                        Ok(t) => {
                            d.add(t.timestamp());
                            tickets.push(t);
                        }
                        Err(e) => {
                            d.add(&e.error);
                            tickets.push(e.ticket);
                        }
                    }
                }
                2 if !tickets.is_empty() => {
                    let t = tickets.swap_remove(rng.below(tickets.len() as u64) as usize);
                    match f.fork(t) {
                        Ok((a, b)) => {
                            d.add((a.timestamp(), b.timestamp()));
                            tickets.push(a);
                            tickets.push(b);
                        }
                        Err(e) => {
                            d.add(&e.error);
                            tickets.push(e.ticket);
                        }
                    }
                }
                3 if !tickets.is_empty() => {
                    let t = tickets.swap_remove(rng.below(tickets.len() as u64) as usize);
                    if let Err(e) = f.retire(t) {
                        d.add(&e.error);
                        tickets.push(e.ticket);
                    }
                }
                _ => {}
            }
            d.add((f.watermark(), f.live_holds()));
        }
        for t in tickets {
            d.add(f.retire(t).map_err(|e| e.error));
        }
        d.add((f.watermark(), f.live_holds()));
    }
    assert_eq!(d.0, GOLDEN_FRONTIER, "digest {:#018x}", d.0);
}

const GOLDEN_SETS: u64 = 0x9971_c492_20c1_406c;
const GOLDEN_CIRCUITS: u64 = 0x3138_890d_e370_6d24;
const GOLDEN_FRONTIER: u64 = 0xbf7e_4da6_1f1b_a0fb;

//! Differential test of the Vesper checkpoint image codec against the Rust
//! codec it replaced (`checkpoint_reference.rs`). Each case builds a seeded
//! circuit and state, then checkpoints it under seeded limits with both and
//! compares the images or errors; and restores seeded corruptions of a
//! valid image (flipped, truncated, extended and re-signed bytes) into two
//! fresh executors with both, comparing the outcome and the state.
use super::super::{CircuitBuilder, Executor, Field, Predicate, TickInput};
use super::{CheckpointLimits, reference};
use crate::{CdcHeader, CdcPolicy, Error, Limits, NodeId, SourceBinding, WeightPolicy, ZSet};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn cell(&mut self) -> i64 {
        match self.below(10) {
            0 => i64::MIN,
            1 => i64::MAX,
            2 => -1,
            _ => self.below(5) as i64,
        }
    }
}

/// A seeded executor: a small circuit over one to three inputs, bound to a
/// CDC source or ticked directly, with a few ticks or batches applied.
fn executor(seed: u64, run: bool) -> Executor {
    let mut rng = Rng(seed);
    let limits = Limits {
        max_rows: 64 + rng.below(512) as usize,
        ..Limits::default()
    };
    let mut b = CircuitBuilder::new(limits).unwrap();
    let inputs: Vec<(NodeId, usize)> = (0..1 + rng.below(3))
        .map(|_| {
            let w = 1 + rng.below(3) as usize;
            (b.input(w).unwrap(), w)
        })
        .collect();
    let mut nodes = inputs.clone();
    for _ in 0..rng.below(5) {
        let (a, wa) = nodes[rng.below(nodes.len() as u64) as usize];
        let next = match rng.below(4) {
            0 => {
                let column = rng.below(wa as u64) as usize;
                let value = rng.below(4) as i64;
                let p = if rng.below(2) == 0 {
                    Predicate::Equal { column, value }
                } else {
                    Predicate::AtLeast { column, value }
                };
                b.filter(a, p).map(|n| (n, wa))
            }
            1 => {
                let fields: Vec<Field> = (0..1 + rng.below(3))
                    .map(|_| {
                        if rng.below(3) == 0 {
                            Field::Constant(rng.cell())
                        } else {
                            Field::Column(rng.below(wa as u64) as usize)
                        }
                    })
                    .collect();
                let w = fields.len();
                b.map(a, fields).map(|n| (n, w))
            }
            2 => b.union(a, a).map(|n| (n, wa)),
            _ => {
                let (c, wc) = nodes[rng.below(nodes.len() as u64) as usize];
                b.equijoin(a, c, 0, 0).map(|n| (n, wa + wc))
            }
        };
        if let Ok(n) = next
            && n.1 <= 4
        {
            nodes.push(n);
        }
    }
    let out = nodes[rng.below(nodes.len() as u64) as usize].0;
    let mut e = b.finish(out).unwrap().executor();
    if rng.below(2) == 0 {
        let bindings: Vec<SourceBinding> = (0..inputs.len())
            .map(|i| SourceBinding {
                partition_id: 3 * i as u32 + 1,
                schema_id: 10 + i as u16,
                input: i,
                first_lsn: rng.below(3) * 1000,
                weights: if rng.below(2) == 0 {
                    WeightPolicy::Unit
                } else {
                    WeightPolicy::NonZero
                },
            })
            .collect();
        let policy = CdcPolicy {
            bindings,
            max_batch_bytes: 1 << 16,
            max_records: 4 + rng.below(32) as usize,
        };
        e.bind_sources(&policy).unwrap();
        for _ in 0..if run { rng.below(4) } else { 0 } {
            let mut bytes = Vec::new();
            let mut next: Vec<u64> = policy
                .bindings
                .iter()
                .map(|b| e.next_source_lsn(b.partition_id).unwrap())
                .collect();
            for _ in 0..1 + rng.below(policy.max_records as u64) {
                let i = rng.below(inputs.len() as u64) as usize;
                let b = &policy.bindings[i];
                let lsn = next[i];
                next[i] += 1;
                let weight = match b.weights {
                    WeightPolicy::Unit => [1, -1][rng.below(2) as usize],
                    WeightPolicy::NonZero => [1, -1, 2, -3][rng.below(4) as usize],
                };
                let row: Vec<i64> = (0..inputs[i].1).map(|_| rng.cell()).collect();
                let header = CdcHeader {
                    source_lsn: lsn,
                    event_ts_ms: rng.below(100) as i64,
                    z_weight: weight,
                    partition_id: b.partition_id,
                    schema_id: b.schema_id,
                    flags: 0,
                };
                bytes.extend(e.encode_cdc_record(header, &row).unwrap());
            }
            let _ = e.ingest_cdc(&bytes);
        }
    } else {
        for _ in 0..if run { rng.below(4) } else { 0 } {
            let deltas: Vec<ZSet> = inputs
                .iter()
                .map(|(_, w)| {
                    let entries: Vec<(Vec<i64>, i64)> = (0..rng.below(12))
                        .map(|_| {
                            let row = (0..*w).map(|_| rng.cell()).collect();
                            (row, [1, -1, 2][rng.below(3) as usize])
                        })
                        .collect();
                    ZSet::consolidate(*w, &entries, limits).unwrap()
                })
                .collect();
            let _ = e.tick(&TickInput { deltas });
        }
    }
    e
}

fn limits(rng: &mut Rng, image: usize) -> CheckpointLimits {
    let d = CheckpointLimits::default();
    CheckpointLimits {
        max_bytes: match rng.below(4) {
            0 => 64.max(image.saturating_sub(rng.below(40) as usize)),
            1 => 64.max(image),
            _ => d.max_bytes,
        },
        max_total_rows: match rng.below(4) {
            0 => rng.below(64) as usize,
            _ => d.max_total_rows,
        },
        max_total_cells: match rng.below(4) {
            0 => rng.below(128) as usize,
            _ => d.max_total_cells,
        },
        max_nodes: match rng.below(6) {
            0 => 1 + rng.below(6) as usize,
            _ => d.max_nodes,
        },
        max_sources: match rng.below(6) {
            0 => rng.below(3) as usize,
            _ => d.max_sources,
        },
        max_work: match rng.below(3) {
            0 => rng.below(600) as usize,
            _ => d.max_work,
        },
    }
}

fn corrupt(rng: &mut Rng, image: &[u8]) -> Vec<u8> {
    let mut v = image.to_vec();
    let resign = |v: &mut Vec<u8>| {
        if v.len() >= 32 {
            let n = v.len() - 32;
            let sum = blake3::hash(&v[..n]);
            v[n..].copy_from_slice(sum.as_bytes());
        }
    };
    match rng.below(7) {
        0 => {}
        1 => {
            let i = rng.below(v.len() as u64) as usize;
            v[i] ^= 1 << rng.below(8);
        }
        2 | 3 => {
            // A re-signed flip of a byte of the body, mostly in the head.
            let body = v.len() - 32;
            let i = if rng.below(2) == 0 {
                rng.below(body.min(120) as u64) as usize
            } else {
                rng.below(body as u64) as usize
            };
            v[i] = match rng.below(3) {
                0 => v[i] ^ (1 << rng.below(8)),
                1 => 0,
                _ => rng.below(256) as u8,
            };
            resign(&mut v);
        }
        4 => {
            v.truncate(rng.below(v.len() as u64) as usize);
            resign(&mut v);
        }
        5 => {
            let n = v.len() - 32;
            let extra: Vec<u8> = (0..1 + rng.below(24))
                .map(|_| rng.below(256) as u8)
                .collect();
            v.splice(n..n, extra);
            resign(&mut v);
        }
        _ => {
            v.truncate(rng.below(40) as usize);
        }
    }
    v
}

fn state(e: &Executor) -> String {
    format!("{:?} {} {:?}", e.prior, e.ticks, e.source)
}

#[test]
fn checkpoint_codec_matches_the_rust_codec() {
    let mut errors = std::collections::BTreeMap::<String, usize>::new();
    let mut restored = 0;
    for seed in 0..1500u64 {
        let mut rng = Rng(seed ^ 0x5EED);
        let e = executor(seed, true);
        let full = CheckpointLimits::default();
        let image = e.checkpoint(full).unwrap();
        assert_eq!(
            image,
            reference::checkpoint(&e, full).unwrap(),
            "seed {seed}"
        );
        for _ in 0..4 {
            let l = limits(&mut rng, image.len());
            let port = e.checkpoint(l);
            assert_eq!(port, reference::checkpoint(&e, l), "seed {seed} {l:?}");
            if let Err(err) = port {
                *errors.entry(format!("checkpoint {err:?}")).or_default() += 1;
            }
        }
        for _ in 0..6 {
            let bad = corrupt(&mut rng, &image);
            let l = if rng.below(3) == 0 {
                limits(&mut rng, bad.len())
            } else {
                full
            };
            // Fresh executors bound to the same source, as restore expects.
            let (mut a, mut b) = (executor(seed, false), executor(seed, false));
            let before = state(&a);
            let port = a.restore_checkpoint(&bad, l);
            let refr = reference::restore(&mut b, &bad, l);
            assert_eq!(port, refr, "seed {seed} {l:?}");
            assert_eq!(state(&a), state(&b), "seed {seed}");
            match port {
                Ok(()) => {
                    restored += 1;
                    // A re-signed change of an input's rows is a valid
                    // other state; the image itself restores the original.
                    if bad == image {
                        assert_eq!(state(&a), state(&e), "seed {seed}");
                    }
                }
                Err(err) => {
                    assert_eq!(state(&a), before, "seed {seed}");
                    *errors.entry(format!("restore {err:?}")).or_default() += 1;
                }
            }
        }
    }
    assert!(restored > 1000, "{restored} restores");
    // Every error the codec raises occurs.
    for want in [
        Error::Limit("work"),
        Error::Limit("checkpoint bytes"),
        Error::Limit("checkpoint state"),
        Error::Checkpoint("truncated"),
        Error::Checkpoint("checksum"),
        Error::Checkpoint("version"),
        Error::Checkpoint("identity"),
        Error::Checkpoint("node count"),
        Error::Checkpoint("row schema"),
        Error::Checkpoint("noncanonical rows"),
        Error::Checkpoint("trailing bytes"),
        Error::Checkpoint("source tag"),
        Error::Checkpoint("weight policy"),
        Error::Limit("checkpoint sources"),
        Error::Overflow,
    ] {
        assert!(
            errors.keys().any(|k| k.ends_with(&format!("{want:?}"))),
            "{want:?} never occurs: {errors:?}"
        );
    }
}

fn view(p: &super::durable::ParsedManifest) -> String {
    format!("{} {:?} {:?} {:?}", p.ticks, p.source, p.nodes, p.parent)
}

/// Seeded spines for `e`'s nodes: up to three batches each, in increasing
/// tick ranges (sometimes not), with random digests (sometimes repeated).
fn spines(rng: &mut Rng, e: &Executor) -> Vec<Vec<super::durable::BatchRef>> {
    use super::durable::{BatchRef, blob_id};
    let mut named = Vec::new();
    e.prior
        .iter()
        .map(|_| {
            let mut hi = 0u64;
            let count = if rng.below(30) == 0 { 9 } else { rng.below(4) };
            (0..count)
                .map(|_| {
                    let lo = if rng.below(12) == 0 {
                        hi.saturating_sub(1)
                    } else {
                        hi
                    };
                    hi = lo + 1 + rng.below(2);
                    if rng.below(16) == 0 {
                        hi = lo;
                    }
                    let digest: [u8; 32] = if !named.is_empty() && rng.below(12) == 0 {
                        named[rng.below(named.len() as u64) as usize]
                    } else {
                        std::array::from_fn(|_| rng.below(256) as u8)
                    };
                    named.push(digest);
                    let rows = match rng.below(10) {
                        0 => 0,
                        1 => e.circuit.limits.max_rows + 1,
                        _ => 1 + rng.below(40) as usize,
                    };
                    BatchRef {
                        id: blob_id(digest),
                        lo,
                        hi,
                        rows,
                    }
                })
                .collect()
        })
        .collect()
}

fn mutate(rng: &mut Rng, blob: &[u8]) -> Vec<u8> {
    let mut v = blob.to_vec();
    match rng.below(6) {
        0 | 1 => {}
        2 => {
            // Mostly in the head.
            let span = if rng.below(2) == 0 { 140 } else { usize::MAX };
            let i = rng.below(v.len().min(span) as u64) as usize;
            v[i] = match rng.below(3) {
                0 => v[i] ^ (1 << rng.below(8)),
                1 => 0,
                _ => rng.below(256) as u8,
            };
        }
        3 => v.truncate(rng.below(v.len() as u64) as usize),
        4 => v.extend((0..1 + rng.below(9)).map(|_| rng.below(256) as u8)),
        _ => {
            if let Some(last) = v.last_mut() {
                *last = rng.below(3) as u8;
            }
        }
    }
    v
}

#[test]
fn durable_codecs_match_the_rust_codecs() {
    use super::durable::{BatchRef, blob_id, decode_batch, decode_manifest, encode_manifest};
    use crate::Budget;
    let mut errors = std::collections::BTreeMap::<String, usize>::new();
    let mut note = |what: &str, r: &Result<(), Error>| {
        let key = match r {
            Ok(()) => format!("{what} ok"),
            Err(e) => format!("{what} {e:?}"),
        };
        *errors.entry(key).or_default() += 1;
    };
    for seed in 0..1500u64 {
        let mut rng = Rng(seed ^ 0xD0_5EED);
        let e = executor(seed, true);
        let full = CheckpointLimits::default();
        let fingerprint = super::identity(
            &e.circuit,
            e.source.as_ref(),
            full,
            &mut full.budget(&e.circuit),
        )
        .unwrap();
        // Manifests: encode under seeded limits and budgets, then decode
        // seeded mutations of the valid one.
        let sp = spines(&mut rng, &e);
        let parent = (rng.below(2) == 0).then(|| blob_id(std::array::from_fn(|i| i as u8)));
        let mut valid = None;
        for round in 0..3 {
            let l = if round == 0 {
                full
            } else {
                limits(&mut rng, 600)
            };
            let work = if round == 2 {
                rng.below(400) as usize
            } else {
                1 << 20
            };
            let (mut a, mut b) = (Budget { remaining: work }, Budget { remaining: work });
            let port = encode_manifest(&e, &fingerprint, &sp, parent.as_ref(), l, &mut a);
            let refr =
                reference::encode_manifest(&e, &fingerprint, &sp, parent.as_ref(), l, &mut b);
            assert_eq!(port, refr, "seed {seed}");
            if port.is_ok() {
                assert_eq!(a.remaining, b.remaining, "seed {seed}");
            }
            note(
                "manifest write",
                &port.as_ref().map(|_| ()).map_err(Clone::clone),
            );
            if round == 0 {
                valid = port.ok();
            }
        }
        let valid = valid.unwrap();
        for _ in 0..6 {
            let blob = mutate(&mut rng, &valid);
            let l = if rng.below(3) == 0 {
                limits(&mut rng, 600)
            } else {
                full
            };
            let work = if rng.below(4) == 0 {
                rng.below(200) as usize
            } else {
                1 << 20
            };
            let (mut a, mut b) = (Budget { remaining: work }, Budget { remaining: work });
            let port = decode_manifest(&blob, &e.circuit, &fingerprint, l, &mut a);
            let refr = reference::decode_manifest(&blob, &e.circuit, &fingerprint, l, &mut b);
            match (&port, &refr) {
                (Ok(p), Ok(r)) => {
                    assert_eq!(view(p), view(r), "seed {seed}");
                    assert_eq!(a.remaining, b.remaining, "seed {seed}");
                }
                (Err(p), Err(r)) => assert_eq!(p, r, "seed {seed}"),
                _ => panic!("seed {seed}: {:?} vs {:?}", port.is_ok(), refr.is_ok()),
            }
            note("manifest read", &port.map(|_| ()));
        }
        // Batches: each node's integral, encoded under seeded byte limits,
        // then decoded from seeded mutations against seeded references.
        for (node, set) in e.prior.iter().enumerate() {
            let lo = rng.below(3);
            let hi = lo + 1 + rng.below(3);
            let mut l = full;
            if rng.below(4) == 0 {
                l.max_bytes = 64 + rng.below(300) as usize;
            }
            let port = super::durable::encode_batch(&fingerprint, node, lo, hi, set, l);
            let refr = reference::encode_batch(&fingerprint, node, lo, hi, set, l);
            assert_eq!(port, refr, "seed {seed}");
            note(
                "batch write",
                &port.as_ref().map(|_| ()).map_err(Clone::clone),
            );
            let Ok(blob) = port
                .or_else(|_| super::durable::encode_batch(&fingerprint, node, lo, hi, set, full))
            else {
                unreachable!()
            };
            for _ in 0..3 {
                let bad = mutate(&mut rng, &blob);
                let mut b = BatchRef {
                    id: blob_id([0; 32]),
                    lo,
                    hi,
                    rows: set.len(),
                };
                let (mut n, mut w) = (node, set.width);
                match rng.below(8) {
                    0 => b.lo += 1,
                    1 => b.hi += 1,
                    2 => b.rows += 1,
                    3 => n += 1,
                    4 => w += 1,
                    _ => {}
                }
                let fp = if rng.below(10) == 0 {
                    [7; 32]
                } else {
                    fingerprint
                };
                let work = if rng.below(4) == 0 {
                    rng.below(200) as usize
                } else {
                    1 << 20
                };
                let (mut x, mut y) = (Budget { remaining: work }, Budget { remaining: work });
                let port = decode_batch(&bad, &b, &fp, n, w, &mut x);
                let refr = reference::decode_batch(&bad, &b, &fp, n, w, &mut y);
                assert_eq!(port, refr, "seed {seed}");
                if port.is_ok() {
                    assert_eq!(x.remaining, y.remaining, "seed {seed}");
                }
                note("batch read", &port.map(|_| ()));
            }
        }
    }
    for want in [
        "manifest write ok",
        "manifest write Limit(\"work\")",
        "manifest write Limit(\"checkpoint bytes\")",
        "manifest read ok",
        "manifest read Limit(\"work\")",
        "manifest read Limit(\"checkpoint state\")",
        "manifest read Checkpoint(\"manifest version\")",
        "manifest read Checkpoint(\"identity\")",
        "manifest read Checkpoint(\"truncated\")",
        "manifest read Checkpoint(\"node count\")",
        "manifest read Checkpoint(\"row schema\")",
        "manifest read Checkpoint(\"spine levels\")",
        "manifest read Checkpoint(\"spine ranges\")",
        "manifest read Checkpoint(\"batch rows\")",
        "manifest read Checkpoint(\"batch named twice\")",
        "manifest read Checkpoint(\"parent tag\")",
        "manifest read Checkpoint(\"trailing bytes\")",
        "batch write ok",
        "batch write Limit(\"checkpoint bytes\")",
        "batch read ok",
        "batch read Limit(\"work\")",
        "batch read Checkpoint(\"batch version\")",
        "batch read Checkpoint(\"batch identity\")",
        "batch read Checkpoint(\"batch header\")",
        "batch read Checkpoint(\"truncated\")",
        "batch read Checkpoint(\"noncanonical rows\")",
        "batch read Checkpoint(\"trailing bytes\")",
    ] {
        assert!(errors.contains_key(want), "{want} never occurs: {errors:?}");
    }
}

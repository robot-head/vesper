//! Whole-store ABBA benchmark comparing the frozen Rust seed copy
//! (`vesper_loom_cas_rust`, baseline A) with the Vesper port (`vesper_loom_cas`,
//! candidate B) in one process (#1411).
//!
//! Each epoch runs A, B, B, A after a warmup pass so OS cache and frequency
//! drift cancel across the pair. `tempfile::TempDir` setup and cleanup live
//! outside the timed window.
//!
//! Workloads:
//! - `put_blob`
//! - `read_blob`
//! - `put_tree`
//! - `ingest_dir`
//! - `materialize`
//! - `publish_derivation`
//! - `lookup_derivation`
//! - `export_pack`
//! - `verify_all`
//!
//! Usage:
//! - `abba compare [epochs]`
//! - `abba flip-gate <workload>` (`ABBA_EPOCHS` defaults to 30)

use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::time::Instant;
use tempfile::TempDir;
use vesper_bench::{PairedSample, analyze_pairs};

fn make_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed | 1;
    let mut out = vec![0u8; len];
    for chunk in out.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let b = x.to_le_bytes();
        let n = chunk.len();
        chunk.copy_from_slice(&b[..n]);
    }
    out
}

fn build_sample_workspace(dir: &Path) {
    let sub1 = dir.join("src").join("core");
    let sub2 = dir.join("src").join("util");
    let sub3 = dir.join("assets").join("data");
    fs::create_dir_all(&sub1).unwrap();
    fs::create_dir_all(&sub2).unwrap();
    fs::create_dir_all(&sub3).unwrap();
    fs::write(dir.join("Loom.toml"), make_bytes(1, 512)).unwrap();
    fs::write(dir.join("README.md"), make_bytes(2, 1024)).unwrap();
    for i in 0..10u64 {
        let sz = 512 + (i as usize * 701) % 8192;
        fs::write(
            sub1.join(format!("mod_{i:02}.ves")),
            make_bytes(100 + i, sz),
        )
        .unwrap();
        fs::write(
            sub2.join(format!("helper_{i:02}.ves")),
            make_bytes(200 + i, sz),
        )
        .unwrap();
        fs::write(
            sub3.join(format!("blob_{i:02}.bin")),
            make_bytes(300 + i, sz + 1024),
        )
        .unwrap();
    }
}

fn digest_of(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

macro_rules! define_arm {
    ($m:ident, $krate:ident) => {
        mod $m {
            use super::*;
            use $krate::transfer::{TransferLimits, export_pack};
            use $krate::{DerivationInputs, Domain, MaterializeOptions, ObjectId, Store, Tree};

            pub fn put_blob_once() -> (u64, u64) {
                let payloads: Vec<Vec<u8>> = (0..64u64)
                    .map(|i| make_bytes(10 + i, 512 + ((i as usize * 997) % 16_384)))
                    .chain([make_bytes(500, 262_144), make_bytes(501, 262_144)])
                    .collect();
                let dir = TempDir::new().unwrap();
                let store = Store::open_unsynced(dir.path()).unwrap();
                let start = Instant::now();
                let mut acc = 0u64;
                for p in &payloads {
                    let ing = store.put_blob(Domain::Artifact, p).unwrap();
                    acc =
                        acc.wrapping_add(digest_of(&ing.id.digest.0) ^ u64::from(ing.newly_stored));
                }
                for p in &payloads[..64] {
                    let ing = store.put_blob(Domain::Artifact, p).unwrap();
                    acc =
                        acc.wrapping_add(digest_of(&ing.id.digest.0) ^ u64::from(ing.newly_stored));
                }
                let elapsed = start.elapsed().as_nanos() as u64;
                drop(store);
                drop(dir);
                (elapsed, acc)
            }

            pub struct ReadBench {
                _dir: TempDir,
                store: Store,
                ids: Vec<ObjectId>,
            }

            impl ReadBench {
                pub fn new() -> Self {
                    let dir = TempDir::new().unwrap();
                    let store = Store::open_unsynced(dir.path()).unwrap();
                    let mut ids = Vec::new();
                    for i in 0..24u64 {
                        let d = make_bytes(1000 + i, 8192);
                        ids.push(store.put_blob(Domain::Source, &d).unwrap().id);
                    }
                    for i in 0..6u64 {
                        let d = make_bytes(2000 + i, 65_536);
                        ids.push(store.put_blob(Domain::Artifact, &d).unwrap().id);
                    }
                    for i in 0..2u64 {
                        let d = make_bytes(3000 + i, 524_288);
                        ids.push(store.put_blob(Domain::Interface, &d).unwrap().id);
                    }
                    Self {
                        _dir: dir,
                        store,
                        ids,
                    }
                }

                pub fn run(&self) -> (u64, u64) {
                    let start = Instant::now();
                    let mut acc = 0u64;
                    for _ in 0..4 {
                        for id in &self.ids {
                            let b = self.store.read_blob(id).unwrap();
                            acc = acc.wrapping_add(b.len() as u64 ^ u64::from(b[0]));
                        }
                    }
                    (start.elapsed().as_nanos() as u64, acc)
                }
            }

            pub fn put_tree_once() -> (u64, u64) {
                let dir = TempDir::new().unwrap();
                let store = Store::open_unsynced(dir.path()).unwrap();
                let mut blobs = Vec::new();
                for i in 0..32u64 {
                    blobs.push(
                        store
                            .put_blob(Domain::Source, &make_bytes(400 + i, 256))
                            .unwrap(),
                    );
                }
                let trees: Vec<Tree> = (0..24usize)
                    .map(|t| {
                        let mut tree = Tree::new(Domain::Source);
                        for j in 0..16usize {
                            let b = &blobs[(t + j) % blobs.len()];
                            tree.insert_file(
                                &format!("f_{j:02}_{t:02}.ves"),
                                b.id,
                                b.size,
                                j % 3 == 0,
                            )
                            .unwrap();
                        }
                        tree
                    })
                    .collect();
                let start = Instant::now();
                let mut acc = 0u64;
                for t in &trees {
                    let ing = store.put_tree(t).unwrap();
                    acc =
                        acc.wrapping_add(digest_of(&ing.id.digest.0) ^ u64::from(ing.newly_stored));
                }
                for t in &trees {
                    let ing = store.put_tree(t).unwrap();
                    acc =
                        acc.wrapping_add(digest_of(&ing.id.digest.0) ^ u64::from(ing.newly_stored));
                }
                let elapsed = start.elapsed().as_nanos() as u64;
                drop(store);
                drop(dir);
                (elapsed, acc)
            }

            pub fn ingest_dir_once(ws: &Path) -> (u64, u64) {
                let dir = TempDir::new().unwrap();
                let store = Store::open_unsynced(dir.path()).unwrap();
                let start = Instant::now();
                let ing = store.ingest_dir(Domain::Source, ws).unwrap();
                let elapsed = start.elapsed().as_nanos() as u64;
                let acc = digest_of(&ing.id.digest.0) ^ ing.size;
                drop(store);
                drop(dir);
                (elapsed, acc)
            }

            pub struct MaterializeBench {
                _dir: TempDir,
                store: Store,
                root: ObjectId,
            }

            impl MaterializeBench {
                pub fn new(ws: &Path) -> Self {
                    let dir = TempDir::new().unwrap();
                    let store = Store::open_unsynced(dir.path()).unwrap();
                    let root = store.ingest_dir(Domain::Source, ws).unwrap().id;
                    Self {
                        _dir: dir,
                        store,
                        root,
                    }
                }

                pub fn run(&self) -> (u64, u64) {
                    let out_parent = TempDir::new().unwrap();
                    let dest = out_parent.path().join("workspace");
                    let start = Instant::now();
                    let rep = self
                        .store
                        .materialize(&self.root, &dest, MaterializeOptions::default())
                        .unwrap();
                    let elapsed = start.elapsed().as_nanos() as u64;
                    let acc = rep.directories ^ rep.files.wrapping_mul(31) ^ rep.bytes;
                    drop(out_parent);
                    (elapsed, acc)
                }
            }

            pub fn publish_derivation_once() -> (u64, u64) {
                let dir = TempDir::new().unwrap();
                let store = Store::open_unsynced(dir.path()).unwrap();
                let mut inputs_pool = Vec::new();
                for i in 0..8u64 {
                    inputs_pool.push(
                        store
                            .put_blob(Domain::Source, &make_bytes(700 + i, 256))
                            .unwrap()
                            .id,
                    );
                }
                let output_id = store
                    .put_blob(Domain::Artifact, &make_bytes(777, 1024))
                    .unwrap()
                    .id;
                let drvs: Vec<DerivationInputs> = (0..32u64)
                    .map(|k| {
                        let mut d = DerivationInputs::new(
                            Domain::Artifact,
                            "vesper-cc",
                            format!("0.1.{k}"),
                        )
                        .with_target("x86_64-unknown-linux-gnu")
                        .with_option("opt", "2")
                        .with_option("kind", "obj")
                        .with_option("slot", format!("{k}"));
                        for (idx, id) in inputs_pool.iter().take(4).enumerate() {
                            d = d.with_input(format!("in_{idx}"), *id);
                        }
                        d
                    })
                    .collect();
                let start = Instant::now();
                let mut acc = 0u64;
                for d in &drvs {
                    let out = store.publish_derivation(d, output_id).unwrap();
                    acc = acc.wrapping_add(digest_of(&out.key.0.0) ^ u64::from(out.newly_recorded));
                }
                for d in &drvs {
                    let out = store.publish_derivation(d, output_id).unwrap();
                    acc = acc.wrapping_add(digest_of(&out.key.0.0) ^ u64::from(out.newly_recorded));
                }
                let elapsed = start.elapsed().as_nanos() as u64;
                drop(store);
                drop(dir);
                (elapsed, acc)
            }

            pub struct LookupBench {
                _dir: TempDir,
                store: Store,
                hits: Vec<DerivationInputs>,
                misses: Vec<DerivationInputs>,
            }

            impl LookupBench {
                pub fn new() -> Self {
                    let dir = TempDir::new().unwrap();
                    let store = Store::open_unsynced(dir.path()).unwrap();
                    let inp = store
                        .put_blob(Domain::Source, &make_bytes(900, 512))
                        .unwrap()
                        .id;
                    let out = store
                        .put_blob(Domain::Artifact, &make_bytes(901, 512))
                        .unwrap()
                        .id;
                    let mut hits = Vec::new();
                    for k in 0..64u64 {
                        let d = DerivationInputs::new(
                            Domain::Artifact,
                            "vesper-cc",
                            format!("1.0.{k}"),
                        )
                        .with_target("x86_64-unknown-linux-gnu")
                        .with_option("opt", "2")
                        .with_input("src", inp);
                        store.publish_derivation(&d, out).unwrap();
                        hits.push(d);
                    }
                    let mut misses = Vec::new();
                    for k in 100..116u64 {
                        misses.push(
                            DerivationInputs::new(
                                Domain::Artifact,
                                "vesper-cc",
                                format!("1.0.{k}"),
                            )
                            .with_target("x86_64-unknown-linux-gnu")
                            .with_option("opt", "2")
                            .with_input("src", inp),
                        );
                    }
                    Self {
                        _dir: dir,
                        store,
                        hits,
                        misses,
                    }
                }

                pub fn run(&self) -> (u64, u64) {
                    let start = Instant::now();
                    let mut acc = 0u64;
                    for _ in 0..4 {
                        for d in &self.hits {
                            let id = self.store.lookup_derivation(d).unwrap().unwrap();
                            acc = acc.wrapping_add(digest_of(&id.digest.0));
                        }
                        for d in &self.misses {
                            let res = self.store.lookup_derivation(d).unwrap();
                            acc = acc.wrapping_add(u64::from(res.is_some()));
                        }
                    }
                    (start.elapsed().as_nanos() as u64, acc)
                }
            }

            pub struct PackBench {
                _dir: TempDir,
                store: Store,
                root: ObjectId,
            }

            impl PackBench {
                pub fn new(ws: &Path) -> Self {
                    let dir = TempDir::new().unwrap();
                    let store = Store::open_unsynced(dir.path()).unwrap();
                    let sub_a = store.ingest_dir(Domain::Source, ws).unwrap();
                    let mut shared = Tree::new(Domain::Source);
                    shared.insert_dir("a", sub_a.id).unwrap();
                    shared.insert_dir("b", sub_a.id).unwrap();
                    let b1 = store
                        .put_blob(Domain::Source, &make_bytes(88, 65_536))
                        .unwrap();
                    shared
                        .insert_file("big.bin", b1.id, b1.size, false)
                        .unwrap();
                    let root = store.put_tree(&shared).unwrap().id;
                    Self {
                        _dir: dir,
                        store,
                        root,
                    }
                }

                pub fn run(&self) -> (u64, u64) {
                    let start = Instant::now();
                    let mut acc = 0u64;
                    for _ in 0..8 {
                        let pack =
                            export_pack(&self.store, self.root, TransferLimits::default()).unwrap();
                        acc = acc.wrapping_add(digest_of(&pack) ^ pack.len() as u64);
                    }
                    (start.elapsed().as_nanos() as u64, acc)
                }
            }

            pub struct VerifyAllBench {
                _dir: TempDir,
                store: Store,
            }

            impl VerifyAllBench {
                pub fn new(ws: &Path) -> Self {
                    let dir = TempDir::new().unwrap();
                    let store = Store::open_unsynced(dir.path()).unwrap();
                    let root = store.ingest_dir(Domain::Source, ws).unwrap().id;
                    for i in 0..16u64 {
                        let out = store
                            .put_blob(Domain::Artifact, &make_bytes(6000 + i, 4096))
                            .unwrap()
                            .id;
                        let d = DerivationInputs::new(
                            Domain::Artifact,
                            "vesper-cc",
                            format!("0.2.{i}"),
                        )
                        .with_input("root", root);
                        store.publish_derivation(&d, out).unwrap();
                    }
                    Self { _dir: dir, store }
                }

                pub fn run(&self) -> (u64, u64) {
                    let start = Instant::now();
                    let mut acc = 0u64;
                    for _ in 0..6 {
                        let rep = self.store.verify_all().unwrap();
                        acc = acc
                            .wrapping_add(rep.verified_objects ^ (rep.verified_derivations << 16));
                    }
                    (start.elapsed().as_nanos() as u64, acc)
                }
            }
        }
    };
}

define_arm!(rust_arm, vesper_loom_cas_rust);
define_arm!(port_arm, vesper_loom_cas);

fn measure_abba(
    name: &str,
    epochs: usize,
    gate: bool,
    mut run_rust: impl FnMut() -> (u64, u64),
    mut run_port: impl FnMut() -> (u64, u64),
) {
    // Parity check + warmup before timing.
    let (r_warm, r_digest) = run_rust();
    let (p_warm, p_digest) = run_port();
    black_box((r_warm, p_warm));
    assert_eq!(r_digest, p_digest, "workload {name} diverged");

    let mut pairs = Vec::with_capacity(epochs);
    for _ in 0..epochs {
        let (a1, da1) = run_rust();
        let (b1, db1) = run_port();
        let (b2, db2) = run_port();
        let (a2, da2) = run_rust();
        assert_eq!(da1, db1, "workload {name} diverged");
        assert_eq!(da2, db2, "workload {name} diverged");
        pairs.push(PairedSample::new(a1.max(1) + a2.max(1), b1.max(1) + b2.max(1), 2).unwrap());
    }
    if gate {
        let line: Vec<String> = pairs
            .iter()
            .map(|p| format!("{}:{}", p.baseline_total(), p.candidate_total()))
            .collect();
        println!("flip-gate pairs {name} {}", line.join(" "));
        return;
    }
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let rust_ms = median(
        pairs
            .iter()
            .map(|p| p.baseline_total() as f64 / 2_000_000.0)
            .collect(),
    );
    let port_ms = median(
        pairs
            .iter()
            .map(|p| p.candidate_total() as f64 / 2_000_000.0)
            .collect(),
    );
    let ratios: Vec<f64> = pairs
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    let lo = ratios.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = ratios.iter().copied().fold(0.0, f64::max);
    let ratio = median(ratios);
    let analysis = analyze_pairs(&pairs, 0).unwrap();
    let ci_lo = 1.0 + analysis.median_lower.regression_bp_bounds().floor as f64 / 10_000.0;
    let ci_hi = 1.0 + analysis.median_upper.regression_bp_bounds().floor as f64 / 10_000.0;
    println!(
        "{name:<20} rust {rust_ms:>7.2} ms  port {port_ms:>7.2} ms  port/rust {ratio:.3}x \
         (95% CI [{ci_lo:.3}x, {ci_hi:.3}x], range {lo:.3}x..{hi:.3}x, n={epochs})"
    );
}

fn run(only: Option<&str>, epochs: usize, gate: bool) {
    assert_eq!(vesper_loom_cas::IMPLEMENTATION, "vesper");
    assert_eq!(vesper_loom_cas_rust::IMPLEMENTATION, "rust");

    let ws_dir = TempDir::new().unwrap();
    build_sample_workspace(ws_dir.path());
    let ws = ws_dir.path();

    let all = [
        "put_blob",
        "read_blob",
        "put_tree",
        "ingest_dir",
        "materialize",
        "publish_derivation",
        "lookup_derivation",
        "export_pack",
        "verify_all",
    ];
    for w in all {
        if only.is_some_and(|o| o != w) {
            continue;
        }
        match w {
            "put_blob" => measure_abba(
                w,
                epochs,
                gate,
                rust_arm::put_blob_once,
                port_arm::put_blob_once,
            ),
            "read_blob" => {
                let r = rust_arm::ReadBench::new();
                let p = port_arm::ReadBench::new();
                measure_abba(w, epochs, gate, || r.run(), || p.run());
            }
            "put_tree" => measure_abba(
                w,
                epochs,
                gate,
                rust_arm::put_tree_once,
                port_arm::put_tree_once,
            ),
            "ingest_dir" => measure_abba(
                w,
                epochs,
                gate,
                || rust_arm::ingest_dir_once(ws),
                || port_arm::ingest_dir_once(ws),
            ),
            "materialize" => {
                let r = rust_arm::MaterializeBench::new(ws);
                let p = port_arm::MaterializeBench::new(ws);
                measure_abba(w, epochs, gate, || r.run(), || p.run());
            }
            "publish_derivation" => measure_abba(
                w,
                epochs,
                gate,
                rust_arm::publish_derivation_once,
                port_arm::publish_derivation_once,
            ),
            "lookup_derivation" => {
                let r = rust_arm::LookupBench::new();
                let p = port_arm::LookupBench::new();
                measure_abba(w, epochs, gate, || r.run(), || p.run());
            }
            "export_pack" => {
                let r = rust_arm::PackBench::new(ws);
                let p = port_arm::PackBench::new(ws);
                measure_abba(w, epochs, gate, || r.run(), || p.run());
            }
            "verify_all" => {
                let r = rust_arm::VerifyAllBench::new(ws);
                let p = port_arm::VerifyAllBench::new(ws);
                measure_abba(w, epochs, gate, || r.run(), || p.run());
            }
            _ => unreachable!(),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => run(None, 30, false),
        ["compare"] => run(None, 30, false),
        ["compare", epochs] => run(None, epochs.parse().unwrap(), false),
        ["compare", epochs, only] => run(Some(only), epochs.parse().unwrap(), false),
        ["flip-gate", workload] => {
            let epochs = std::env::var("ABBA_EPOCHS")
                .ok()
                .and_then(|e| e.parse().ok())
                .unwrap_or(30);
            run(Some(workload), epochs, true);
        }
        _ => {
            eprintln!("usage: abba [compare [epochs] [workload] | flip-gate <workload>]");
            std::process::exit(2);
        }
    }
}

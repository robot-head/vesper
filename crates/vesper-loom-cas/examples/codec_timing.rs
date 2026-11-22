//! Rough timings of the ported codecs, for comparing the Rust build with
//! the Vesper port (`cargo run -p vesper-loom-cas-port --example
//! codec_timing_port`). Not the flip gate's ABBA
//! benchmark: one build per run, median of 15 rounds.
use std::hint::black_box;
use std::time::Instant;

use vesper_loom_cas::{DerivationInputs, Domain, Store, Tree, blob_id, validate_name};

fn median(mut f: impl FnMut()) -> f64 {
    let mut times: Vec<f64> = (0..15)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times[7]
}

fn main() {
    let names: Vec<String> = (0..1000).map(|i| format!("module_{i:04}.ves")).collect();
    let mut tree = Tree::new(Domain::Source);
    for n in &names {
        tree.insert_file(n, blob_id(Domain::Source, n.as_bytes()), 100, false)
            .unwrap();
    }
    let enc = tree.encode();
    let big = vec![7u8; 1 << 20];
    let mut drv = DerivationInputs::new(Domain::Artifact, "vesper-codegen-clif", "0.1.0");
    for n in names.iter().take(64) {
        drv = drv.with_input(n.clone(), blob_id(Domain::Source, n.as_bytes()));
    }
    let drv_enc = drv.encode().unwrap();
    let dir = std::env::temp_dir().join(format!("cas-timing-{}", std::process::id()));
    let store = Store::open_unsynced(&dir).unwrap();
    let small: Vec<Vec<u8>> = (0..200u32)
        .map(|i| format!("object {i} {}", "x".repeat(200)).into_bytes())
        .collect();
    let stored: Vec<_> = small
        .iter()
        .map(|b| store.put_blob(Domain::Artifact, b).unwrap().id)
        .collect();
    let big_id = store.put_blob(Domain::Artifact, &big).unwrap().id;
    let mut fresh = 0u64;
    let rows: [(&str, f64); 12] = [
        (
            "validate_name x1000",
            median(|| {
                for n in &names {
                    black_box(validate_name(n)).unwrap();
                }
            }),
        ),
        (
            "blob_id 64 B x1000",
            median(|| {
                for n in &names {
                    black_box(blob_id(Domain::Source, n.as_bytes()));
                }
            }),
        ),
        (
            "blob_id 1 MiB",
            median(|| {
                black_box(blob_id(Domain::Artifact, &big));
            }),
        ),
        (
            "tree encode 1000",
            median(|| {
                black_box(tree.encode());
            }),
        ),
        (
            "tree decode 1000",
            median(|| {
                black_box(Tree::decode(Domain::Source, &enc)).unwrap();
            }),
        ),
        (
            "drv key 64 inputs",
            median(|| {
                black_box(drv.key()).unwrap();
            }),
        ),
        (
            "drv decode 64 inputs",
            median(|| {
                black_box(DerivationInputs::decode(&drv_enc)).unwrap();
            }),
        ),
        (
            "store open",
            median(|| {
                black_box(Store::open_unsynced(&dir)).unwrap();
            }),
        ),
        (
            "put_blob new 256 B x200",
            median(|| {
                fresh += 1;
                for b in &small {
                    let mut b = b.clone();
                    b.extend_from_slice(&fresh.to_le_bytes());
                    black_box(store.put_blob(Domain::Artifact, &b)).unwrap();
                }
            }),
        ),
        (
            "put_blob dedup 256 B x200",
            median(|| {
                for b in &small {
                    black_box(store.put_blob(Domain::Artifact, b)).unwrap();
                }
            }),
        ),
        (
            "read_blob 256 B x200",
            median(|| {
                for id in &stored {
                    black_box(store.read_blob(id)).unwrap();
                }
            }),
        ),
        (
            "read_blob 1 MiB",
            median(|| {
                black_box(store.read_blob(&big_id)).unwrap();
            }),
        ),
    ];
    let _ = std::fs::remove_dir_all(&dir);
    for (name, us) in rows {
        println!("{name:24} {us:10.1} us");
    }
}

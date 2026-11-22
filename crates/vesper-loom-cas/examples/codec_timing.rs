//! Rough timings of the ported codecs, for comparing the Rust build with
//! the Vesper port (`--features vesper-port`). Not the flip gate's ABBA
//! benchmark: one build per run, median of 15 rounds.
use std::hint::black_box;
use std::time::Instant;

use vesper_loom_cas::{DerivationInputs, Domain, Tree, blob_id, validate_name};

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
    let rows: [(&str, f64); 7] = [
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
    ];
    for (name, us) in rows {
        println!("{name:24} {us:10.1} us");
    }
}

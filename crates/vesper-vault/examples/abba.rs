//! ABBA comparison of the frozen Rust seed (`vesper_vault_rust`, baseline)
//! and the Vesper port (`vesper_vault`, candidate) in one process: each epoch
//! times both arms in A B B A order and analyzes the paired ratios with
//! `vesper-bench`.
//!
//! ```text
//! abba compare <epochs> [ops]       # human summary with exact 95% interval
//! abba flip-gate <workload> <ops>   # `flip-gate pairs ...` for flip_gate.py
//! abba one <rust|port> <workload> <ops>
//! ```

use std::time::Instant;
use vesper_bench::{PairedSample, analyze_pairs};

macro_rules! workloads {
    ($m:ident, $krate:ident) => {
        mod $m {
            use $krate::kem::server_encapsulate_for_testing;
            use $krate::{
                AeadAlgorithm, AeadOpener, AeadSealer, Aes256Gcm, ChaCha20Poly1305,
                CiphertextFrame, FRAME_HEADER_LEN, HybridClientSecret, LockPolicy, SecretSlot,
                verify_tag_16_ct,
            };

            const PAYLOAD_LEN: usize = 1200;
            const FRAMES_PER_OP: u64 = 512;

            fn slot32(byte: u8) -> SecretSlot<32> {
                SecretSlot::<32>::from_fn(LockPolicy::BestEffort, |k| k.fill(byte)).unwrap()
            }

            fn aead_work<A: AeadAlgorithm>(seed: u64, ops: u64) -> u64 {
                let aad = [0xA5u8; 48];
                let total_frames = ops.max(1) * FRAMES_PER_OP;
                let mut sealer =
                    AeadSealer::<A>::with_record_limit(slot32(seed as u8 | 1), 1, u64::MAX);
                let mut opener = AeadOpener::<A>::new(slot32(seed as u8 | 1), 1);
                let mut buf = [0u8; FRAME_HEADER_LEN + PAYLOAD_LEN];
                let mut acc = 0u64;
                for i in 0..total_frames {
                    if i > 0 && i % 256 == 0 {
                        let (next_s, _r1) = sealer.rotate(slot32((seed ^ i) as u8 | 1)).unwrap();
                        let _r2 = opener.retire();
                        sealer = next_s;
                        opener = AeadOpener::<A>::new(slot32((seed ^ i) as u8 | 1), sealer.epoch());
                    }
                    buf[FRAME_HEADER_LEN..FRAME_HEADER_LEN + 8].copy_from_slice(&i.to_le_bytes());
                    let ticket = sealer.mint_nonce().unwrap();
                    let sealed = sealer.seal_in_place(ticket, &aad, &mut buf).unwrap();
                    let pt = opener
                        .open_in_place(&aad, CiphertextFrame::new(&mut buf))
                        .unwrap();
                    let ok = verify_tag_16_ct(&pt.header().tag, &pt.header().tag);
                    acc = acc.wrapping_add(sealed.frame_len as u64 ^ u64::from(ok));
                    acc ^= u64::from(pt.bytes()[0]);
                }
                let _ = sealer.retire();
                let _ = opener.retire();
                acc
            }

            pub fn aead_aes(seed: u64, ops: u64) -> u64 {
                aead_work::<Aes256Gcm>(seed, ops)
            }

            pub fn aead_chacha(seed: u64, ops: u64) -> u64 {
                aead_work::<ChaCha20Poly1305>(seed, ops)
            }

            pub fn kem_hybrid(seed: u64, ops: u64) -> u64 {
                let mut acc = 0u64;
                let iters = ops.max(1);
                for i in 0..iters {
                    let s = seed.wrapping_add(i).to_le_bytes();
                    let mut ml_seed = [0u8; 64];
                    for (j, b) in ml_seed.iter_mut().enumerate() {
                        *b = s[j % 8].wrapping_add(j as u8);
                    }
                    let mut x_sec = [0u8; 32];
                    for (j, b) in x_sec.iter_mut().enumerate() {
                        *b = s[j % 8].wrapping_mul(3).wrapping_add(j as u8);
                    }
                    let (client, cshare) = HybridClientSecret::from_seeds_for_testing(
                        &mut ml_seed,
                        &mut x_sec,
                        LockPolicy::BestEffort,
                    )
                    .unwrap();

                    let mut m = [0u8; 32];
                    let mut eph = [0u8; 32];
                    for j in 0..32 {
                        m[j] = (i as u8).wrapping_add(j as u8);
                        eph[j] = (seed as u8).wrapping_add((j as u8).wrapping_mul(5));
                    }
                    let (sshare, server_ss) =
                        server_encapsulate_for_testing(&cshare, &m, &eph, LockPolicy::BestEffort)
                            .unwrap();
                    let client_ss = client.decapsulate(&sshare, LockPolicy::BestEffort).unwrap();

                    let s_tag: [u8; 16] = server_ss.with(|b| b[0..16].try_into().unwrap());
                    let c_tag: [u8; 16] = client_ss.with(|b| b[0..16].try_into().unwrap());
                    assert!(verify_tag_16_ct(&s_tag, &c_tag));
                    acc = acc.wrapping_add(u64::from_le_bytes(c_tag[0..8].try_into().unwrap()));
                    let _ = server_ss.shred();
                    let _ = client_ss.shred();
                }
                acc
            }
        }
    };
}

workloads!(rust_build, vesper_vault_rust);
workloads!(port_build, vesper_vault);

type Work = fn(u64, u64) -> u64;

const WORKLOADS: [(&str, u64, Work, Work); 3] = [
    ("aead-aes", 16, rust_build::aead_aes, port_build::aead_aes),
    (
        "aead-chacha",
        16,
        rust_build::aead_chacha,
        port_build::aead_chacha,
    ),
    (
        "kem-hybrid",
        256,
        rust_build::kem_hybrid,
        port_build::kem_hybrid,
    ),
];

fn measure(work: Work, seed: u64, ops: u64) -> u64 {
    let warm = work(seed, (ops / 8).max(1));
    std::hint::black_box(warm);
    let start = Instant::now();
    let sink = work(seed, ops);
    let elapsed = start.elapsed().as_nanos() as u64;
    std::hint::black_box(sink);
    elapsed.max(1)
}

fn ratio_of(sample: &PairedSample) -> f64 {
    sample.candidate_total() as f64 / sample.baseline_total() as f64
}

fn compare(epochs: usize, ops_override: Option<u64>, only: Option<&str>, gate: bool) {
    for &(name, default_ops, rust, port) in &WORKLOADS {
        if only.is_some_and(|o| o != name) {
            continue;
        }
        let ops = ops_override.unwrap_or(default_ops);
        assert_eq!(
            rust(42, 2),
            port(42, 2),
            "{name} result differs between rust and port"
        );
        let mut pairs = Vec::with_capacity(epochs);
        for ep in 0..epochs {
            let seed = (ep as u64).wrapping_add(1);
            let a1 = measure(rust, seed, ops);
            let b1 = measure(port, seed, ops);
            let b2 = measure(port, seed, ops);
            let a2 = measure(rust, seed, ops);
            pairs.push(PairedSample::new(a1 + a2, b1 + b2, 2 * ops).unwrap());
        }
        if gate {
            let line: Vec<String> = pairs
                .iter()
                .map(|p| format!("{}:{}", p.baseline_total(), p.candidate_total()))
                .collect();
            println!("flip-gate pairs {name} {}", line.join(" "));
            continue;
        }
        let mut ratios: Vec<f64> = pairs.iter().map(ratio_of).collect();
        ratios.sort_by(f64::total_cmp);
        let median_ratio = ratios[ratios.len() / 2];
        let mut rust_ns: Vec<f64> = pairs
            .iter()
            .map(|p| p.baseline_total() as f64 / (2 * ops) as f64)
            .collect();
        let mut port_ns: Vec<f64> = pairs
            .iter()
            .map(|p| p.candidate_total() as f64 / (2 * ops) as f64)
            .collect();
        rust_ns.sort_by(f64::total_cmp);
        port_ns.sort_by(f64::total_cmp);
        let analysis = analyze_pairs(&pairs, 5000).unwrap();
        let interval = analysis
            .interval
            .expect("n >= 6 produces an exact 95% interval");
        let lo95 = ratio_of(&interval.lower);
        let hi95 = ratio_of(&interval.upper);
        println!(
            "{name:<12} rust {:>9.0} ns/op  port {:>9.0} ns/op  port/rust {median_ratio:.3}x \
             (95% CI {lo95:.3}x..{hi95:.3}x, {epochs} epochs, ops={ops})",
            rust_ns[rust_ns.len() / 2],
            port_ns[port_ns.len() / 2],
        );
        assert!(
            hi95 <= 1.50,
            "{name} 95% upper bound {hi95:.3}x exceeds 1.50x flip-gate threshold"
        );
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
        ["compare", epochs] => compare(epochs.parse().unwrap(), None, None, false),
        ["compare", epochs, ops] => compare(
            epochs.parse().unwrap(),
            Some(ops.parse().unwrap()),
            None,
            false,
        ),
        ["flip-gate", workload, ops] => {
            let epochs = std::env::var("ABBA_EPOCHS")
                .ok()
                .and_then(|e| e.parse().ok())
                .unwrap_or(30);
            compare(epochs, Some(ops.parse().unwrap()), Some(workload), true);
        }
        ["one", build, workload, ops] => {
            let (_, _, rust, port) = WORKLOADS
                .iter()
                .find(|w| w.0 == *workload)
                .expect("a workload name");
            let work = if *build == "rust" { *rust } else { *port };
            println!("{}", work(1, ops.parse().unwrap()));
        }
        _ => {
            eprintln!(
                "usage: abba compare <epochs> [ops] | abba flip-gate <workload> <ops> | abba one <rust|port> <workload> <ops>"
            );
            std::process::exit(2);
        }
    }
}

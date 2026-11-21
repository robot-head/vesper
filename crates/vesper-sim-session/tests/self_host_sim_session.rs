//! Transcript differential for the Vesper port (self-hosting wave SH5
//! lane 19, #1342).
//!
//! Each case records a fixture and digests everything the run produced
//! except the execution identity (whose executable digest differs between
//! any two builds): the SimPulse journal, the source and model traces, the
//! outputs, the durable bytes, the lost tasks and the virtual time. The
//! digests in `fixtures/transcripts.txt` were blessed from the Rust build;
//! `cargo test --features vesper-port` must reproduce every one of them.
//! Rebless (Rust build only) with `VESPER_SIM_TRANSCRIPTS_BLESS=1`.
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use vesper_sim_session::{CheckedProgram, RunResult, Scenario, Session};

const PIPELINE: &str = include_str!("../fixtures/pipeline.ves");
const PIPELINE_SCENARIO: &str = include_str!("../fixtures/pipeline.json");
const KV: &str = include_str!("../fixtures/kv.ves");
const KV_SCENARIO: &str = include_str!("../fixtures/kv_ack_first.json");
const WARP: &str = include_str!("../fixtures/warp.ves");
const WAN: &str = include_str!("../fixtures/warp_wan.json");
const ACK: &str = include_str!("../fixtures/warp_ack.ves");
const ACK_SCENARIO: &str = include_str!("../fixtures/warp_ack.json");
const GOLDEN: &str = include_str!("../fixtures/transcripts.txt");

/// `(name, source, scenario)`; the Warp cases also run under a second seed,
/// which redraw every link's loss, reordering and duplication.
fn cases() -> Vec<(String, String, Json)> {
    let parse = |s: &str| -> Json { serde_json::from_str(s).unwrap() };
    let mut cases = vec![
        ("pipeline".into(), PIPELINE.into(), parse(PIPELINE_SCENARIO)),
        ("kv_ack_first".into(), KV.into(), parse(KV_SCENARIO)),
    ];
    for seed in [None, Some(1u64)] {
        let suffix = seed.map_or(String::new(), |s| format!("_seed{s}"));
        let mut wan = parse(WAN);
        let mut ack = parse(ACK_SCENARIO);
        if let Some(seed) = seed {
            wan["seed"] = json!(seed);
            ack["seed"] = json!(seed);
        }
        cases.push((format!("warp_wan{suffix}"), WARP.into(), wan));
        cases.push((format!("warp_ack{suffix}"), format!("{WARP}\n{ACK}"), ack));
    }
    cases
}

fn digest(source: &str, scenario: &Json) -> String {
    let program = CheckedProgram::new(source).unwrap_or_else(|e| panic!("{e}"));
    let scenario = Scenario::from_json(&scenario.to_string()).unwrap();
    let report = match Session::record(&program, scenario).unwrap().run(u64::MAX) {
        RunResult::Complete(report) => report,
        other => panic!("expected completion, got {other:?}"),
    };
    let r = &report.recording;
    let transcript = json!({
        "journal": serde_json::to_value(&r.journal).unwrap(),
        "trace": serde_json::to_value(&r.trace).unwrap(),
        "outputs": serde_json::to_value(&r.outputs).unwrap(),
        "model_trace": serde_json::to_value(&r.model_trace).unwrap(),
        "durable": serde_json::to_value(&r.durable).unwrap(),
        "lost": serde_json::to_value(&r.lost).unwrap(),
        "virtual_ns": report.virtual_ns,
    });
    Sha256::digest(transcript.to_string().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn transcripts_match_the_rust_build() {
    let golden: Vec<(&str, &str)> = GOLDEN
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split_once(' ').expect("`<name> <digest>` lines"))
        .collect();
    let mut blessed = String::from(
        "# SHA-256 of each fixture's transcript under the Rust build\n# (tests/self_host_sim_session.rs).\n",
    );
    let mut differences = vec![];
    for (name, source, scenario) in cases() {
        let start = std::time::Instant::now();
        let current = digest(&source, &scenario);
        eprintln!("{name}: {:?}", start.elapsed());
        blessed.push_str(&format!("{name} {current}\n"));
        match golden.iter().find(|(n, _)| *n == name) {
            Some((_, expected)) if *expected == current => {}
            expected => differences.push(format!("{name}: {current}, expected {expected:?}")),
        }
    }
    // Only the Rust build blesses; the port build always compares.
    if !cfg!(feature = "vesper-port")
        && std::env::var("VESPER_SIM_TRANSCRIPTS_BLESS").as_deref() == Ok("1")
    {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/transcripts.txt");
        std::fs::write(path, blessed).unwrap();
        return;
    }
    assert!(
        differences.is_empty(),
        "{} transcript differences:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// The link model alone: 10^6 transmissions over a Gilbert–Elliott chain,
/// independent loss, reordering and a queue that sometimes overflows.
/// `cargo test --release --test self_host_sim_session -- --ignored
/// --nocapture`, with and without `--features vesper-port`.
#[test]
#[ignore = "timing; run by hand in release"]
fn link_throughput() {
    use vesper_sim_session::model::link::{
        Bottleneck, Fate, GilbertElliott, Link, LinkSpec, Reorder,
    };
    let spec = LinkSpec {
        name: "wan".into(),
        base_delay_ns: 20_000,
        gilbert_elliott: Some(GilbertElliott {
            good_to_bad_ppm: 10_000,
            bad_to_good_ppm: 100_000,
            loss_good_ppm: 1_000,
            loss_bad_ppm: 500_000,
        }),
        loss_ppm: 5_000,
        duplicate_ppm: 10_000,
        reorder: Some(Reorder {
            ppm: 20_000,
            extra_delay_ns: 50_000,
        }),
        bottleneck: Bottleneck {
            buffer_packets: 64,
            rate_bytes_per_sec: 125_000_000,
        },
    };
    let n = 1_000_000u64;
    let mut best = std::time::Duration::MAX;
    let mut check = 0u64;
    for _ in 0..5 {
        let mut link = Link::new(spec.clone(), 35);
        let start = std::time::Instant::now();
        let mut acc = 0u64;
        for i in 0..n {
            // 1,200-byte datagrams every 8 µs against 9.6 µs of service.
            let t = i * 8_000;
            acc = acc.wrapping_add(u64::from(link.duplicates()));
            match link.transmit(t, t, 1_200).unwrap().fate {
                Fate::Delivered { arrive_at_ns, .. } => acc = acc.wrapping_add(arrive_at_ns),
                Fate::Lost { .. } => acc = acc.wrapping_add(1),
            }
        }
        best = best.min(start.elapsed());
        check = acc;
    }
    eprintln!(
        "link: {:.1} ns per datagram (best of 5), checksum {check}",
        best.as_nanos() as f64 / n as f64
    );
}

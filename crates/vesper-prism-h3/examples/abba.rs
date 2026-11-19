//! ABBA benchmark of the Rust and Vesper builds of vesper-prism-h3 (port
//! wave P3b lane 5, #1067): QPACK decoding of the committed QIFs corpus,
//! QPACK encoding of its field lists, and frame parsing (`frames`): request
//! streams of HEADERS and DATA frames, a control stream of SETTINGS and
//! PRIORITY_UPDATE frames, and capsules, read in packet-sized pushes; and
//! message validation (`messages`) of the netbsd QIF's field lists as requests
//! or responses.
//!
//! The two builds are one crate compiled with and without `vesper-port`, so
//! they cannot share a process. Each arm is a worker process (this example
//! built one way or the other) that runs one batch of the workload per
//! request on stdin; the driver times each request with `vesper-bench`'s
//! `run_abba`, so both arms carry the same pipe round trip. A batch is long
//! (milliseconds) next to that round trip. Every batch returns a digest of
//! its results, and the driver checks that both builds agree. `once` runs
//! batches without the driver, for callgrind instruction counts.
//!
//! ```text
//! crates/vesper-prism-h3/scripts/bench.sh       # builds both, runs every workload
//! abba worker <decode|encode|frames|messages>                # one arm
//! abba drive <decode|encode|frames|messages> <rust-worker> <vesper-worker>
//! abba once <decode|encode|frames|messages> <batches>
//! ```
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};
use vesper_prism_h3::capsule::{self, Capsule, CapsuleReader};
use vesper_prism_h3::frame::{self, Frame, FrameReader, Settings};
use vesper_prism_h3::message;
use vesper_prism_h3::priority::{Priority, PriorityUpdate};
use vesper_prism_h3::qpack::{Decoded, Decoder, DecoderConfig, Encoder, EncoderConfig, Field};

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qifs")
}

/// One QIFs offline-interop file: its table capacity, blocked-stream limit
/// and (stream, bytes) records.
struct Encoded {
    capacity: u64,
    blocked: u64,
    records: Vec<(u64, Vec<u8>)>,
}

fn corpus() -> Vec<Encoded> {
    let mut out = Vec::new();
    let root = fixtures().join("encoded/qpack-05");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    dirs.sort();
    for dir in dirs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        for file in files {
            let name = file.file_name().unwrap().to_str().unwrap().to_owned();
            let Some((_, params)) = name.split_once(".out.") else {
                continue;
            };
            let p: Vec<u64> = params.split('.').map(|x| x.parse().unwrap()).collect();
            let data = std::fs::read(&file).unwrap();
            let mut records = Vec::new();
            let mut at = 0;
            while at < data.len() {
                let stream = u64::from_be_bytes(data[at..at + 8].try_into().unwrap());
                let len = u32::from_be_bytes(data[at + 8..at + 12].try_into().unwrap()) as usize;
                records.push((stream, data[at + 12..at + 12 + len].to_vec()));
                at += 12 + len;
            }
            out.push(Encoded {
                capacity: p[0],
                blocked: p[1],
                records,
            });
        }
    }
    out
}

fn qif_blocks() -> Vec<Vec<Field>> {
    let text = std::fs::read_to_string(fixtures().join("qifs/netbsd.qif")).unwrap();
    let mut blocks = Vec::new();
    let mut cur = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        if line.is_empty() {
            if !cur.is_empty() {
                blocks.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let (n, v) = line.split_once('\t').unwrap_or((line, ""));
        cur.push(Field::new(n, v));
    }
    if !cur.is_empty() {
        blocks.push(cur);
    }
    blocks
}

fn digest_fields(mut h: u64, fields: &[Field]) -> u64 {
    for f in fields {
        h = fnv(h, &f.name);
        h = fnv(h, &f.value);
    }
    h
}

/// Decodes all 70 corpus files (1,330 field sections).
fn decode_batch(corpus: &[Encoded]) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    for file in corpus {
        let mut d = Decoder::new(DecoderConfig {
            max_table_capacity: file.capacity,
            max_blocked_streams: file.blocked,
            max_field_section_size: 1 << 20,
            max_blocked_bytes: 1 << 20,
        });
        if file.capacity > 0 {
            let mut set = Vec::new();
            vesper_prism_h3::qpack::prefix::encode_int(&mut set, 0x20, 5, file.capacity);
            d.on_encoder_stream(&set).unwrap();
        }
        for (stream, bytes) in &file.records {
            if *stream == 0 {
                d.on_encoder_stream(bytes).unwrap();
                while let Some(s) = d.poll_unblocked() {
                    digest = digest_fields(digest, &s.unwrap().fields);
                }
            } else if let Decoded::Fields(s) = d.decode_section(*stream, bytes).unwrap() {
                digest = digest_fields(digest, &s.fields);
            }
            digest = fnv(digest, &d.take_decoder_stream());
        }
    }
    digest
}

/// Encodes the corpus's field lists under four table settings, feeding
/// each section's acknowledgment back, as a server's encoder sees it.
fn encode_batch(blocks: &[Vec<Field>]) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    for (capacity, blocked) in [(0, 0), (256, 0), (4096, 0), (4096, 100)] {
        let mut enc = Encoder::new(EncoderConfig {
            max_table_capacity: capacity,
            ..EncoderConfig::default()
        });
        let mut dec = Decoder::new(DecoderConfig {
            max_table_capacity: capacity,
            max_blocked_streams: blocked,
            max_field_section_size: 1 << 20,
            max_blocked_bytes: 1 << 20,
        });
        enc.apply_peer_settings(capacity, blocked);
        for round in 0..4u64 {
            for (i, block) in blocks.iter().enumerate() {
                let stream = 4 * (round * blocks.len() as u64 + i as u64);
                let section = enc.encode(stream, block);
                let instructions = enc.take_encoder_stream();
                digest = fnv(digest, &section);
                digest = fnv(digest, &instructions);
                dec.on_encoder_stream(&instructions).unwrap();
                while dec.poll_unblocked().is_some() {}
                let _ = dec.decode_section(stream, &section).unwrap();
                enc.on_decoder_stream(&dec.take_decoder_stream()).unwrap();
            }
        }
    }
    digest
}

/// The frame workload's streams (built once): 64 request streams, each
/// HEADERS (a section of the corpus) then DATA frames of 1 to 16 KiB; a
/// control stream; and a capsule stream.
struct Streams {
    requests: Vec<Vec<u8>>,
    control: Vec<u8>,
    capsules: Vec<u8>,
}

fn streams(corpus: &[Encoded]) -> Streams {
    let sections: Vec<&[u8]> = corpus
        .iter()
        .flat_map(|f| f.records.iter().filter(|(s, _)| *s != 0))
        .map(|(_, b)| b.as_slice())
        .collect();
    let mut requests = Vec::new();
    for i in 0..64usize {
        let mut w = Vec::new();
        frame::write_frame(&mut w, frame::HEADERS, sections[i * 7 % sections.len()]);
        for k in 0..4 {
            let len = 1024 * (1 + (i * 5 + k * 3) % 16);
            frame::write_frame(&mut w, frame::DATA, &vec![(i + k) as u8; len]);
        }
        frame::write_frame(&mut w, 0x21 + 0x1f * (i as u64 % 5), b"grease");
        requests.push(w);
    }
    let mut control = Vec::new();
    Settings {
        qpack_max_table_capacity: 4096,
        qpack_blocked_streams: 100,
        max_field_section_size: Some(65536),
        enable_connect_protocol: true,
        h3_datagram: true,
        webtransport_max_sessions: Some(8),
        no_rfc7540_priorities: true,
    }
    .encode(&mut control);
    for i in 0..256u64 {
        PriorityUpdate {
            push: false,
            element_id: 4 * i,
            priority: Priority::new((i % 8) as u8, i % 3 == 0),
        }
        .encode(&mut control);
    }
    frame::write_varint_frame(&mut control, frame::GOAWAY, 1 << 20);
    let mut capsules = Vec::new();
    for i in 0..512usize {
        capsule::write_capsule(
            &mut capsules,
            capsule::DATAGRAM,
            &vec![i as u8; 32 + i % 1000],
        );
        if i % 16 == 0 {
            capsule::write_capsule(&mut capsules, 0x17, &[0; 100]);
        }
    }
    capsule::write_close_session(&mut capsules, 0x1234, b"done");
    Streams {
        requests,
        control,
        capsules,
    }
}

/// Bytes arrive in packet-sized pieces.
const PACKET: usize = 1200;

fn frames_batch(s: &Streams) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    let frame_digest = |digest: &mut u64, f: Frame| {
        *digest = fnv(*digest, &f.frame_type().to_be_bytes());
        match f {
            Frame::Data(d) | Frame::Headers(d) => *digest = fnv(*digest, &d),
            Frame::PriorityUpdate(u) => {
                let p = u.priority;
                *digest = fnv(*digest, &[p.urgency, u8::from(p.incremental)]);
                // The scheduler's view of the update.
                *digest = fnv(
                    *digest,
                    Priority::parse(p.serialize().as_bytes())
                        .serialize()
                        .as_bytes(),
                );
            }
            Frame::Settings(st) => {
                *digest = fnv(*digest, &st.qpack_max_table_capacity.to_be_bytes())
            }
            _ => {}
        }
    };
    for w in s.requests.iter().chain([&s.control]) {
        let mut r = FrameReader::new(1 << 16);
        for chunk in w.chunks(PACKET) {
            r.push(chunk);
            while let Some(f) = r.next_frame().unwrap() {
                frame_digest(&mut digest, f);
            }
        }
        r.finish().unwrap();
    }
    let mut r = CapsuleReader::new(1 << 16);
    for chunk in s.capsules.chunks(PACKET) {
        r.push(chunk);
        while let Some(c) = r.next_capsule().unwrap() {
            match c {
                Capsule::Datagram(d) => digest = fnv(digest, &d),
                Capsule::CloseSession { code, reason } => {
                    digest = fnv(digest, &code.to_be_bytes());
                    digest = fnv(digest, &reason);
                }
                Capsule::DrainSession => digest = fnv(digest, b"drain"),
                Capsule::Unknown(t) => digest = fnv(digest, &t.to_be_bytes()),
            }
        }
    }
    r.finish().unwrap();
    digest
}

/// Validates every field list of the QIFs corpus (requests, and responses
/// when it starts with `:status`), 64 rounds.
fn messages_batch(lists: &[Vec<Field>]) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    for _ in 0..64 {
        for list in lists {
            if list.first().is_some_and(|f| f.name == b":status") {
                match message::parse_response(list.clone()) {
                    Ok(r) => digest = fnv(digest, &r.status.to_be_bytes()),
                    Err(e) => digest = fnv(digest, format!("{e:?}").as_bytes()),
                }
            } else {
                match message::parse_request(list.clone()) {
                    Ok(r) => {
                        digest = fnv(digest, r.path.as_bytes());
                        digest = fnv(digest, &r.fields.len().to_be_bytes());
                    }
                    Err(e) => digest = fnv(digest, format!("{e:?}").as_bytes()),
                }
            }
        }
    }
    digest
}

fn frame_count(_s: &Streams) -> usize {
    // 64 requests x 6 frames, the control stream's 258, and the capsules.
    64 * 6 + 258 + 512 + 512 / 16 + 1
}

struct Workload {
    corpus: Vec<Encoded>,
    blocks: Vec<Vec<Field>>,
    streams: Streams,
    lists: Vec<Vec<Field>>,
}

impl Workload {
    fn load() -> Self {
        let corpus = corpus();
        let streams = streams(&corpus);
        Self {
            corpus,
            blocks: qif_blocks(),
            streams,
            lists: qif_blocks(),
        }
    }

    fn run(&self, workload: &str) -> u64 {
        match workload {
            "decode" => decode_batch(&self.corpus),
            "encode" => encode_batch(&self.blocks),
            "frames" => frames_batch(&self.streams),
            "messages" => messages_batch(&self.lists),
            other => panic!("unknown workload {other}"),
        }
    }
}

/// Serve batches: each stdin line `go <n>` runs one batch and answers with
/// its digest.
fn worker(workload: &str) {
    let w = Workload::load();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.expect("read request");
        assert!(line.starts_with("go "), "request `go <n>`");
        writeln!(out, "{}", w.run(workload)).expect("answer");
        out.flush().expect("flush");
    }
}

/// One arm: a worker process timed per request.
struct Arm {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    digests: Vec<u64>,
}

impl Arm {
    fn spawn(binary: &str, workload: &str) -> Arm {
        let mut child = Command::new(binary)
            .args(["worker", workload])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {binary}: {e}"));
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Arm {
            child,
            stdin,
            stdout,
            digests: Vec::new(),
        }
    }
}

impl BenchCase for Arm {
    type Input = usize;
    type Output = u64;
    type Error = std::io::Error;
    fn setup(&mut self, i: Invocation) -> Result<usize, std::io::Error> {
        Ok(i.operation)
    }
    fn run(&mut self, n: usize) -> Result<u64, std::io::Error> {
        writeln!(self.stdin, "go {n}")?;
        self.stdin.flush()?;
        let mut line = String::new();
        self.stdout.read_line(&mut line)?;
        line.trim()
            .parse()
            .map_err(|e| std::io::Error::other(format!("worker answered {line:?}: {e}")))
    }
    fn teardown(&mut self, output: u64) -> Result<(), std::io::Error> {
        self.digests.push(output);
        Ok(())
    }
}

fn drive(workload: &str, rust: &str, vesper: &str) {
    let w = Workload::load();
    let (per_batch, unit) = match workload {
        "decode" => (
            w.corpus
                .iter()
                .map(|f| f.records.iter().filter(|(s, _)| *s != 0).count())
                .sum::<usize>(),
            "section",
        ),
        "encode" => (16 * w.blocks.len(), "section"),
        "frames" => (frame_count(&w.streams), "frame"),
        "messages" => (64 * w.lists.len(), "message"),
        other => panic!("unknown workload {other}"),
    };
    let operations = 16;
    let config = RunConfig {
        epochs: 12,
        warmup_epochs: 1,
        operations_per_window: operations,
        max_elapsed_nanos: 600_000_000_000,
        ..RunConfig::default()
    };
    let mut baseline = Arm::spawn(rust, workload);
    let mut candidate = Arm::spawn(vesper, workload);
    let report = run_abba(config, &mut baseline, &mut candidate).expect("ABBA run");
    let want = w.run(workload);
    assert!(
        baseline
            .digests
            .iter()
            .chain(&candidate.digests)
            .all(|d| *d == want),
        "the Rust and Vesper builds computed different results"
    );
    let mut ratios: Vec<f64> = report
        .pairs()
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let per_unit = |total: u64| total as f64 / (2 * operations) as f64 / per_batch as f64;
    let mut rust_ns: Vec<f64> = report
        .pairs()
        .iter()
        .map(|p| per_unit(p.baseline_total()))
        .collect();
    let mut vesper_ns: Vec<f64> = report
        .pairs()
        .iter()
        .map(|p| per_unit(p.candidate_total()))
        .collect();
    rust_ns.sort_by(f64::total_cmp);
    vesper_ns.sort_by(f64::total_cmp);
    let median = |v: &[f64]| (v[(v.len() - 1) / 2] + v[v.len() / 2]) / 2.0;
    println!(
        "{workload}: {} measured epochs x A/B/B/A x {operations} batches of {per_batch} {unit}s; \
         Rust median {:.0} ns/{unit}, Vesper median {:.0} ns/{unit}, \
         Vesper/Rust median {:.2}x (min {:.2}x, max {:.2}x)",
        report.pairs().len(),
        median(&rust_ns),
        median(&vesper_ns),
        median(&ratios),
        ratios[0],
        ratios[ratios.len() - 1],
    );
    let analysis = analyze_pairs(report.pairs(), 10_000).expect("analysis");
    println!(
        "{workload}: 2x gate (candidate at most 10000 bp slower): {:?}; digests agree over {} batches",
        analysis.decision,
        baseline.digests.len() + candidate.digests.len()
    );
    if let Some(interval) = analysis.interval {
        println!(
            "{workload}: 95% median interval of Vesper/Rust: {:.3}x..{:.3}x",
            ratios[interval.lower_rank - 1],
            ratios[interval.upper_rank - 1]
        );
    }
    for arm in [&mut baseline, &mut candidate] {
        let _ = arm.child.kill();
        let _ = arm.child.wait();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[1..] {
        ["worker", workload] => worker(workload),
        ["drive", workload, rust, vesper] => drive(workload, rust, vesper),
        ["once", workload, batches] => {
            let w = Workload::load();
            let mut digest = 0;
            for _ in 0..batches.parse::<u32>().expect("batches") {
                digest ^= w.run(workload);
            }
            println!("{digest}");
        }
        _ => {
            eprintln!(
                "usage: abba worker <decode|encode> | abba drive <decode|encode|frames|messages> <rust-worker> <vesper-worker> | abba once <decode|encode|frames|messages> <batches>"
            );
            std::process::exit(2);
        }
    }
}

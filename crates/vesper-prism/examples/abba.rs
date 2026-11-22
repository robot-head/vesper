//! ABBA comparison of the replaced Rust implementation (baseline) and the
//! Vesper port (candidate), port parity wave lane 3 (#951). The Rust
//! implementation now lives only in history (SH6 lane 5, #1400), so this
//! example is built twice, once at a commit that still has it, and, in
//! `compare` mode, runs the two executables as separate processes in Rust,
//! port, port, Rust order per epoch, then analyzes the paired totals with
//! vesper-bench. Run `crates/vesper-prism/bench-abba.sh`.
//!
//! `abba measure <ops>` warms each workload up and prints its total
//! nanoseconds over `<ops>` operations. `abba compare <rust> <port> <epochs>
//! <ops>` drives the two builds.
//!
//! One operation handles a pipelined batch of 32 requests (GET and POST with
//! bodies, 5-7 header fields each, about 6 KiB in all):
//! - `decode`: push the batch into a `Decoder` and drain every request.
//! - `route`: `Target::parse` plus `Router::resolve` of each target against a
//!   14-route table.
//! - `encode`: build one response with three fields and a body per request.
//! - `connection`: the whole pipeline on a `Connection`: receive the batch,
//!   issue every request, submit a response for each, write the output.
//!
//! The batch, the targets and the router are built in setup, outside the timed
//! body.
use std::{hint::black_box, process::Command, time::Instant};
use vesper_bench::{PairedSample, analyze_pairs};
use vesper_prism::{
    Decoder, Limits,
    connection::{Connection, ConnectionLimits, RequestEvent, Response},
    response::{ConnectionPolicy, ResponseBuilder, ResponseContext, ResponseLimits},
    router::{Resolution, Router, RouterLimits},
    target::{Target, TargetLimits},
};

const BATCH: usize = 32;

const ROUTES: [(&str, &str); 14] = [
    ("GET", "/"),
    ("GET", "/health"),
    ("GET", "/static/:file"),
    ("GET", "/api/v1/users"),
    ("POST", "/api/v1/users"),
    ("GET", "/api/v1/users/:id"),
    ("PUT", "/api/v1/users/:id"),
    ("DELETE", "/api/v1/users/:id"),
    ("GET", "/api/v1/users/:id/posts"),
    ("GET", "/api/v1/users/:id/posts/:post"),
    ("POST", "/api/v1/users/:id/posts"),
    ("GET", "/api/v1/search"),
    ("GET", "/api/v2/items/:item"),
    ("POST", "/api/v2/items/:item/events"),
];

/// Request `i` of the batch: (method, target, body).
fn request(i: usize) -> (&'static str, String, Vec<u8>) {
    match i % 8 {
        0 => ("GET", "/".to_owned(), Vec::new()),
        1 => ("GET", format!("/static/app-{i}.js"), Vec::new()),
        2 => ("GET", format!("/api/v1/users/{}", 1000 + i), Vec::new()),
        3 => (
            "GET",
            format!("/api/v1/users/{}/posts/{}?expand=author&limit=20", i, i * 7),
            Vec::new(),
        ),
        4 => (
            "POST",
            "/api/v1/users".to_owned(),
            format!("{{\"name\":\"user {i}\",\"email\":\"u{i}@example.test\"}}").into_bytes(),
        ),
        5 => (
            "GET",
            format!("/api/v1/search?q=caf%C3%A9+{i}&page=2&sort=recent"),
            Vec::new(),
        ),
        6 => ("PUT", format!("/api/v1/users/{i}"), vec![b'x'; 96 + i]),
        _ => (
            "POST",
            format!("/api/v2/items/item-{i}/events"),
            vec![b'e'; 240],
        ),
    }
}

fn wire() -> (Vec<u8>, Vec<(&'static str, String)>) {
    let mut bytes = Vec::new();
    let mut targets = Vec::new();
    for i in 0..BATCH {
        let (method, target, body) = request(i);
        bytes.extend_from_slice(format!("{method} {target} HTTP/1.1\r\n").as_bytes());
        bytes.extend_from_slice(b"Host: api.example.test:8080\r\n");
        bytes.extend_from_slice(b"User-Agent: prism-bench/1.0 (x86_64; rv:1)\r\n");
        bytes.extend_from_slice(b"Accept: application/json, text/plain;q=0.9, */*;q=0.1\r\n");
        bytes.extend_from_slice(b"Accept-Encoding: gzip, deflate, br\r\n");
        bytes.extend_from_slice(format!("X-Request-Id: 4f1c9a2e-{i:04}-7c3d-b8e1\r\n").as_bytes());
        if !body.is_empty() {
            bytes.extend_from_slice(b"Content-Type: application/json\r\n");
            bytes.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
        }
        bytes.extend_from_slice(b"Connection: keep-alive\r\n\r\n");
        bytes.extend_from_slice(&body);
        targets.push((method, target));
    }
    (bytes, targets)
}

fn limits() -> Limits {
    Limits {
        max_head_bytes: 8 * 1024,
        max_headers: 64,
        max_body_bytes: 64 * 1024,
        max_buffered_bytes: 1024 * 1024,
    }
}

fn router() -> Router<usize> {
    let mut router = Router::new(RouterLimits::default()).unwrap();
    for (i, (method, pattern)) in ROUTES.iter().enumerate() {
        router.insert(method, pattern, i).unwrap();
    }
    router
}

fn encode(i: usize, body: &[u8]) -> usize {
    ResponseBuilder::new(
        200,
        ResponseContext::Ordinary,
        ConnectionPolicy::KeepAlive,
        body.len(),
        ResponseLimits::default(),
    )
    .unwrap()
    .header("Content-Type", b"application/json; charset=utf-8")
    .unwrap()
    .header("Cache-Control", b"no-store")
    .unwrap()
    .header("X-Request-Id", format!("4f1c9a2e-{i:04}").as_bytes())
    .unwrap()
    .finish(body)
    .unwrap()
    .len()
}

const WORKLOADS: [&str; 4] = ["decode", "route", "encode", "connection"];

/// Total nanoseconds for `ops` timed operations after `ops / 4` warm-up ones.
fn measure(name: &str, ops: u64) -> u128 {
    let (bytes, targets) = wire();
    let router = router();
    let bodies: Vec<Vec<u8>> = (0..BATCH)
        .map(|i| format!("{{\"id\":{i},\"ok\":true,\"items\":[1,2,3]}}").into_bytes())
        .collect();
    let mut once: Box<dyn FnMut() -> usize> = match name {
        "decode" => Box::new(|| {
            let mut decoder = Decoder::new(limits()).unwrap();
            decoder.push(&bytes).unwrap();
            let mut n = 0;
            while let Some(decoded) = decoder.next_request().unwrap() {
                n += decoded.consumed;
            }
            assert_eq!(n, bytes.len());
            n
        }),
        "route" => Box::new(|| {
            let mut n = 0;
            for (method, raw) in &targets {
                let target = Target::parse(raw, TargetLimits::default()).unwrap();
                match router.resolve(method, &target).unwrap() {
                    Resolution::Matched(found) => n += found.value + found.params().len(),
                    _ => panic!("{method} {raw} did not match"),
                }
            }
            n
        }),
        "encode" => Box::new(|| {
            bodies
                .iter()
                .enumerate()
                .map(|(i, body)| encode(i, body))
                .sum()
        }),
        "connection" => Box::new(|| {
            let mut connection = Connection::new(ConnectionLimits::default()).unwrap();
            let mut rest = bytes.as_slice();
            let mut written = 0;
            let mut issued = Vec::with_capacity(BATCH);
            // At most `max_in_flight` (16) requests are issued at a time, so
            // the loop runs until every request of the batch was answered.
            let mut answered = 0;
            while answered < BATCH {
                let accepted = connection.receive(rest).unwrap();
                rest = &rest[accepted..];
                while let RequestEvent::Request(inbound) = connection.poll_request() {
                    issued.push(inbound.id);
                }
                assert!(!issued.is_empty(), "the connection stalled");
                for id in issued.drain(..) {
                    let body = bodies[answered % BATCH].clone();
                    answered += 1;
                    connection
                        .submit(
                            id,
                            Response::new(200, body).header("Cache-Control", b"no-store"),
                        )
                        .unwrap();
                }
                loop {
                    let n = connection.pending_output().len();
                    if n == 0 {
                        break;
                    }
                    written += n;
                    connection.advance_output(n).unwrap();
                }
            }
            assert_eq!(answered, BATCH);
            written
        }),
        _ => unreachable!(),
    };
    let mut sink = 0;
    for _ in 0..ops / 4 {
        sink += once();
    }
    let start = Instant::now();
    for _ in 0..ops {
        sink += once();
    }
    let elapsed = start.elapsed().as_nanos();
    black_box(sink);
    elapsed
}

fn run(exe: &str, ops: u64) -> Vec<u64> {
    let out = Command::new(exe)
        .args(["measure", &ops.to_string()])
        .output()
        .expect("run a build");
    assert!(out.status.success(), "{exe} failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.split_whitespace().nth(1).unwrap().parse().unwrap())
        .collect()
}

fn compare(rust: &str, port: &str, epochs: usize, ops: u64) {
    let mut pairs = vec![Vec::new(); WORKLOADS.len()];
    for _ in 0..epochs {
        let a1 = run(rust, ops);
        let b1 = run(port, ops);
        let b2 = run(port, ops);
        let a2 = run(rust, ops);
        for (w, pair) in pairs.iter_mut().enumerate() {
            pair.push(PairedSample::new(a1[w] + a2[w], b1[w] + b2[w], 2 * ops).unwrap());
        }
    }
    for (name, pairs) in WORKLOADS.iter().zip(&pairs) {
        let per_op = |t: u64| t as f64 / (2 * ops) as f64;
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let rust_ns = median(pairs.iter().map(|p| per_op(p.baseline_total())).collect());
        let port_ns = median(pairs.iter().map(|p| per_op(p.candidate_total())).collect());
        let ratios: Vec<f64> = pairs
            .iter()
            .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
            .collect();
        let ratio = median(ratios.clone());
        let (lo, hi) = median_interval(ratios);
        let analysis = analyze_pairs(pairs, 0).unwrap();
        let bp = |s: PairedSample| s.regression_bp_bounds().floor as f64 / 100.0;
        println!(
            "{name:<10} rust {rust_ns:>9.0} ns/op  port {port_ns:>9.0} ns/op  port/rust {ratio:.2}x \
             (95% median interval {lo:.2}x..{hi:.2}x, vesper-bench median regression \
             {:+.0}%..{:+.0}%, {epochs} epochs)",
            bp(analysis.median_lower),
            bp(analysis.median_upper),
        );
    }
}

/// The distribution-free 95% interval of the median ratio: the order
/// statistics `k` and `n + 1 - k` (1-based) for the largest `k` with
/// `P(Binomial(n, 1/2) < k) <= 0.025`. With fewer than 6 epochs it is the
/// whole range.
fn median_interval(mut ratios: Vec<f64>) -> (f64, f64) {
    ratios.sort_by(f64::total_cmp);
    let n = ratios.len();
    let mut k = 1;
    let mut below = 0.0;
    let mut choose = 1.0;
    // After step `j`, `below` is P(Binomial(n, 1/2) <= j).
    for j in 0..n {
        below += choose / 2f64.powi(n as i32);
        if below > 0.025 {
            break;
        }
        k = j + 1;
        choose = choose * (n - j) as f64 / (j + 1) as f64;
    }
    (ratios[k - 1], ratios[n - k])
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["measure", ops] => {
            let ops = ops.parse().unwrap();
            for name in WORKLOADS {
                println!("{name} {}", measure(name, ops));
            }
        }
        // One workload only (for profiling): `abba measure <ops> <workload>`.
        ["measure", ops, name] if WORKLOADS.contains(name) => {
            println!("{name} {}", measure(name, ops.parse().unwrap()));
        }
        ["compare", rust, port, epochs, ops] => {
            compare(rust, port, epochs.parse().unwrap(), ops.parse().unwrap())
        }
        _ => {
            eprintln!("usage: abba measure <ops> | abba compare <rust> <port> <epochs> <ops>");
            std::process::exit(2);
        }
    }
}

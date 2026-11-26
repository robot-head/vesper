//! The dyn lowering swap (front-end wiring slice 4, #1976) against Rust:
//! `dyn_fn::lower` and `dyn_trait::lower` answered by the linked Vesper
//! module (`modules/check_dyn`, what the hybrid runs) on the checker AST
//! crossing, against `rust_lower`, over every repository `.ves` source with
//! both passes forced on and over hand-written cases that reach the
//! diagnostics and every side table. The root package's
//! tests/self_host_check_dyn.rs checks the port itself against Rust on the
//! dyn suites and their mutants.
#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/check_dyn.rs"]
mod check_dyn_binding;

#[path = "../../../tests/common/ast_words.rs"]
mod ast_words;

#[allow(dead_code)]
#[path = "../src/parse_words.rs"]
mod parse_words;

#[path = "../../../tests/common/dyn_words.rs"]
mod dyn_words;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vesper::ast::{Diagnostic, Module};
use vesper::{dyn_fn, dyn_trait};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

/// Every `.ves` file of the repository, sorted.
fn repository_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if p.is_dir() {
                if !matches!(&*name, "target" | ".git" | "node_modules" | "bazel-out")
                    && !name.starts_with("bazel-")
                {
                    walk(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "ves") {
                out.push(p);
            }
        }
    }
    let mut out = vec![];
    walk(&repository(), &mut out);
    out.sort();
    out
}

/// Cases that lower function objects and `dyn` parameters (params,
/// forwards and boxed traits all non-empty) or fail with diagnostics.
const CASES: &[&str] = &[
    "trait Shape { fn area(in self) -> i64; }\nstruct Sq { s: i64 }\nimpl Shape for Sq { fn area(in self) -> i64 { self.s * self.s } }\nfn total(in a: dyn Shape, in b: dyn Shape) -> i64 { a.area() + b.area() }\nfn twice(in s: dyn Shape) -> i64 { total(s, s) }\nfn main() -> i64 { let q = Sq { s: 3 }; twice(q) }\n",
    "trait T { fn f(in self) -> i64; }\nfn g(in x: dyn T) -> i64 { x.f() }\nfn h(in y: dyn T) -> i64 { g(y) + y.f() }\nfn k(in b: Box[dyn T]) -> i64 { 0 }\n",
    "fn ap(in f: dyn Fn(i64) -> i64) -> i64 { f(1) }\nfn main() -> i64 { let k = 2; ap(|a| a + k) + ap(|b: i64| -> i64 b * 2) }\n",
    "fn ap(in f: dyn FnMut(i64) -> i64) -> i64 { f(1) }\nfn once(sink f: dyn FnOnce(i64) -> i64) -> i64 { f(2) }\nfn main() -> i64 { ap(|a| a) + once(|a| a) }\n",
    "trait T { type A; fn f(in self) -> i64; }\nfn g(in x: dyn T) -> i64 { x.f() }\n",
    "trait T { fn f(in self) -> i64; }\nfn g(in x: dyn T) -> i64 { let y = x; 0 }\n",
    "trait T { fn f(in self) -> i64; }\nstruct S { a: dyn T }\nfn g() -> dyn T { 0 }\n",
    "fn ap(in f: dyn Fn(i64) -> i64) -> i64 { f(1) }\nfn main() -> i64 { ap(|a, b| a) }\n",
];

/// The swap's answer, as `src/swap/check_dyn.rs` computes it.
fn vesper_fn(m: &Module) -> Result<dyn_fn::Lowered, Vec<Diagnostic>> {
    let base = dyn_fn::span_base(m);
    let bytes: Vec<u8> = ast_words::module_words(m)
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    dyn_words::decode_dyn_fn(
        m,
        base,
        &check_dyn_binding::dyn_fn_lower(&bytes, base as u64),
    )
}

fn vesper_trait(m: &Module) -> Result<dyn_trait::Lowered, Vec<Diagnostic>> {
    let bytes: Vec<u8> = ast_words::module_words(m)
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    dyn_words::decode_dyn_trait(m, &check_dyn_binding::dyn_trait_lower(&bytes))
}

#[derive(Default)]
struct Seen {
    fn_lowered: usize,
    trait_lowered: usize,
    errors: usize,
    params: usize,
    forwards: usize,
    boxed: usize,
}

/// Both passes on `source` as the checker chains them (the trait pass on
/// the function-object pass's module), each against Rust; the first
/// difference, if any.
fn compare(source: &str, seen: &mut Seen) -> Option<String> {
    let module = vesper::parser::parse(source).module;
    let next = match (dyn_fn::rust_lower(&module), vesper_fn(&module)) {
        (Ok(r), Ok(v)) => {
            if r.module != v.module {
                return Some("dyn_fn: lowered modules differ".into());
            }
            if (r.base, r.end, &r.origins) != (v.base, v.end, &v.origins) {
                return Some(format!(
                    "dyn_fn: spans differ: rust {} {} {:?}, vesper {} {} {:?}",
                    r.base, r.end, r.origins, v.base, v.end, v.origins
                ));
            }
            seen.fn_lowered += 1;
            r.module
        }
        (Err(r), Err(v)) => {
            if r != v {
                return Some(format!("dyn_fn: diagnostics differ: {r:?} vs {v:?}"));
            }
            seen.errors += 1;
            return None;
        }
        (r, v) => {
            return Some(format!(
                "dyn_fn: rust ok {}, vesper ok {}",
                r.is_ok(),
                v.is_ok()
            ));
        }
    };
    match (dyn_trait::rust_lower(&next), vesper_trait(&next)) {
        (Ok(r), Ok(v)) => {
            if r.module != v.module {
                return Some("dyn_trait: lowered modules differ".into());
            }
            if r.params != v.params {
                return Some(format!(
                    "dyn_trait: params {:?} vs {:?}",
                    r.params, v.params
                ));
            }
            if format!("{:?}", r.forwards) != format!("{:?}", v.forwards) {
                return Some(format!(
                    "dyn_trait: forwards {:?} vs {:?}",
                    r.forwards, v.forwards
                ));
            }
            if r.boxed != v.boxed {
                return Some(format!("dyn_trait: boxed {:?} vs {:?}", r.boxed, v.boxed));
            }
            seen.trait_lowered += 1;
            seen.params += r.params.len();
            seen.forwards += r.forwards.len();
            seen.boxed += r.boxed.len();
            None
        }
        (Err(r), Err(v)) => {
            seen.errors += 1;
            (r != v).then(|| format!("dyn_trait: diagnostics differ: {r:?} vs {v:?}"))
        }
        (r, v) => Some(format!(
            "dyn_trait: rust ok {}, vesper ok {}",
            r.is_ok(),
            v.is_ok()
        )),
    }
}

/// The hand-written cases reach every outcome and side table.
#[test]
fn dyn_swap_matches_rust_on_the_cases() {
    let mut seen = Seen::default();
    let failures: Vec<String> = CASES
        .iter()
        .enumerate()
        .filter_map(|(i, s)| compare(s, &mut seen).map(|d| format!("case {i}: {d}")))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(seen.errors >= 3, "{} failing cases", seen.errors);
    assert!(seen.params > 0 && seen.forwards > 0 && seen.boxed > 0);
}

/// Every repository source, both passes forced on.
#[test]
fn dyn_swap_matches_rust_on_the_repository_sources() {
    let files = repository_sources();
    assert!(files.len() >= 300, "{} sources", files.len());
    let mut seen = Seen::default();
    let mut failures = vec![];
    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        if let Some(d) = compare(&source, &mut seen) {
            failures.push(format!("{}: {d}", path.display()));
        }
    }
    eprintln!(
        "dyn swap: {} sources, {} lowered by dyn_fn, {} by dyn_trait, {} failed, \
         {} params, {} forwards, {} boxed",
        files.len(),
        seen.fn_lowered,
        seen.trait_lowered,
        seen.errors,
        seen.params,
        seen.forwards,
        seen.boxed
    );
    failures.truncate(8);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn time(f: impl FnOnce()) -> Duration {
    let t = Instant::now();
    f();
    t.elapsed()
}

/// Paired ABBA rounds; the median ratio and the 95% CI of the geometric
/// mean ratio (tests/common/check_harness.rs `abba_ci`).
fn abba_ci(
    label: &str,
    rounds: usize,
    rust: &mut dyn FnMut() -> Duration,
    vesper: &mut dyn FnMut() -> Duration,
) {
    rust();
    vesper();
    let mut ratios = vec![];
    for _ in 0..rounds {
        let r1 = rust();
        let v1 = vesper();
        let v2 = vesper();
        let r2 = rust();
        ratios.push((v1 + v2).as_secs_f64() / (r1 + r2).as_secs_f64());
    }
    let logs: Vec<f64> = ratios.iter().map(|r| r.ln()).collect();
    let n = logs.len() as f64;
    let mean = logs.iter().sum::<f64>() / n;
    let var = logs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1.0);
    let t = if rounds <= 10 {
        2.262
    } else if rounds <= 20 {
        2.093
    } else {
        2.045
    };
    let half = t * (var / n).sqrt();
    ratios.sort_by(f64::total_cmp);
    eprintln!(
        "ABBA-CI {label}: {rounds} rounds, median {:.3}x, 95% CI {:.3}x..{:.3}x",
        ratios[ratios.len() / 2],
        (mean - half).exp(),
        (mean + half).exp()
    );
}

/// The swap (encode, Vesper, decode) against `rust_lower`, both passes,
/// on the modules the checker sends them (the cases and the repository
/// sources that use function objects or `dyn`) and on every repository
/// source forced. Run with `cargo test --release -p vesper-hybrid --test
/// check_dyn -- --ignored --nocapture`.
#[test]
#[ignore]
fn dyn_swap_speed() {
    let mut all: Vec<Module> = CASES
        .iter()
        .map(|s| vesper::parser::parse(s).module)
        .collect();
    all.extend(
        repository_sources()
            .iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .map(|s| vesper::parser::parse(&s).module),
    );
    let used: Vec<Module> = all
        .iter()
        .filter(|m| dyn_fn::uses_fn_objects(m) || dyn_trait::uses_dyn(m))
        .cloned()
        .collect();
    eprintln!("{} modules, {} use the passes", all.len(), used.len());
    for (label, workload, set) in [
        ("modules that use the passes", "used", &used),
        ("every module, forced", "forced", &all),
    ] {
        let mut rust = || {
            time(|| {
                for m in set.iter() {
                    if let Ok(l) = dyn_fn::rust_lower(m) {
                        std::hint::black_box(dyn_trait::rust_lower(&l.module).is_ok());
                    }
                }
            })
        };
        let mut vesper = || {
            time(|| {
                for m in set.iter() {
                    if let Ok(l) = vesper_fn(m) {
                        std::hint::black_box(vesper_trait(&l.module).is_ok());
                    }
                }
            })
        };
        if std::env::var_os("FLIP_GATE").is_some() {
            // The flip gate (scripts/ci/flip-gate/dyn.json, #2207):
            // `FLIP_GATE_EPOCHS` (default 12) ABBA rounds after a warm-up.
            let epochs: usize = std::env::var("FLIP_GATE_EPOCHS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(12);
            rust();
            vesper();
            let mut pairs = String::new();
            for _ in 0..epochs {
                let (r1, v1, v2, r2) = (rust(), vesper(), vesper(), rust());
                pairs += &format!(" {}:{}", (r1 + r2).as_nanos(), (v1 + v2).as_nanos());
            }
            println!("flip-gate pairs {workload}{pairs}");
            continue;
        }
        abba_ci(label, 10, &mut rust, &mut vesper);
    }
}

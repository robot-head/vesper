//! Speed and memory of the front end v2 (`packages/vesper-front`, #1129)
//! against the Rust parser on every repository `.ves` file. Ignored by
//! default; run with
//!
//! ```text
//! cargo test --release --test self_host_front_v2_bench -- --ignored --nocapture
//! ```
//!
//! `abba_against_the_rust_parser` times the two parsers in ABBA order over
//! the whole corpus. `once_for_callgrind` parses the corpus once with each
//! parser inside `run_rust` and `run_vesper`, for
//! `valgrind --tool=callgrind` and `callgrind_annotate --inclusive=yes`.
//! `memory_per_node` counts the Rust parser's heap bytes with a counting
//! allocator and the Vesper tables' bytes from their row counts.
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use vesper::{check, native, packages};

struct Counting;
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);
static TOTAL: AtomicUsize = AtomicUsize::new(0);
// SAFETY: forwards to the system allocator and only counts sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live =
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed) + layout.size() as isize;
        PEAK.fetch_max(live, Ordering::Relaxed);
        TOTAL.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: same layout contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        // SAFETY: `ptr` came from `alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn corpus() -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                let name = name.to_string_lossy();
                if !matches!(&*name, "target" | ".git" | "node_modules")
                    && !name.starts_with("bazel-")
                {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "ves") {
                out.push(path);
            }
        }
    }
    let mut paths = vec![];
    walk(&root(), &mut paths);
    paths.sort();
    paths
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter(|s| s.len() <= 1 << 20)
        .collect()
}

struct Front {
    counts: native::WordFunction,
    lex: native::WordFunction,
    lex_raw: native::WordFunction,
    editor: native::WordFunction,
}

fn front() -> Front {
    let assembled = packages::load(&root().join("packages/vesper-front")).unwrap();
    assert!(assembled.ok(), "{:#?}", assembled.diagnostics);
    let checked = check::check(&assembled.module);
    assert!(checked.diagnostics.is_empty(), "{:#?}", checked.diagnostics);
    let compile = |entry| native::WordFunction::compile(&checked.world, entry, true).unwrap();
    Front {
        counts: compile("parse_counts"),
        lex: compile("lex_count"),
        lex_raw: compile("lex_raw_count"),
        editor: compile("parse_editor_count"),
    }
}

fn call(f: &native::WordFunction, source: &str) -> u64 {
    let words = [source.len() as u64, 0, source.as_ptr() as u64];
    // SAFETY: one `in Str` argument, passed as the address of its words.
    unsafe { f.call(&[words.as_ptr() as u64]) }.unwrap()
}

#[inline(never)]
fn run_rust(corpus: &[String]) -> usize {
    let mut nodes = 0;
    for s in corpus {
        let parse = vesper_syntax::parser::parse(s);
        nodes += parse.syntax().descendants().count();
    }
    nodes
}

#[inline(never)]
fn run_rust_parse_only(corpus: &[String]) -> usize {
    let mut n = 0;
    for s in corpus {
        let parse = vesper_syntax::parser::parse(s);
        n += parse.diagnostics.len();
        std::hint::black_box(&parse);
    }
    n
}

#[inline(never)]
fn run_vesper(f: &Front, corpus: &[String]) -> u64 {
    corpus
        .iter()
        .map(|s| call(&f.counts, s) & 0xffff_ffff)
        .sum()
}

#[inline(never)]
fn run_vesper_lex_raw(f: &Front, corpus: &[String]) -> u64 {
    corpus.iter().map(|s| call(&f.lex_raw, s)).sum()
}

#[inline(never)]
fn run_rust_lex(corpus: &[String]) -> usize {
    corpus
        .iter()
        .map(|s| vesper_syntax::lexer::lex(s).0.len())
        .sum()
}

#[inline(never)]
fn run_vesper_lex(f: &Front, corpus: &[String]) -> u64 {
    corpus.iter().map(|s| call(&f.lex, s)).sum()
}

fn time(mut f: impl FnMut()) -> Duration {
    let t = Instant::now();
    f();
    t.elapsed()
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

#[test]
#[ignore = "benchmark"]
fn abba_against_the_rust_parser() {
    let corpus = corpus();
    let f = front();
    let bytes: usize = corpus.iter().map(String::len).sum();
    let tokens = run_vesper_lex(&f, &corpus);
    let nodes = run_vesper(&f, &corpus);
    assert_eq!(nodes as usize, run_rust(&corpus), "node counts");
    assert_eq!(tokens as usize, run_rust_lex(&corpus), "token counts");
    let rounds = 7;
    let (mut ra, mut va, mut rl, mut vl, mut rp, mut vr) =
        (vec![], vec![], vec![], vec![], vec![], vec![]);
    for _ in 0..rounds {
        ra.push(time(|| {
            std::hint::black_box(run_rust_parse_only(&corpus));
        }));
        va.push(time(|| {
            std::hint::black_box(run_vesper(&f, &corpus));
        }));
        va.push(time(|| {
            std::hint::black_box(run_vesper(&f, &corpus));
        }));
        ra.push(time(|| {
            std::hint::black_box(run_rust_parse_only(&corpus));
        }));
        rl.push(time(|| {
            std::hint::black_box(run_rust_lex(&corpus));
        }));
        vl.push(time(|| {
            std::hint::black_box(run_vesper_lex(&f, &corpus));
        }));
        vl.push(time(|| {
            std::hint::black_box(run_vesper_lex(&f, &corpus));
        }));
        rl.push(time(|| {
            std::hint::black_box(run_rust_lex(&corpus));
        }));
        rp.push(time(|| {
            std::hint::black_box(run_rust(&corpus));
        }));
        vr.push(time(|| {
            std::hint::black_box(run_vesper_lex_raw(&f, &corpus));
        }));
    }
    let (ra, va, rl, vl, rp, vr) = (
        median(ra),
        median(va),
        median(rl),
        median(vl),
        median(rp),
        median(vr),
    );
    let per_s = |n: u64, d: Duration| n as f64 / d.as_secs_f64() / 1e6;
    println!(
        "corpus: {} files, {bytes} bytes, {tokens} tokens, {nodes} nodes",
        corpus.len()
    );
    println!(
        "parse  rust {ra:?} ({:.1} M tokens/s, {:.2} M nodes/s); vesper {va:?} ({:.1} M tokens/s, {:.2} M nodes/s); vesper/rust {:.2}x",
        per_s(tokens, ra),
        per_s(nodes, ra),
        per_s(tokens, va),
        per_s(nodes, va),
        va.as_secs_f64() / ra.as_secs_f64()
    );
    println!("rust parse + CST walk {rp:?}");
    println!(
        "lex    rust {rl:?} ({:.1} M tokens/s); vesper {vl:?} ({:.1} M tokens/s); vesper/rust {:.2}x",
        per_s(tokens, rl),
        per_s(tokens, vl),
        vl.as_secs_f64() / rl.as_secs_f64()
    );
    println!(
        "lex without identifier classification: vesper {vr:?} ({:.1} M tokens/s); vesper/rust {:.2}x",
        per_s(tokens, vr),
        vr.as_secs_f64() / rl.as_secs_f64()
    );
}

#[test]
#[ignore = "benchmark"]
fn once_for_callgrind() {
    let corpus = corpus();
    let f = front();
    let a = run_rust_parse_only(&corpus);
    let b = run_vesper(&f, &corpus);
    let c = run_rust_lex(&corpus);
    let d = run_vesper_lex(&f, &corpus);
    let e = run_vesper_lex_raw(&f, &corpus);
    println!("{a} {b} {c} {d} {e}");
}

#[test]
#[ignore = "benchmark"]
fn memory_per_node() {
    let corpus = corpus();
    let f = front();
    let (mut retained, mut green_only, mut peak, mut total) = (0isize, 0isize, 0isize, 0usize);
    let (mut nodes, mut tokens, mut bytes) = (0u64, 0u64, 0u64);
    for s in &corpus {
        let base = LIVE.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let t0 = TOTAL.load(Ordering::Relaxed);
        let parse = vesper_syntax::parser::parse(s);
        peak += PEAK.load(Ordering::Relaxed) - base;
        total += TOTAL.load(Ordering::Relaxed) - t0;
        retained += LIVE.load(Ordering::Relaxed) - base;
        let green = parse.green.clone();
        drop(parse);
        green_only += LIVE.load(Ordering::Relaxed) - base;
        drop(green);
        let c = call(&f.counts, s);
        nodes += c & 0xffff_ffff;
        tokens += c >> 32;
        bytes += s.len() as u64;
    }
    // Vesper tables (row counts times strides): the token table (kind 1,
    // start 4, end 4, word 1, symbol 4 bytes), the CST (node kind 1,
    // parent 4, start 4, end 4; leaf token 4, parent 4) and the parser's
    // working rows (significant index and rank 4 + 4 per token, events 4
    // per leaf and 8 per node, source copy 1 per byte).
    let leaves = tokens - corpus.len() as u64;
    let token_table = tokens * 14;
    let tree = nodes * 13 + leaves * 8;
    let working = tokens * 8 + (leaves + 2 * nodes) * 4 + bytes;
    let per = |b: f64| b / nodes as f64;
    println!(
        "corpus: {} files, {bytes} bytes, {tokens} tokens, {nodes} nodes",
        corpus.len()
    );
    println!(
        "rust:   retained {:.1} B/node (green tree alone {:.1}), peak {:.1} B/node, allocated {:.1} B/node",
        per(retained as f64),
        per(green_only as f64),
        per(peak as f64),
        per(total as f64)
    );
    println!(
        "vesper: tables {:.1} B/node (tokens {:.1}, CST {:.1}), working rows {:.1} B/node",
        per((token_table + tree) as f64),
        per(token_table as f64),
        per(tree as f64),
        per(working as f64)
    );
}

#[test]
#[ignore = "benchmark"]
fn lex_abba_against_logos() {
    let corpus = corpus();
    let f = front();
    let tokens = run_vesper_lex(&f, &corpus);
    if std::env::var_os("VESPER_FRONT_BENCH_EXPERIMENT").is_none() {
        assert_eq!(tokens as usize, run_rust_lex(&corpus), "token counts");
    }
    let rounds = 15;
    let (mut rl, mut vl, mut vr) = (vec![], vec![], vec![]);
    for _ in 0..rounds {
        rl.push(time(|| {
            std::hint::black_box(run_rust_lex(&corpus));
        }));
        vl.push(time(|| {
            std::hint::black_box(run_vesper_lex(&f, &corpus));
        }));
        vr.push(time(|| {
            std::hint::black_box(run_vesper_lex_raw(&f, &corpus));
        }));
        vl.push(time(|| {
            std::hint::black_box(run_vesper_lex(&f, &corpus));
        }));
        rl.push(time(|| {
            std::hint::black_box(run_rust_lex(&corpus));
        }));
    }
    let (rl, vl, vr) = (median(rl), median(vl), median(vr));
    println!(
        "lex rust {rl:?}; vesper {vl:?} ({:.2}x); raw {vr:?} ({:.2}x); {tokens} tokens",
        vl.as_secs_f64() / rl.as_secs_f64(),
        vr.as_secs_f64() / rl.as_secs_f64()
    );
}

#[test]
#[ignore = "benchmark"]
fn lex_once_for_callgrind() {
    let corpus = corpus();
    let f = front();
    let c = run_rust_lex(&corpus);
    let d = run_vesper_lex(&f, &corpus);
    let e = run_vesper_lex_raw(&f, &corpus);
    println!("{c} {d} {e}");
}

#[inline(never)]
fn run_rust_editor(corpus: &[String]) -> usize {
    corpus
        .iter()
        .map(|s| {
            vesper_syntax::parser::parse_recovery(s, vesper_syntax::recovery::Limits::default())
                .map_or(0, |r| r.declarations().len())
        })
        .sum()
}

#[inline(never)]
fn run_vesper_editor(f: &Front, corpus: &[String]) -> u64 {
    corpus.iter().map(|s| call(&f.editor, s)).sum()
}

/// Editor mode (`parse_recovery`, default limits) in ABBA order over the
/// corpus files within its 128 KiB source budget.
#[test]
#[ignore = "benchmark"]
fn editor_abba_against_parse_recovery() {
    let corpus: Vec<String> = corpus()
        .into_iter()
        .filter(|s| s.len() <= 128 * 1024)
        .collect();
    let f = front();
    let rounds = 9;
    let (mut r, mut v) = (vec![], vec![]);
    for _ in 0..rounds {
        r.push(time(|| {
            std::hint::black_box(run_rust_editor(&corpus));
        }));
        v.push(time(|| {
            std::hint::black_box(run_vesper_editor(&f, &corpus));
        }));
        v.push(time(|| {
            std::hint::black_box(run_vesper_editor(&f, &corpus));
        }));
        r.push(time(|| {
            std::hint::black_box(run_rust_editor(&corpus));
        }));
    }
    let (r, v) = (median(r), median(v));
    println!(
        "editor rust {r:?}; vesper {v:?} ({:.2}x); {} files",
        v.as_secs_f64() / r.as_secs_f64(),
        corpus.len()
    );
}

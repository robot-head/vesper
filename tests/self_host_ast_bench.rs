//! Speed and memory of parsing to an AST with `packages/vesper-ast`
//! (#1152: the front end v2 plus its lowering) against the Rust parser
//! (`vesper_syntax::parser::parse`, which builds `vesper_core::ast`) on every
//! repository `.ves` file. Ignored by default; run with
//!
//! ```text
//! cargo test --release --test self_host_ast_bench -- --ignored --nocapture
//! ```
//!
//! `abba_against_the_rust_parser` times both in ABBA order over the corpus,
//! and the front end v2 alone (`parse_counts`) to split parse from lowering.
//! `once_for_callgrind` parses the corpus once with each inside `run_rust`,
//! `run_vesper` and `run_vesper_front`, for `valgrind --tool=callgrind` and
//! `callgrind_annotate --inclusive=yes`. `memory_per_node` counts the heap
//! bytes the Rust `Module` keeps with a counting allocator, and the Vesper
//! AST's bytes as its row counts times each element's native stride
//! (`Layout::of`), since Vesper sequences allocate with `malloc` directly.
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use vesper::ast::Type;
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

struct Bench {
    world: check::World,
    count: native::WordFunction,
    rows: native::WordFunction,
    front: native::WordFunction,
}

fn bench() -> Bench {
    let load = |dir: &str| {
        let assembled = packages::load(&root().join(dir)).unwrap();
        assert!(assembled.ok());
        let checked = check::check(&assembled.module);
        assert!(checked.diagnostics.is_empty());
        checked
    };
    let ast = load("packages/vesper-ast");
    let front = load("packages/vesper-front");
    let compile =
        |c: &check::Checked, entry| native::WordFunction::compile(&c.world, entry, true).unwrap();
    Bench {
        count: compile(&ast, "ast_count"),
        rows: compile(&ast, "ast_rows"),
        front: compile(&front, "parse_counts"),
        world: ast.world,
    }
}

fn call(f: &native::WordFunction, source: &str) -> u64 {
    let words = [source.len() as u64, 0, source.as_ptr() as u64];
    // SAFETY: one `in Str` argument, passed as the address of its words.
    unsafe { f.call(&[words.as_ptr() as u64]) }.unwrap()
}

/// Reads and frees a returned `ChunkSeq[u64]` (header `{len, cap, data}`).
fn take_words(handle: u64) -> Vec<u64> {
    // SAFETY: `handle` is a live sequence header the entry returned, whose
    // `data` holds `len` packed u64 elements.
    let words = unsafe {
        let header = handle as *const u64;
        let len = *header as usize;
        let data = *header.add(2) as *const u64;
        if len == 0 {
            vec![]
        } else {
            std::slice::from_raw_parts(data, len).to_vec()
        }
    };
    vesper::seq::native_runtime::vesper_seq_free(handle);
    words
}

#[inline(never)]
fn run_rust(corpus: &[String]) -> usize {
    let mut n = 0;
    for s in corpus {
        let parse = vesper_syntax::parser::parse(s);
        n += parse.module.items.len();
        std::hint::black_box(&parse);
    }
    n
}

#[inline(never)]
fn run_vesper(b: &Bench, corpus: &[String]) -> u64 {
    corpus.iter().map(|s| call(&b.count, s)).sum()
}

#[inline(never)]
fn run_vesper_front(b: &Bench, corpus: &[String]) -> u64 {
    corpus.iter().map(|s| call(&b.front, s) & 0xffff_ffff).sum()
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
    let b = bench();
    let bytes: usize = corpus.iter().map(String::len).sum();
    let nodes = run_vesper(&b, &corpus);
    let (mut ra, mut va, mut vf) = (vec![], vec![], vec![]);
    for _ in 0..7 {
        ra.push(time(|| {
            std::hint::black_box(run_rust(&corpus));
        }));
        va.push(time(|| {
            std::hint::black_box(run_vesper(&b, &corpus));
        }));
        va.push(time(|| {
            std::hint::black_box(run_vesper(&b, &corpus));
        }));
        ra.push(time(|| {
            std::hint::black_box(run_rust(&corpus));
        }));
        vf.push(time(|| {
            std::hint::black_box(run_vesper_front(&b, &corpus));
        }));
    }
    let (ra, va, vf) = (median(ra), median(va), median(vf));
    let mps = |d: Duration| nodes as f64 / d.as_secs_f64() / 1e6;
    println!(
        "corpus: {} files, {bytes} bytes, {nodes} AST nodes (expressions, patterns, types)",
        corpus.len()
    );
    println!(
        "parse to AST: rust {ra:?} ({:.2} M nodes/s); vesper {va:?} ({:.2} M nodes/s); vesper/rust {:.2}x",
        mps(ra),
        mps(va),
        va.as_secs_f64() / ra.as_secs_f64()
    );
    println!(
        "vesper front end v2 alone {vf:?} ({:.2}x rust); lowering {:?} ({:.0}% of the vesper time)",
        vf.as_secs_f64() / ra.as_secs_f64(),
        va.saturating_sub(vf),
        100.0 * va.saturating_sub(vf).as_secs_f64() / va.as_secs_f64()
    );
}

#[test]
#[ignore = "benchmark"]
fn once_for_callgrind() {
    let corpus = corpus();
    let b = bench();
    let r = run_rust(&corpus);
    let v = run_vesper(&b, &corpus);
    let f = run_vesper_front(&b, &corpus);
    println!("{r} {v} {f}");
}

/// The native element stride of the `@vesper/ast` struct named `name`.
fn stride(world: &check::World, name: &str) -> u64 {
    let key = world
        .adts
        .keys()
        .find(|k| *k == name || k.ends_with(&format!("::{name}")))
        .unwrap_or_else(|| panic!("no type {name}"));
    let ty = Type::Named(key.clone(), vec![]);
    u64::from(vesper::layout::Layout::of(world, &ty).unwrap().size)
}

#[test]
#[ignore = "benchmark"]
fn memory_per_node() {
    // `ast_rows` order: 29 arenas, then the pool, five module lists, the
    // interned symbol count and the interned text's bytes.
    const ARENAS: [&str; 29] = [
        "Lit",
        "ExprNode",
        "PatNode",
        "TypeNode",
        "StmtNode",
        "ArgNode",
        "ClauseNode",
        "ParamNode",
        "EffectsNode",
        "EffectNode",
        "GenericNode",
        "FieldNode",
        "VariantNode",
        "AdtNode",
        "FnNode",
        "EffectDeclNode",
        "ImplNode",
        "TraitNode",
        "AssocDeclNode",
        "TraitRefNode",
        "WhereNode",
        "AssocBindNode",
        "ConfNode",
        "ItemNode",
        "ModelUseNode",
        "BoundNode",
        "UseNode",
        "UseNameNode",
        "ListRef",
    ];
    let corpus = corpus();
    let b = bench();
    let strides: Vec<u64> = ARENAS.iter().map(|n| stride(&b.world, n)).collect();
    for (n, s) in ARENAS.iter().zip(&strides) {
        print!("{n}={s} ");
    }
    println!();
    let (mut rust_module, mut rust_peak, mut rust_total) = (0isize, 0isize, 0usize);
    let (mut nodes, mut arenas, mut lists, mut names) = (0u64, 0u64, 0u64, 0u64);
    for s in &corpus {
        let base = LIVE.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let t0 = TOTAL.load(Ordering::Relaxed);
        let parse = vesper_syntax::parser::parse(s);
        rust_peak += PEAK.load(Ordering::Relaxed) - base;
        rust_total += TOTAL.load(Ordering::Relaxed) - t0;
        let vesper_syntax::parser::Parse {
            module,
            green,
            diagnostics,
        } = parse;
        drop((green, diagnostics));
        rust_module += LIVE.load(Ordering::Relaxed) - base;
        drop(module);
        let rows = take_words(call(&b.rows, s));
        assert_eq!(rows.len(), 37);
        nodes += rows[1] + rows[2] + rows[3];
        arenas += rows[..29]
            .iter()
            .zip(&strides)
            .map(|(r, s)| r * s)
            .sum::<u64>();
        // The pool and module lists are `ChunkSeq[u32]` (stride 4).
        lists += rows[29..35].iter().sum::<u64>() * 4;
        // The interner: text, a start and a hash word per symbol, and a
        // slot table of at least twice the symbol count (u32 slots).
        let n = rows[35];
        names += rows[36] + n * 16 + n.next_power_of_two() * 2 * 4;
    }
    let per = |b: f64| b / nodes as f64;
    println!(
        "corpus: {} files, {nodes} AST nodes (expressions, patterns, types)",
        corpus.len()
    );
    println!(
        "rust:   Module retained {:.1} B/node; parse peak {:.1} B/node, allocated {:.1} B/node (CST included)",
        per(rust_module as f64),
        per(rust_peak as f64),
        per(rust_total as f64)
    );
    println!(
        "vesper: AST {:.1} B/node (arenas {:.1}, pool and lists {:.1}, interner {:.1}), row payload before capacity slack",
        per((arenas + lists + names) as f64),
        per(arenas as f64),
        per(lists as f64),
        per(names as f64)
    );
}

//! `@vesper-std/graph` (self-hosting SH0 lane 5, #1127): differential tests
//! against petgraph on random graphs.
//!
//! A driver package builds the same pseudo-random directed graphs (an LCG
//! picks the edges, so parallel edges and self-loops occur) and folds, for
//! each one: the strongly connected components as each node's smallest
//! component-mate, whether the component numbering is a reverse topological
//! order of the condensation, whether a topological order exists, the Kahn
//! order itself and checks of it (every edge forward, or the reported cycle
//! is a real cycle), the immediate dominators from node 0, and the nodes
//! node 0 reaches. The Rust model computes the same fold with petgraph
//! (`tarjan_scc`, `toposort`, `dominators::simple_fast`, `Dfs`,
//! `has_path_connecting`). The driver runs in the JIT with and without
//! `optimize`, as an executable and (on small graphs) in the evaluator.
use petgraph::{
    algo::{dominators::simple_fast, has_path_connecting, tarjan_scc, toposort},
    graph::{DiGraph, NodeIndex},
    visit::Dfs,
};
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, packages, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-std-graph-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn driver(source: &str) -> Temp {
    let temp = Temp::new();
    let std = fs::canonicalize("packages/vesper-std/graph").unwrap();
    let manifest = format!(
        "[package]\nname = \"@local/graph-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = [\"Fail\", \"Alloc\"]\n\n[dependencies]\n\"@vesper-std/graph\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
        std.display().to_string().replace('\\', "/")
    );
    fs::write(temp.0.join("Loom.toml"), manifest).unwrap();
    fs::create_dir_all(temp.0.join("src")).unwrap();
    fs::write(temp.0.join("src/main.ves"), source).unwrap();
    temp
}

fn load(dir: &Path) -> check::Checked {
    let assembled = packages::load(dir).unwrap();
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    let checked = check::check(&assembled.module);
    let shown: Vec<_> = checked
        .diagnostics
        .iter()
        .map(|d| assembled.sources.localize(d))
        .collect();
    assert!(checked.diagnostics.is_empty(), "{shown:?}");
    checked
}

fn executable_exit(dir: &Path) -> Option<i32> {
    let exe = dir.join("driver");
    let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(dir)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "vesper build failed\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    Command::new(&exe).output().unwrap().status.code()
}

// ---------------------------------------------------------------------------
// The Rust model.

const NONE: u64 = u32::MAX as u64;

fn mix(acc: u64, v: u64) -> u64 {
    acc.wrapping_mul(1099511628211).wrapping_add(v)
}

fn lcg(x: u64) -> u64 {
    x.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}

/// The edges of case `(n, m, seed)`, in insertion order.
fn edges(n: u64, m: u64, seed: u64) -> Vec<(u32, u32)> {
    let mut x = seed;
    let mut out = Vec::new();
    if n == 0 {
        return out;
    }
    for _ in 0..m {
        x = lcg(x);
        let a = (x >> 33) % n;
        x = lcg(x);
        let b = (x >> 33) % n;
        out.push((a as u32, b as u32));
    }
    out
}

fn model_case(acc0: u64, n: u64, m: u64, seed: u64) -> u64 {
    let es = edges(n, m, seed);
    let mut g = DiGraph::<(), ()>::new();
    for _ in 0..n {
        g.add_node(());
    }
    for &(a, b) in &es {
        g.add_edge(NodeIndex::new(a as usize), NodeIndex::new(b as usize), ());
    }
    let mut acc = mix(acc0, n);
    // SCC: each node's smallest component-mate, and the count.
    let comps = tarjan_scc(&g);
    let mut rep = vec![0u64; n as usize];
    for c in &comps {
        let min = c.iter().map(|v| v.index()).min().unwrap() as u64;
        for v in c {
            rep[v.index()] = min;
        }
    }
    acc = mix(acc, comps.len() as u64);
    for r in &rep {
        acc = mix(acc, *r);
    }
    // The driver checks its numbering is reverse topological: 0 violations.
    acc = mix(acc, 0);
    // Topological order: Kahn, FIFO, seeded in node order.
    let dag = toposort(&g, None).is_ok();
    acc = mix(acc, u64::from(dag));
    let mut indeg = vec![0u64; n as usize];
    let mut succ = vec![Vec::new(); n as usize];
    for &(a, b) in &es {
        indeg[b as usize] += 1;
        succ[a as usize].push(b);
    }
    let mut queue: VecDeque<u32> = (0..n as u32).filter(|&v| indeg[v as usize] == 0).collect();
    let mut order = Vec::new();
    while let Some(u) = queue.pop_front() {
        order.push(u);
        for &w in &succ[u as usize] {
            indeg[w as usize] -= 1;
            if indeg[w as usize] == 0 {
                queue.push_back(w);
            }
        }
    }
    acc = mix(acc, order.len() as u64);
    for v in &order {
        acc = mix(acc, u64::from(*v));
    }
    // The driver checks the order (DAG) or the cycle (otherwise): valid = 1.
    acc = mix(acc, 1);
    if n > 0 {
        let root = NodeIndex::new(0);
        let dom = simple_fast(&g, root);
        for v in 0..n as usize {
            let i = if v == 0 {
                0
            } else {
                dom.immediate_dominator(NodeIndex::new(v))
                    .map_or(NONE, |d| d.index() as u64)
            };
            acc = mix(acc, i);
        }
        let mut seen = vec![false; n as usize];
        let mut dfs = Dfs::new(&g, root);
        while let Some(v) = dfs.next(&g) {
            seen[v.index()] = true;
        }
        for s in seen {
            acc = mix(acc, u64::from(s));
        }
        // `reaches` on a few pairs.
        let mut x = seed ^ 0x5555;
        for _ in 0..8 {
            x = lcg(x);
            let a = (x >> 33) % n;
            x = lcg(x);
            let b = (x >> 33) % n;
            let r = has_path_connecting(&g, NodeIndex::new(a as usize), NodeIndex::new(b as usize), None);
            acc = mix(acc, u64::from(r));
        }
    }
    acc
}

/// Cases `(n, m, seed)`: sparse to dense, DAG-heavy (m < n) to cyclic.
fn cases(big: bool) -> Vec<(u64, u64, u64)> {
    let sizes: &[u64] = if big {
        &[0, 1, 2, 3, 5, 8, 13, 50, 200, 1000, 5000]
    } else {
        &[0, 1, 2, 3, 5, 8, 13, 30]
    };
    let mut out = Vec::new();
    let mut seed = 1u64;
    for &n in sizes {
        for num in [1u64, 2, 4, 8, 16] {
            // m = n * num / 4: a quarter edge per node up to four per node.
            let m = n * num / 4;
            for _ in 0..3 {
                seed = lcg(seed);
                out.push((n, m, seed >> 1));
            }
        }
    }
    out
}

fn model(cases: &[(u64, u64, u64)]) -> u64 {
    cases
        .iter()
        .fold(14695981039346656037, |acc, &(n, m, s)| model_case(acc, n, m, s))
}

// ---------------------------------------------------------------------------
// The Vesper driver.

const DRIVER: &str = "use @vesper-std/graph::{graph_builder, none, Graph};
use @vesper-std/graph::scc::scc;
use @vesper-std/graph::topo::toposort;
use @vesper-std/graph::dom::dominators;
use @vesper-std/graph::reach::{reachable_from, reaches};

fn mix(acc: u64, v: u64) -> u64 {
    wrapping_add(wrapping_mul(acc, 1099511628211), v)
}

fn lcg(x: u64) -> u64 {
    wrapping_add(wrapping_mul(x, 6364136223846793005), 1442695040888963407)
}

fn rem(x: u64, n: u64) -> u64 {
    if n == 0 { 0 } else { x % n }
}

fn bit(b: bool) -> u64 {
    if b { 1 } else { 0 }
}

fn build(n: u64, m: u64, seed: u64) -> Graph {
    let mut b = graph_builder(wrapping_u32(n));
    let mut x = seed;
    if n > 0 {
        let mut i: u64 = 0;
        loop {
            if i >= m {
                break;
            }
            x = lcg(x);
            let a = rem(x >> 33, n);
            x = lcg(x);
            let c = rem(x >> 33, n);
            b.add_edge(wrapping_u32(a), wrapping_u32(c));
            i = wrapping_add(i, 1);
        }
    }
    b.build()
}

fn one_case(acc0: u64, n: u64, m: u64, seed: u64) -> u64 {
    let g = build(n, m, seed);
    let mut acc = mix(acc0, n);
    // SCC: count, each node's smallest component-mate.
    let s = scc(in g);
    acc = mix(acc, s.count());
    let mut v: u64 = 0;
    loop {
        if v >= n {
            break;
        }
        let c = s.component(wrapping_u32(v));
        let size = s.size(c);
        let mut least: u32 = none();
        let mut i: u64 = 0;
        loop {
            if i >= size {
                break;
            }
            let w = s.member(c, i);
            if w < least {
                least = w;
            }
            i = wrapping_add(i, 1);
        }
        acc = mix(acc, widen_u64(least));
        v = wrapping_add(v, 1);
    }
    // Every edge goes from a higher component number to a lower or equal one.
    let mut bad: u64 = 0;
    v = 0;
    loop {
        if v >= n {
            break;
        }
        let a = wrapping_u32(v);
        let (lo, hi) = g.succ_range(a);
        let mut e = lo;
        loop {
            if e >= hi {
                break;
            }
            if s.component(a) < s.component(g.succ_edge(e)) {
                bad = wrapping_add(bad, 1);
            }
            e = wrapping_add(e, 1);
        }
        v = wrapping_add(v, 1);
    }
    acc = mix(acc, bad);
    // Topological order.
    let t = toposort(in g);
    acc = mix(acc, bit(t.is_dag()));
    acc = mix(acc, t.order_len());
    let mut pos: ChunkSeq[u64] = ChunkSeq::with_capacity[u64](n);
    pos.resize(n, 18446744073709551615);
    let mut i: u64 = 0;
    loop {
        if i >= t.order_len() {
            break;
        }
        let w = t.order_at(i);
        acc = mix(acc, widen_u64(w));
        pos.try_set(widen_u64(w), i);
        i = wrapping_add(i, 1);
    }
    let mut valid = true;
    if t.is_dag() {
        // Every edge goes forward in the order.
        v = 0;
        loop {
            if v >= n {
                break;
            }
            let a = wrapping_u32(v);
            let (lo, hi) = g.succ_range(a);
            let mut e = lo;
            loop {
                if e >= hi {
                    break;
                }
                let pa = match pos.try_get(v) {
                    Option::Some(p) => p,
                    Option::None => 0,
                };
                let pb = match pos.try_get(widen_u64(g.succ_edge(e))) {
                    Option::Some(p) => p,
                    Option::None => 0,
                };
                if pa >= pb {
                    valid = false;
                }
                e = wrapping_add(e, 1);
            }
            v = wrapping_add(v, 1);
        }
        if t.order_len() != n {
            valid = false;
        }
    } else {
        // The cycle is a real cycle.
        let k = t.cycle_len();
        if k == 0 {
            valid = false;
        }
        let mut j: u64 = 0;
        loop {
            if j >= k {
                break;
            }
            let a = t.cycle_at(j);
            let b = t.cycle_at(rem(wrapping_add(j, 1), k));
            if !g.has_edge(a, b) {
                valid = false;
            }
            j = wrapping_add(j, 1);
        }
    }
    acc = mix(acc, bit(valid));
    if n > 0 {
        let d = dominators(in g, 0);
        v = 0;
        loop {
            if v >= n {
                break;
            }
            acc = mix(acc, widen_u64(d.idom(wrapping_u32(v))));
            v = wrapping_add(v, 1);
        }
        let seen = reachable_from(in g, 0);
        v = 0;
        loop {
            if v >= n {
                break;
            }
            let r = match seen.try_get(v) {
                Option::Some(x) => x,
                Option::None => false,
            };
            acc = mix(acc, bit(r));
            v = wrapping_add(v, 1);
        }
        let mut x = seed ^ 21845;
        let mut q: u64 = 0;
        loop {
            if q >= 8 {
                break;
            }
            x = lcg(x);
            let a = rem(x >> 33, n);
            x = lcg(x);
            let b = rem(x >> 33, n);
            acc = mix(acc, bit(reaches(in g, wrapping_u32(a), wrapping_u32(b))));
            q = wrapping_add(q, 1);
        }
    }
    acc
}

// A 100,000-node cycle: one component, no topological order, a cycle of
// every node, a chain of dominators. Checks that nothing recurses.
pub fn deep() -> u64 {
    let n: u64 = 100000;
    let mut b = graph_builder(wrapping_u32(n));
    let mut i: u64 = 0;
    loop {
        if i >= n {
            break;
        }
        b.add_edge(wrapping_u32(i), wrapping_u32(rem(wrapping_add(i, 1), n)));
        i = wrapping_add(i, 1);
    }
    let g = b.build();
    let s = scc(in g);
    let t = toposort(in g);
    let d = dominators(in g, 0);
    let mut acc = mix(s.count(), t.cycle_len());
    acc = mix(acc, widen_u64(d.idom(99999)));
    acc = mix(acc, bit(d.dominates(5, 99999)));
    acc
}

pub fn random() -> u64 {
    let cases: ChunkSeq[u64] = CASES;
    let mut acc: u64 = 14695981039346656037;
    let mut k: u64 = 0;
    loop {
        if wrapping_add(k, 2) >= cases.len() {
            break acc;
        }
        let n = match cases.try_get(k) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        let m = match cases.try_get(wrapping_add(k, 1)) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        let s = match cases.try_get(wrapping_add(k, 2)) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        acc = one_case(acc, n, m, s);
        k = wrapping_add(k, 3);
    }
}

fn main() -> u64 {
    random() ^ deep()
}
";

fn cases_literal(cases: &[(u64, u64, u64)]) -> String {
    let mut s = String::from("{\n        let mut z: ChunkSeq[u64] = ChunkSeq::new[u64]();\n");
    for (n, m, seed) in cases {
        s.push_str(&format!(
            "        z.push({n});\n        z.push({m});\n        z.push({seed});\n"
        ));
    }
    s.push_str("        z\n    }");
    s
}

fn model_deep() -> u64 {
    // One component, a cycle through all 100,000 nodes, idom(99999) =
    // 99998, and 5 dominates 99999 (every path from 0 passes it).
    let acc = mix(1, 100000);
    let acc = mix(acc, 99998);
    mix(acc, 1)
}

#[test]
fn graph_algorithms_match_petgraph_natively() {
    let cases = cases(true);
    let dir = driver(&DRIVER.replace("CASES", &cases_literal(&cases)));
    let c = load(&dir.0);
    let want = model(&cases);
    for optimize in [true, false] {
        for (entry, value) in [("random", want), ("deep", model_deep())] {
            let got = native::jit_with_options(&c.world, entry, optimize)
                .unwrap_or_else(|e| panic!("JIT {entry} (optimize {optimize}) failed: {e}"))
                .to_string();
            assert_eq!(got, value.to_string(), "JIT {entry} (optimize {optimize})");
        }
    }
    if links_executables() {
        assert_eq!(
            executable_exit(&dir.0),
            Some(((want ^ model_deep()) & 0xff) as i32),
            "executable"
        );
    }
}

#[test]
fn graph_algorithms_match_petgraph_in_the_evaluator() {
    let cases = cases(false);
    let dir = driver(&DRIVER.replace("CASES", &cases_literal(&cases)));
    let c = load(&dir.0);
    let mut runtime = Runtime::new(&c.world);
    runtime.optimize = false;
    runtime.fuel = 4_000_000_000;
    let got = runtime.run("random", vec![]).unwrap().to_string();
    assert_eq!(got, model(&cases).to_string());
}

#[test]
fn graph_package_checks_on_its_own() {
    load(Path::new("packages/vesper-std/graph"));
}

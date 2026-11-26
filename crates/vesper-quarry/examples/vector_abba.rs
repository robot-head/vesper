//! Differential digest and ABBA benchmark of the vectorized executor
//! (`execute_vectorized`, `aggregate_vectorized`, `execute_tiled` and
//! `vectorized_join_orders`, in Vesper since #1999). One binary holds one
//! build, so the arms are two builds of this example (the Rust arm from a
//! revision before the port, with this file copied in):
//!
//! ```sh
//! vector_abba digest > a.txt     # every result, work counter and error
//! vector_abba driver RUST_BIN PORT_BIN WORKLOAD
//! ```
//!
//! Workloads: `scan` (filter and projection over 8,000 rows), `agg`
//! (aggregates over the same scan), `join` (a hash join of 3,000 by 3,000
//! rows with a residual), `chain` (a reordered three-way inner join),
//! `tiles` (the scan into reused AoSoA tiles). `window W N` prints the
//! nanoseconds of N passes after one warm-up pass and a digest of the
//! results; `driver A B W` runs epochs of windows in the order A B B A,
//! checks that both arms computed the same results, pairs each epoch's
//! totals and reports the median B/A with its 95% interval.
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::process::Command;
use std::time::Instant;
use vesper_quarry::vector::{aggregate_rows, Aggregate, TileSet, VectorEngine};
use vesper_quarry::{DataSet, HermeticCatalog, Limits, ParamDef, PreparedQuery, ScalarType, Value};

const EPOCHS: usize = 15;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn maybe(&mut self, v: Value) -> Value {
        if self.below(5) == 0 {
            Value::Null
        } else {
            v
        }
    }
}

fn catalog() -> HermeticCatalog {
    let mut c = HermeticCatalog::default();
    c.apply_ddl(
        "CREATE TABLE orders (id I64 NOT NULL, org U64 NOT NULL, cents I64, \
         rate F64, paid BOOL, note TEXT, tag BYTES); \
         CREATE TABLE l (id I64 NOT NULL, k U64 NULL, f F64 NULL, s TEXT NULL); \
         CREATE TABLE r (id I64 NOT NULL, k U64 NULL, f F64 NULL, b BOOL NULL, s TEXT NULL); \
         CREATE TABLE m (k U64 NOT NULL, tag BYTES NULL)",
        Limits::default(),
    )
    .unwrap();
    c
}

/// `orders` rows, and `l`/`r`/`m` rows with `k` below `keys`.
fn data(c: &HermeticCatalog, seed: u64, no: usize, nl: usize, nr: usize, keys: u64) -> DataSet {
    let mut g = Rng(seed);
    let floats = [0.0, -0.0, 1.5, -2.25, 1e300, -1e-300, 3.0];
    let texts = ["", "a", "ab", "b", "Béa", "zz"];
    let orders: Vec<Vec<Value>> = (0..no)
        .map(|i| {
            let cents = Value::I64(g.below(2001) as i64 - 1000);
            let rate = Value::F64(floats[g.below(7) as usize]);
            let paid = Value::Bool(g.below(2) == 0);
            let note = Value::Text(texts[g.below(6) as usize].into());
            let tag = Value::Bytes(vec![g.below(3) as u8; g.below(3) as usize]);
            vec![
                Value::I64(i as i64),
                Value::U64(g.below(4)),
                g.maybe(cents),
                g.maybe(rate),
                g.maybe(paid),
                g.maybe(note),
                g.maybe(tag),
            ]
        })
        .collect();
    let l: Vec<Vec<Value>> = (0..nl)
        .map(|i| {
            let k = Value::U64(g.below(keys));
            let f = Value::F64(floats[g.below(4) as usize]);
            let s = Value::Text(texts[g.below(4) as usize].into());
            vec![Value::I64(i as i64), g.maybe(k), g.maybe(f), g.maybe(s)]
        })
        .collect();
    let r: Vec<Vec<Value>> = (0..nr)
        .map(|i| {
            let k = Value::U64(g.below(keys));
            let f = Value::F64(floats[g.below(4) as usize]);
            let b = Value::Bool(g.below(2) == 0);
            let s = Value::Text(texts[g.below(4) as usize].into());
            vec![
                Value::I64(100 + i as i64),
                g.maybe(k),
                g.maybe(f),
                g.maybe(b),
                g.maybe(s),
            ]
        })
        .collect();
    let m: Vec<Vec<Value>> = (0..keys.min(64))
        .map(|k| {
            let tag = Value::Bytes(vec![k as u8; (k % 3) as usize]);
            vec![Value::U64(k), g.maybe(tag)]
        })
        .collect();
    let mut d = DataSet::new(c);
    d.set_rows("orders", &orders, Limits::default()).unwrap();
    d.set_rows("l", &l, Limits::default()).unwrap();
    d.set_rows("r", &r, Limits::default()).unwrap();
    d.set_rows("m", &m, Limits::default()).unwrap();
    d
}

fn defs() -> Vec<ParamDef> {
    let p = |name: &str, ty, nullable| ParamDef {
        name: name.into(),
        ty,
        nullable,
    };
    vec![
        p("org", ScalarType::U64, false),
        p("rate", ScalarType::F64, false),
        p("tag", ScalarType::Bytes, true),
        p("s", ScalarType::Text, true),
        p("k", ScalarType::U64, false),
    ]
}
fn all_binds() -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("org".to_string(), Value::U64(2)),
        ("rate".to_string(), Value::F64(0.0)),
        ("tag".to_string(), Value::Bytes(vec![1])),
        ("s".to_string(), Value::Text("b".into())),
        ("k".to_string(), Value::U64(2)),
    ])
}

fn prepare(c: &HermeticCatalog, sql: &str, limits: Limits) -> Option<PreparedQuery> {
    let used: Vec<ParamDef> = defs()
        .into_iter()
        .filter(|d| sql.contains(&format!(":{}", d.name)))
        .collect();
    c.prepare(sql, &used, limits).ok()
}
fn binds_for(q: &PreparedQuery) -> BTreeMap<String, Value> {
    all_binds()
        .into_iter()
        .filter(|(k, _)| q.parameters().contains_key(k))
        .collect()
}

const PREDICATES: &[&str] = &[
    "TRUE",
    "FALSE",
    "NULL",
    "org = :org",
    "org <> :org",
    "org < :org",
    "cents > -10",
    "cents <= 500",
    "rate = :rate",
    "rate < :rate",
    "rate >= :rate",
    "paid",
    "paid < TRUE",
    "NOT paid",
    "paid IS NULL",
    "note = 'ab'",
    "note < 'b'",
    "tag = :tag",
    "tag > :tag",
    "cents = NULL",
    "NOT (cents > 0 AND paid)",
    "(cents > 0 OR rate < :rate) AND NOT note IS NULL",
    "(paid OR NULL) AND (cents < 100 OR NULL)",
    "1 < 0 OR org = :org",
];
const PROJECTIONS: &[&str] = &[
    "*",
    "id, cents, rate",
    "note, tag, paid",
    "id, cents > 0 AS positive, paid IS NULL AS unknown, :org AS bound, 'k' AS k",
    "org, NOT paid AS unpaid, paid AND cents > 0 AS both, NULL AS n",
];
const ONS: &[&str] = &[
    "a.k = b.k",
    "a.k = b.k AND a.f = b.f",
    "b.k = a.k AND a.s = b.s",
    "a.k = b.k AND b.b",
    "a.f = b.f",
    "a.k < b.k",
    "a.k = b.k OR a.s = b.s",
];
const WHERES: &[&str] = &[
    "",
    " WHERE a.id >= 3",
    " WHERE b.id IS NULL",
    " WHERE a.s = :s OR b.b",
    " WHERE a.k = :k AND b.s <> 'a'",
];
const KINDS: &[&str] = &["", "LEFT", "RIGHT", "FULL"];
const CHAINS: &[&str] = &[
    "SELECT a.id, b.id AS bid, g.tag FROM l a JOIN r b ON a.k = b.k JOIN m g ON g.k = b.k",
    "SELECT a.id, g.k FROM m g JOIN l a ON a.k = g.k JOIN r b ON b.k = a.k AND b.f = a.f WHERE b.b",
    "SELECT a.s, b.s AS bs FROM l a JOIN r b ON a.s = b.s JOIN m g ON g.k = :k JOIN l c ON c.k = a.k",
    "SELECT a.id FROM l a JOIN r b ON TRUE JOIN m g ON a.k = g.k AND b.k = g.k",
    "SELECT a.id, b.id AS bid, g.tag FROM l a LEFT JOIN r b ON a.k = b.k JOIN m g ON g.k = a.k JOIN l c ON c.id = a.id",
];

fn engines() -> Vec<VectorEngine> {
    vec![
        VectorEngine::portable(),
        VectorEngine::portable().with_batch(1).unwrap(),
        VectorEngine::portable().with_batch(7).unwrap(),
    ]
}
fn tight() -> Vec<Limits> {
    let d = Limits::default();
    vec![
        d,
        Limits { work: 2000, ..d },
        Limits { work: 20000, ..d },
        Limits {
            intermediate_rows: 40,
            ..d
        },
        Limits {
            intermediate_rows: 400,
            ..d
        },
        Limits {
            intermediate_bytes: 3000,
            ..d
        },
        Limits {
            intermediate_bytes: 60000,
            ..d
        },
        Limits {
            output_rows: 9,
            ..d
        },
        Limits {
            output_bytes: 700,
            ..d
        },
        Limits {
            input_rows: 100,
            ..d
        },
    ]
}

fn show<T: std::fmt::Debug>(r: vesper_quarry::Result<T>) -> String {
    match r {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR {:?} {}", e.kind, e.message),
    }
}

const AGGS: &[Aggregate] = &[
    Aggregate::CountRows,
    Aggregate::Count(0),
    Aggregate::Sum(0),
    Aggregate::Min(0),
    Aggregate::Max(0),
    Aggregate::Count(1),
    Aggregate::Sum(1),
    Aggregate::Min(1),
    Aggregate::Max(1),
    Aggregate::Min(2),
    Aggregate::Max(2),
];

/// One query on every engine: rows, aggregates per output column, tiles
/// and join orders.
fn run_all(out: &mut Vec<String>, c: &HermeticCatalog, d: &DataSet, q: &PreparedQuery) {
    let binds = binds_for(q);
    for e in engines() {
        out.push(show(q.execute_vectorized(c, d, &binds, &e)));
        for a in AGGS {
            out.push(show(q.aggregate_vectorized(c, d, &binds, &e, &[*a])));
        }
        out.push(show(q.aggregate_vectorized(c, d, &binds, &e, AGGS)));
        let mut tiles = TileSet::new();
        let r = q.execute_tiled(c, d, &binds, &e, &mut tiles);
        out.push(format!("{} {:?}", show(r), tiles.rows()));
        out.push(show(q.vectorized_join_orders(c, d, &binds, &e)));
    }
    if let Ok(rows) = q.execute(c, d, &binds) {
        for a in AGGS {
            out.push(show(aggregate_rows(&rows, &[*a])));
        }
    }
}

fn digest() {
    let c = catalog();
    let mut out = Vec::new();
    for (seed, nl, nr, keys) in [(1, 40, 30, 6), (2, 0, 9, 3), (3, 25, 0, 4)] {
        let d = data(&c, seed, 300, nl, nr, keys);
        for lim in tight() {
            for sel in PROJECTIONS {
                for p in PREDICATES {
                    let sql = format!("SELECT {sel} FROM orders WHERE {p}");
                    if let Some(q) = prepare(&c, &sql, lim) {
                        out.push(sql.clone());
                        run_all(&mut out, &c, &d, &q);
                        run_all(&mut out, &c, &d, &q.optimized());
                    }
                }
            }
            for kind in KINDS {
                for on in ONS {
                    for w in WHERES {
                        let sql = format!(
                            "SELECT a.id, a.s, b.id AS bid, b.f, b.b FROM l a {kind} JOIN r b ON {on}{w}"
                        );
                        if let Some(q) = prepare(&c, &sql, lim) {
                            out.push(sql.clone());
                            run_all(&mut out, &c, &d, &q);
                            run_all(&mut out, &c, &d, &q.optimized());
                        }
                    }
                }
            }
            for sql in CHAINS {
                if let Some(q) = prepare(&c, sql, lim) {
                    out.push(sql.to_string());
                    run_all(&mut out, &c, &d, &q);
                    run_all(&mut out, &c, &d, &q.optimized());
                }
            }
        }
        // A stale catalog and stale data.
        let q = prepare(&c, "SELECT id FROM orders", Limits::default()).unwrap();
        let mut changed = c.clone();
        changed
            .apply_ddl("CREATE TABLE extra (x U64)", Limits::default())
            .unwrap();
        let e = VectorEngine::portable();
        let none = BTreeMap::new();
        out.push(show(q.execute_vectorized(&changed, &d, &none, &e)));
        out.push(show(q.aggregate_vectorized(
            &changed,
            &d,
            &none,
            &e,
            &[Aggregate::Min(4)]
        )));
        out.push(show(q.aggregate_vectorized(&changed, &d, &none, &e, &[Aggregate::Sum(0)])));
    }
    for line in out {
        println!("{line}");
    }
}

struct Work {
    c: HermeticCatalog,
    d: DataSet,
    q: PreparedQuery,
    binds: BTreeMap<String, Value>,
    e: VectorEngine,
    tiles: TileSet,
}
fn workload(name: &str) -> Work {
    let c = catalog();
    let (d, sql) = match name {
        "scan" | "agg" | "tiles" => (
            data(&c, 9, 8000, 0, 0, 1),
            "SELECT id, cents, rate, note FROM orders WHERE cents > -500 AND (paid OR note < 'b')",
        ),
        "join" => (
            data(&c, 9, 0, 3000, 3000, 2500),
            "SELECT a.id, a.s, b.id AS bid, b.f FROM l a JOIN r b ON a.k = b.k AND a.f <> b.f",
        ),
        "chain" => (
            data(&c, 9, 0, 3000, 3000, 2500),
            "SELECT a.id, b.id AS bid, g.tag FROM l a JOIN r b ON a.k = b.k JOIN m g ON g.k = b.k",
        ),
        _ => panic!("unknown workload {name}"),
    };
    let q = prepare(&c, sql, Limits::default()).unwrap().optimized();
    let binds = binds_for(&q);
    Work {
        c,
        d,
        q,
        binds,
        e: VectorEngine::detect(),
        tiles: TileSet::new(),
    }
}
fn pass(name: &str, w: &mut Work) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match name {
        "agg" => {
            let aggs = [
                Aggregate::CountRows,
                Aggregate::Sum(1),
                Aggregate::Min(2),
                Aggregate::Max(3),
                Aggregate::Count(3),
            ];
            let r = w.q.aggregate_vectorized(&w.c, &w.d, &w.binds, &w.e, &aggs).unwrap();
            format!("{r:?}").hash(&mut h);
        }
        "tiles" => {
            let work = w.q.execute_tiled(&w.c, &w.d, &w.binds, &w.e, &mut w.tiles).unwrap();
            (work, w.tiles.len()).hash(&mut h);
        }
        _ => {
            let r = w.q.execute_vectorized(&w.c, &w.d, &w.binds, &w.e).unwrap();
            (r.work, r.rows.len(), format!("{:?}", r.rows.last())).hash(&mut h);
        }
    }
    h.finish()
}

fn window(name: &str, passes: usize) -> (u128, u64) {
    let mut w = workload(name);
    let digest = pass(name, &mut w);
    let start = Instant::now();
    for _ in 0..passes {
        std::hint::black_box(pass(name, std::hint::black_box(&mut w)));
    }
    (start.elapsed().as_nanos(), digest)
}

fn run(bin: &str, name: &str, passes: usize) -> (u64, String) {
    let out = Command::new(bin)
        .args(["window", name, &passes.to_string()])
        .output()
        .expect("run arm");
    assert!(out.status.success(), "{bin} failed");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (ns, digest) = text.trim().split_once(' ').expect("`ns digest`");
    (ns.parse().unwrap(), digest.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("digest") => digest(),
        Some("window") => {
            let (ns, digest) = window(&args[2], args[3].parse().unwrap());
            println!("{ns} {digest}");
        }
        Some("driver") => {
            let (a, b, name) = (&args[2], &args[3], &args[4]);
            // About 0.3 s of the A arm per window.
            let (probe, _) = run(a, name, 5);
            let passes = ((3e8 / (probe as f64 / 5.0)) as usize).clamp(5, 20000);
            let mut pairs = Vec::new();
            let mut ratios = Vec::new();
            for epoch in 0..EPOCHS {
                let (a1, da) = run(a, name, passes);
                let (b1, db) = run(b, name, passes);
                let (b2, _) = run(b, name, passes);
                let (a2, _) = run(a, name, passes);
                assert_eq!(da, db, "the arms computed different results");
                let per = |t: u64| t as f64 / (2 * passes) as f64 / 1000.0;
                println!(
                    "epoch {epoch}: A {:.1} us/pass, B {:.1} us/pass, B/A {:.3}",
                    per(a1 + a2),
                    per(b1 + b2),
                    (b1 + b2) as f64 / (a1 + a2) as f64
                );
                ratios.push((b1 + b2) as f64 / (a1 + a2) as f64);
                pairs.push(
                    vesper_bench::PairedSample::new(a1 + a2, b1 + b2, 2 * passes as u64).unwrap(),
                );
            }
            ratios.sort_by(f64::total_cmp);
            let analysis = vesper_bench::analyze_pairs(&pairs, 10_000).unwrap();
            println!(
                "{name}: median B/A {:.3}x (min {:.3}x, max {:.3}x); 2x gate: {:?}",
                ratios[EPOCHS / 2],
                ratios[0],
                ratios[EPOCHS - 1],
                analysis.decision
            );
            if let Some(interval) = analysis.interval {
                println!(
                    "95% median interval of B/A: {:.3}x..{:.3}x",
                    ratios[interval.lower_rank - 1],
                    ratios[interval.upper_rank - 1]
                );
            }
        }
        _ => {
            eprintln!("usage: vector_abba digest | window W N | driver RUST_BIN PORT_BIN W");
            std::process::exit(2);
        }
    }
}

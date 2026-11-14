use std::collections::BTreeMap;
use vesper_quarry::vector::{aggregate_rows, Aggregate, VectorEngine};
use vesper_quarry::{
    DataSet, ErrorKind, HermeticCatalog, Limits, ParamDef, PreparedQuery, ScalarType, Value,
};

fn catalog() -> HermeticCatalog {
    let mut c = HermeticCatalog::default();
    c.apply_ddl(
        "CREATE TABLE orders (id I64 NOT NULL, org U64 NOT NULL, cents I64, \
         rate F64, paid BOOL, note TEXT, tag BYTES); \
         CREATE TABLE orgs (id U64 NOT NULL)",
        Limits::default(),
    )
    .unwrap();
    c
}

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
    fn null_or(&mut self, v: Value) -> Value {
        if self.below(5) == 0 {
            Value::Null
        } else {
            v
        }
    }
}

fn rows(seed: u64, n: usize) -> Vec<Vec<Value>> {
    let mut r = Rng(seed);
    let floats = [0.0, -0.0, 1.5, -2.25, 1e300, -1e-300, 3.0];
    (0..n)
        .map(|i| {
            let cents = r.below(2001) as i64 - 1000;
            let rate = floats[r.below(floats.len() as u64) as usize];
            let paid = r.below(2) == 0;
            let note = ["", "a", "ab", "b", "Béa", "zz"][r.below(6) as usize].to_string();
            let tag = vec![r.below(3) as u8; r.below(3) as usize];
            vec![
                Value::I64(i as i64),
                Value::U64(r.below(4)),
                r.null_or(Value::I64(cents)),
                r.null_or(Value::F64(rate)),
                r.null_or(Value::Bool(paid)),
                r.null_or(Value::Text(note)),
                r.null_or(Value::Bytes(tag)),
            ]
        })
        .collect()
}

fn params() -> (Vec<ParamDef>, BTreeMap<String, Value>) {
    let defs = vec![
        ParamDef {
            name: "org".into(),
            ty: ScalarType::U64,
            nullable: false,
        },
        ParamDef {
            name: "rate".into(),
            ty: ScalarType::F64,
            nullable: false,
        },
        ParamDef {
            name: "tag".into(),
            ty: ScalarType::Bytes,
            nullable: true,
        },
    ];
    let binds = BTreeMap::from([
        ("org".to_string(), Value::U64(2)),
        ("rate".to_string(), Value::F64(0.0)),
        ("tag".to_string(), Value::Bytes(vec![1])),
    ]);
    (defs, binds)
}

/// Predicates covering every comparison operator on every scalar type,
/// parameters, literals, NULL constants, IS [NOT] NULL, NOT and the AND/OR
/// truth tables.
const PREDICATES: &[&str] = &[
    "TRUE",
    "FALSE",
    "NULL",
    "org = :org",
    "org <> :org",
    "org < :org",
    "org >= :org",
    "cents = 0",
    "cents > -10",
    "cents <= 500",
    "cents != 7",
    "rate = :rate",
    "rate < :rate",
    "rate >= :rate",
    "rate > :rate",
    "paid",
    "paid = TRUE",
    "paid < TRUE",
    "NOT paid",
    "paid IS NULL",
    "paid IS NOT NULL",
    "note = 'ab'",
    "note < 'b'",
    "note >= 'Béa'",
    "tag = :tag",
    "tag > :tag",
    "tag IS NULL",
    "cents = NULL",
    "NULL = cents",
    "NOT (cents > 0)",
    "cents > 0 AND paid",
    "cents > 0 OR paid",
    "NOT (cents > 0 AND paid)",
    "(cents > 0 OR rate < :rate) AND NOT note IS NULL",
    "paid AND (note = 'a' OR tag = :tag)",
    "(paid OR NULL) AND (cents < 100 OR NULL)",
    "cents > 0 AND cents < 0",
    "1 = 1",
    "1 < 0 OR org = :org",
];

const PROJECTIONS: &[&str] = &[
    "*",
    "id, cents, rate",
    "note, tag, paid",
    "id, cents > 0 AS positive, paid IS NULL AS unknown, :org AS bound, 'k' AS k",
    "org, NOT paid AS unpaid, paid AND cents > 0 AS both",
];

fn prepare(c: &HermeticCatalog, select: &str, predicate: &str) -> PreparedQuery {
    let (defs, _) = params();
    let mut used = Vec::new();
    let sql = format!("SELECT {select} FROM orders WHERE {predicate}");
    for d in &defs {
        if sql.contains(&format!(":{}", d.name)) {
            used.push(d.clone());
        }
    }
    c.prepare(&sql, &used, Limits::default())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}
fn binds_for(q: &PreparedQuery) -> BTreeMap<String, Value> {
    let (_, all) = params();
    all.into_iter()
        .filter(|(k, _)| q.parameters().contains_key(k))
        .collect()
}

#[test]
fn vectorized_equals_row_executor_for_every_predicate_projection_and_batch() {
    let c = catalog();
    let mut d = DataSet::new(&c);
    d.set_rows("orders", &rows(7, 700), Limits::default())
        .unwrap();
    d.set_rows("orgs", &[], Limits::default()).unwrap();
    let engines = [
        VectorEngine::detect(),
        VectorEngine::portable(),
        VectorEngine::detect().with_batch(1).unwrap(),
        VectorEngine::detect().with_batch(7).unwrap(),
        VectorEngine::portable().with_batch(64).unwrap(),
    ];
    let mut compared = 0;
    let mut nonempty = 0;
    for select in PROJECTIONS {
        for predicate in PREDICATES {
            for q in [
                prepare(&c, select, predicate),
                prepare(&c, select, predicate).optimized(),
            ] {
                let binds = binds_for(&q);
                let expected = q.execute(&c, &d, &binds).unwrap();
                nonempty += usize::from(!expected.rows.is_empty());
                for e in &engines {
                    let got = q.execute_vectorized(&c, &d, &binds, e).unwrap();
                    assert_eq!(got.columns, expected.columns, "{select} WHERE {predicate}");
                    assert_eq!(got.rows, expected.rows, "{select} WHERE {predicate} {e:?}");
                    compared += 1;
                }
            }
        }
    }
    assert_eq!(
        compared,
        PROJECTIONS.len() * PREDICATES.len() * 2 * engines.len()
    );
    assert!(nonempty > compared / engines.len() / 2);
}

#[test]
fn vectorized_aggregates_equal_row_aggregation() {
    let c = catalog();
    let mut d = DataSet::new(&c);
    d.set_rows("orders", &rows(11, 900), Limits::default())
        .unwrap();
    d.set_rows("orgs", &[], Limits::default()).unwrap();
    let aggs = [
        Aggregate::CountRows,
        Aggregate::Count(1),
        Aggregate::Count(2),
        Aggregate::Sum(0),
        Aggregate::Sum(1),
        Aggregate::Sum(2),
        Aggregate::Sum(7),
        Aggregate::Min(1),
        Aggregate::Max(1),
        Aggregate::Min(2),
        Aggregate::Max(3),
        Aggregate::Min(4),
        Aggregate::Max(5),
        Aggregate::Min(6),
        Aggregate::Max(7),
        Aggregate::Count(7),
    ];
    let select = "org, cents, rate, paid, note, tag, cents > 0 AS pos, :org AS bound";
    for predicate in PREDICATES {
        let q = prepare(&c, &format!("{select}, id"), predicate);
        let binds = binds_for(&q);
        let rows = q.execute(&c, &d, &binds).unwrap();
        let mut agg_set = aggs.to_vec();
        agg_set.push(Aggregate::Sum(8));
        let expected = aggregate_rows(&rows, &agg_set).unwrap();
        for engine in [
            VectorEngine::detect(),
            VectorEngine::portable().with_batch(13).unwrap(),
        ] {
            let got = q
                .aggregate_vectorized(&c, &d, &binds, &engine, &agg_set)
                .unwrap();
            assert_eq!(got, expected, "WHERE {predicate}");
        }
        assert_eq!(expected[0], Value::U64(rows.rows.len() as u64));
    }
}

#[test]
fn aggregate_types_overflow_and_empty_inputs() {
    let mut c = HermeticCatalog::default();
    c.apply_ddl(
        "CREATE TABLE t (u U64, i I64 NOT NULL, s TEXT)",
        Limits::default(),
    )
    .unwrap();
    let mut d = DataSet::new(&c);
    d.set_rows(
        "t",
        &[
            vec![Value::U64(u64::MAX), Value::I64(i64::MAX), Value::Null],
            vec![Value::U64(1), Value::I64(1), Value::Text("x".into())],
            vec![Value::Null, Value::I64(-2), Value::Null],
        ],
        Limits::default(),
    )
    .unwrap();
    let q = c
        .prepare("SELECT u, i, s FROM t", &[], Limits::default())
        .unwrap();
    let e = VectorEngine::detect();
    let none = BTreeMap::new();
    // Exact integer SUM: u overflows; i fits even though a prefix overflows.
    {
        let aggs = [Aggregate::Sum(0)];
        assert_eq!(
            q.aggregate_vectorized(&c, &d, &none, &e, &aggs)
                .unwrap_err()
                .kind,
            ErrorKind::Overflow
        );
        let rows = q.execute(&c, &d, &none).unwrap();
        assert_eq!(
            aggregate_rows(&rows, &aggs).unwrap_err().kind,
            ErrorKind::Overflow
        );
    }
    assert_eq!(
        q.aggregate_vectorized(&c, &d, &none, &e, &[Aggregate::Sum(1)])
            .unwrap(),
        vec![Value::I64(i64::MAX - 1)]
    );
    assert_eq!(
        q.aggregate_vectorized(&c, &d, &none, &e, &[Aggregate::Sum(2)])
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
    assert_eq!(
        q.aggregate_vectorized(&c, &d, &none, &e, &[Aggregate::Min(9)])
            .unwrap_err()
            .kind,
        ErrorKind::Unknown
    );
    let empty = c
        .prepare(
            "SELECT u, i, s FROM t WHERE i > 100 AND i < 0",
            &[],
            Limits::default(),
        )
        .unwrap();
    assert_eq!(
        empty
            .aggregate_vectorized(
                &c,
                &d,
                &none,
                &e,
                &[
                    Aggregate::CountRows,
                    Aggregate::Sum(0),
                    Aggregate::Min(2),
                    Aggregate::Count(1)
                ]
            )
            .unwrap(),
        vec![Value::U64(0), Value::Null, Value::Null, Value::U64(0)]
    );
}

#[test]
fn joins_stale_inputs_and_limits_are_typed_errors() {
    let c = catalog();
    let mut d = DataSet::new(&c);
    d.set_rows("orders", &rows(3, 50), Limits::default())
        .unwrap();
    d.set_rows("orgs", &[vec![Value::U64(1)]], Limits::default())
        .unwrap();
    let e = VectorEngine::detect();
    let none = BTreeMap::new();
    let join = c
        .prepare(
            "SELECT o.id FROM orders o JOIN orgs g ON o.org = g.id",
            &[],
            Limits::default(),
        )
        .unwrap();
    // Joins run vectorized now (tests/vector_join.rs) and match the rows.
    assert_eq!(
        join.execute_vectorized(&c, &d, &none, &e).unwrap().rows,
        join.execute(&c, &d, &none).unwrap().rows
    );
    let q = c
        .prepare("SELECT id FROM orders", &[], Limits::default())
        .unwrap();
    let mut changed = c.clone();
    changed
        .apply_ddl("CREATE TABLE extra (x U64)", Limits::default())
        .unwrap();
    assert_eq!(
        q.execute_vectorized(&changed, &d, &none, &e)
            .unwrap_err()
            .kind,
        ErrorKind::Stale
    );
    let tight = Limits {
        output_rows: 10,
        ..Limits::default()
    };
    let q = c.prepare("SELECT id FROM orders", &[], tight).unwrap();
    assert_eq!(
        q.execute_vectorized(&c, &d, &none, &e).unwrap_err().kind,
        ErrorKind::Limit
    );
    assert!(VectorEngine::detect().with_batch(0).is_err());
    assert!(VectorEngine::detect().with_batch(1 << 17).is_err());
}

#[test]
fn simd_mask_count_matches_portable_count() {
    let simd = VectorEngine::detect();
    if !simd.simd_active() {
        eprintln!("Baseline128 unavailable on this host; portable count only");
    }
    let portable = VectorEngine::portable();
    let mut r = Rng(5);
    for len in [0, 1, 15, 16, 17, 31, 64, 1000, 4097] {
        let mask: Vec<u8> = (0..len).map(|_| (r.below(2)) as u8).collect();
        assert_eq!(simd.count_ones(&mask), portable.count_ones(&mask));
    }
}

#[test]
fn text_and_bytes_parameters_are_compared_without_per_row_copies() {
    let mut c = HermeticCatalog::default();
    c.apply_ddl(
        "CREATE TABLE t (s TEXT, b BYTES, f BOOL)",
        Limits::default(),
    )
    .unwrap();
    let mut d = DataSet::new(&c);
    let rows: Vec<Vec<Value>> = (0..1024)
        .map(|i| {
            vec![
                Value::Text(format!("{i}")),
                Value::Bytes(vec![i as u8]),
                Value::Bool(i % 2 == 0),
            ]
        })
        .collect();
    d.set_rows("t", &rows, Limits::default()).unwrap();
    let defs = [
        ParamDef {
            name: "s".into(),
            ty: ScalarType::Text,
            nullable: false,
        },
        ParamDef {
            name: "b".into(),
            ty: ScalarType::Bytes,
            nullable: false,
        },
        ParamDef {
            name: "f".into(),
            ty: ScalarType::Bool,
            nullable: false,
        },
    ];
    let q = c
        .prepare(
            "SELECT s FROM t WHERE (s < :s OR b >= :b) AND f = :f",
            &defs,
            Limits::default(),
        )
        .unwrap();
    // A 3 MiB parameter: broadcasting it per row would need gigabytes.
    let binds = BTreeMap::from([
        ("s".to_string(), Value::Text("5".repeat(3 << 20))),
        ("b".to_string(), Value::Bytes(vec![200; 16])),
        ("f".to_string(), Value::Bool(true)),
    ]);
    // The row executor's work budget rejects this; compute the answer directly.
    let big = "5".repeat(3 << 20);
    let expected: Vec<Vec<Value>> = rows
        .iter()
        .filter(|r| {
            let (Value::Text(s), Value::Bytes(b), Value::Bool(f)) = (&r[0], &r[1], &r[2]) else {
                unreachable!()
            };
            (s.as_str() < big.as_str() || b.as_slice() >= &[200u8; 16][..]) && *f
        })
        .map(|r| vec![r[0].clone()])
        .collect();
    let got = q
        .execute_vectorized(&c, &d, &binds, &VectorEngine::detect())
        .unwrap();
    assert_eq!(got.rows, expected);
    assert!(!got.rows.is_empty());
    // Small parameters: equal to the row executor too.
    let small = BTreeMap::from([
        ("s".to_string(), Value::Text("5".into())),
        ("b".to_string(), Value::Bytes(vec![200])),
        ("f".to_string(), Value::Bool(false)),
    ]);
    assert_eq!(
        q.execute_vectorized(&c, &d, &small, &VectorEngine::portable())
            .unwrap()
            .rows,
        q.execute(&c, &d, &small).unwrap().rows
    );
}

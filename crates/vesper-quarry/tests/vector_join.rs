//! Differential tests of multi-table vectorized plans (hash joins, nested-
//! loop joins, filters above joins) and AoSoA tile output against the row
//! executor: same rows, same order, same column definitions.
use std::collections::BTreeMap;
use vesper_quarry::vector::{aggregate_rows, Aggregate, TileSet, VectorEngine, TILE_LANES};
use vesper_quarry::{DataSet, HermeticCatalog, Limits, ParamDef, ScalarType, Value};

fn catalog() -> HermeticCatalog {
    let mut c = HermeticCatalog::default();
    c.apply_ddl(
        "CREATE TABLE l (id I64 NOT NULL, k U64 NULL, f F64 NULL, s TEXT NULL); \
         CREATE TABLE r (id I64 NOT NULL, k U64 NULL, f F64 NULL, b BOOL NULL, s TEXT NULL); \
         CREATE TABLE m (k U64 NOT NULL, tag BYTES NULL)",
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
    fn maybe(&mut self, v: impl FnOnce(&mut Self) -> Value) -> Value {
        if self.below(5) == 0 {
            Value::Null
        } else {
            v(self)
        }
    }
}
fn data(c: &HermeticCatalog, seed: u64, nl: usize, nr: usize) -> DataSet {
    let mut g = Rng(seed);
    let floats = [0.0, -0.0, 1.5, 2.0, -3.25];
    let texts = ["", "a", "b", "Béa"];
    let l: Vec<Vec<Value>> = (0..nl)
        .map(|i| {
            vec![
                Value::I64(i as i64),
                g.maybe(|g| Value::U64(g.below(6))),
                g.maybe(|g| Value::F64(floats[g.below(5) as usize])),
                g.maybe(|g| Value::Text(texts[g.below(4) as usize].into())),
            ]
        })
        .collect();
    let r: Vec<Vec<Value>> = (0..nr)
        .map(|i| {
            vec![
                Value::I64(100 + i as i64),
                g.maybe(|g| Value::U64(g.below(6))),
                g.maybe(|g| Value::F64(floats[g.below(5) as usize])),
                g.maybe(|g| Value::Bool(g.below(2) == 0)),
                g.maybe(|g| Value::Text(texts[g.below(4) as usize].into())),
            ]
        })
        .collect();
    let m: Vec<Vec<Value>> = (0..6u64)
        .map(|k| {
            vec![
                Value::U64(k),
                g.maybe(|_| Value::Bytes(vec![k as u8; (k % 3) as usize])),
            ]
        })
        .collect();
    let mut d = DataSet::new(c);
    d.set_rows("l", &l, Limits::default()).unwrap();
    d.set_rows("r", &r, Limits::default()).unwrap();
    d.set_rows("m", &m, Limits::default()).unwrap();
    d
}

const ONS: [&str; 7] = [
    "a.k = b.k",
    "a.k = b.k AND a.f = b.f",
    "b.k = a.k AND a.s = b.s",
    "a.k = b.k AND b.b",
    "a.f = b.f",
    "a.k < b.k",
    "a.k = b.k OR a.s = b.s",
];
const WHERES: [&str; 6] = [
    "",
    " WHERE a.id >= 3",
    " WHERE b.id IS NULL",
    " WHERE a.s = :s OR b.b",
    " WHERE NOT (a.f > b.f)",
    " WHERE a.k = :k AND b.s <> 'a'",
];
const KINDS: [&str; 4] = ["", "LEFT", "RIGHT", "FULL"];

fn params() -> (Vec<ParamDef>, BTreeMap<String, Value>) {
    (
        vec![
            ParamDef {
                name: "s".into(),
                ty: ScalarType::Text,
                nullable: true,
            },
            ParamDef {
                name: "k".into(),
                ty: ScalarType::U64,
                nullable: false,
            },
        ],
        BTreeMap::from([
            ("s".to_string(), Value::Text("b".into())),
            ("k".to_string(), Value::U64(2)),
        ]),
    )
}

#[test]
fn hash_and_nested_loop_joins_match_the_row_executor() {
    let c = catalog();
    let engines = [
        VectorEngine::detect(),
        VectorEngine::portable().with_batch(1).unwrap(),
        VectorEngine::portable().with_batch(7).unwrap(),
        VectorEngine::detect().with_batch(64).unwrap(),
    ];
    let mut tiles = TileSet::new();
    let mut compared = 0;
    for seed in 0..3 {
        let d = data(&c, seed, 25, 20);
        for kind in KINDS {
            for on in ONS {
                for wh in WHERES {
                    let sql = format!(
                        "SELECT a.id, b.id AS bid, a.s, b.f, b.b FROM l a {kind} JOIN r b ON {on}{wh}"
                    );
                    let (defs, binds) = params();
                    let used: Vec<ParamDef> = defs
                        .into_iter()
                        .filter(|p| sql.contains(&format!(":{}", p.name)))
                        .collect();
                    let binds: BTreeMap<String, Value> = binds
                        .into_iter()
                        .filter(|(n, _)| used.iter().any(|p| &p.name == n))
                        .collect();
                    let q = c.prepare(&sql, &used, Limits::default()).unwrap();
                    for q in [q.clone(), q.optimized()] {
                        let expect = q.execute(&c, &d, &binds).unwrap();
                        for e in &engines {
                            let got = q.execute_vectorized(&c, &d, &binds, e).unwrap();
                            assert_eq!(got.rows, expect.rows, "{sql} batch {}", e.batch());
                            assert_eq!(got.columns, expect.columns);
                            q.execute_tiled(&c, &d, &binds, e, &mut tiles).unwrap();
                            assert_eq!(tiles.rows(), expect.rows, "tiles: {sql}");
                            compared += 2;
                        }
                    }
                }
            }
        }
    }
    eprintln!("{compared} join comparisons");
    assert_eq!(compared, 3 * 4 * 7 * 6 * 2 * 4 * 2);
}

#[test]
fn three_way_and_self_joins_and_aggregates_match() {
    let c = catalog();
    let d = data(&c, 9, 30, 30);
    let e = VectorEngine::detect().with_batch(8).unwrap();
    for sql in [
        "SELECT a.id, b.id AS bid, x.tag FROM l a JOIN r b ON a.k = b.k LEFT JOIN m x ON b.k = x.k",
        "SELECT a.id, x.k FROM l a FULL JOIN m x ON a.k = x.k RIGHT JOIN r b ON b.k = x.k AND b.b",
        "SELECT a.id, z.id AS zid FROM l a JOIN l z ON a.k = z.k AND a.id < z.id",
        "SELECT a.id, b.id AS bid FROM l a JOIN r b ON a.k = b.k JOIN m x ON x.k = a.k WHERE x.tag IS NOT NULL",
    ] {
        let q = c.prepare(sql, &[], Limits::default()).unwrap().optimized();
        let none = BTreeMap::new();
        let expect = q.execute(&c, &d, &none).unwrap();
        assert!(!expect.rows.is_empty(), "{sql}");
        let got = q.execute_vectorized(&c, &d, &none, &e).unwrap();
        assert_eq!(got.rows, expect.rows, "{sql}");
        let aggs = [Aggregate::CountRows, Aggregate::Count(1), Aggregate::Min(0), Aggregate::Max(1)];
        assert_eq!(
            q.aggregate_vectorized(&c, &d, &none, &e, &aggs).unwrap(),
            aggregate_rows(&expect, &aggs).unwrap(),
            "{sql}"
        );
    }
}

#[test]
fn tiles_are_aosoa_and_reused_across_executions() {
    let c = catalog();
    let d = data(&c, 4, 40, 40);
    let q = c
        .prepare(
            "SELECT a.id, b.k, a.s, b.f FROM l a JOIN r b ON a.k = b.k",
            &[],
            Limits::default(),
        )
        .unwrap();
    let none = BTreeMap::new();
    let e = VectorEngine::detect();
    let expect = q.execute(&c, &d, &none).unwrap();
    let mut tiles = TileSet::new();
    q.execute_tiled(&c, &d, &none, &e, &mut tiles).unwrap();
    assert_eq!(tiles.len(), expect.rows.len());
    assert_eq!(tiles.tiles().len(), expect.rows.len().div_ceil(TILE_LANES));
    // Hot columns are lane arrays: column 0 (a.id) of tile 0.
    let k = tiles.hot_index(0).unwrap();
    let lanes: Vec<i64> = tiles.tiles()[0].hot(k)[..tiles.tiles()[0].lanes()]
        .iter()
        .map(|&w| w as i64)
        .collect();
    let want: Vec<i64> = expect.rows[..lanes.len()]
        .iter()
        .map(|r| match r[0] {
            Value::I64(x) => x,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(lanes, want);
    assert_eq!(tiles.hot_index(2), None, "text is a cold column");
    let allocated = tiles.allocated_tiles();
    let arena = tiles.arena_capacity();
    q.execute_tiled(&c, &d, &none, &e, &mut tiles).unwrap();
    assert_eq!(tiles.allocated_tiles(), allocated, "tiles reused");
    assert_eq!(tiles.arena_capacity(), arena, "arena reused");
    assert_eq!(tiles.rows(), expect.rows);
}

#[test]
fn join_budgets_are_enforced() {
    let c = catalog();
    let d = data(&c, 1, 60, 60);
    let e = VectorEngine::detect();
    let none = BTreeMap::new();
    let small = Limits {
        intermediate_rows: 100,
        ..Limits::default()
    };
    let q = c
        .prepare("SELECT a.id FROM l a JOIN r b ON a.k < b.k", &[], small)
        .unwrap();
    assert_eq!(
        q.execute_vectorized(&c, &d, &none, &e).unwrap_err().kind,
        vesper_quarry::ErrorKind::Limit
    );
    let tiny = Limits {
        work: 50,
        ..Limits::default()
    };
    let q = c
        .prepare("SELECT a.id FROM l a JOIN r b ON a.k = b.k", &[], tiny)
        .unwrap();
    assert_eq!(
        q.execute_vectorized(&c, &d, &none, &e).unwrap_err().kind,
        vesper_quarry::ErrorKind::Limit
    );
}

/// A wide value repeated by a join is charged against the intermediate byte
/// budget before it is copied, as in the row executor.
#[test]
fn join_intermediate_bytes_are_bounded() {
    let c = catalog();
    let wide = "x".repeat(100_000);
    let l = vec![vec![
        Value::I64(0),
        Value::U64(1),
        Value::Null,
        Value::Text(wide),
    ]];
    let r: Vec<Vec<Value>> = (0..200)
        .map(|i| {
            vec![
                Value::I64(i),
                Value::U64(1),
                Value::Null,
                Value::Null,
                Value::Null,
            ]
        })
        .collect();
    let mut d = DataSet::new(&c);
    d.set_rows("l", &l, Limits::default()).unwrap();
    d.set_rows("r", &r, Limits::default()).unwrap();
    d.set_rows("m", &[], Limits::default()).unwrap();
    let none = BTreeMap::new();
    for on in ["a.k = b.k", "a.k = b.k AND b.id >= 0", "a.k <= b.k"] {
        let sql = format!("SELECT a.id FROM l a JOIN r b ON {on}");
        let q = c.prepare(&sql, &[], Limits::default()).unwrap();
        let row = q.execute(&c, &d, &none).unwrap_err().kind;
        assert_eq!(row, vesper_quarry::ErrorKind::Limit, "{sql}");
        for e in [
            VectorEngine::detect(),
            VectorEngine::portable().with_batch(7).unwrap(),
        ] {
            let got = q.execute_vectorized(&c, &d, &none, &e).unwrap_err();
            assert_eq!(got.kind, vesper_quarry::ErrorKind::Limit, "{sql}: {got:?}");
        }
        // Within budget once the wide value is small.
        let mut small = d.clone();
        let mut l2 = l.clone();
        l2[0][3] = Value::Text("x".into());
        small.set_rows("l", &l2, Limits::default()).unwrap();
        let expect = q.execute(&c, &small, &none).unwrap();
        let got = q
            .execute_vectorized(&c, &small, &none, &VectorEngine::detect())
            .unwrap();
        assert_eq!(got.rows, expect.rows, "{sql}");
    }
}

/// Chains of three and four inner joins are reordered by cost (smallest
/// input first, connected inputs next) and still return the row executor's
/// rows in its order, across batch sizes, residuals, NULL keys, a cross
/// product component and an outer join kept as one input.
#[test]
fn reordered_inner_chains_match_the_row_executor() {
    let c = catalog();
    let none = BTreeMap::new();
    let engines = [
        VectorEngine::detect(),
        VectorEngine::portable().with_batch(1).unwrap(),
        VectorEngine::portable().with_batch(5).unwrap(),
    ];
    let mut compared = 0;
    for seed in 0..4 {
        // l is the largest input and written first; m (6 rows) is smallest.
        let d = data(&c, 20 + seed, 30, 20);
        for (sql, first) in [
            (
                "SELECT a.id, b.id AS bid, x.tag FROM l a JOIN r b ON a.k = b.k JOIN m x ON x.k = b.k",
                "m",
            ),
            (
                "SELECT a.id, b.id AS bid, x.k FROM l a JOIN r b ON a.k = b.k AND a.f <> b.f \
                 JOIN m x ON x.k = a.k AND x.tag IS NOT NULL",
                "m",
            ),
            (
                "SELECT a.id, z.id AS zid, x.k FROM l a JOIN l z ON a.k = z.k AND a.id < z.id \
                 JOIN m x ON x.k = z.k JOIN r b ON b.k = x.k WHERE b.b",
                // Optimized, `b.b` filters r below m's size first.
                "?",
            ),
            // No equality to m: a nested-loop step with a residual.
            (
                "SELECT a.id, b.id AS bid, x.k FROM l a JOIN r b ON a.k = b.k JOIN m x ON x.k < b.k",
                "m",
            ),
            (
                "SELECT a.id, b.id AS bid, x.k FROM l a JOIN r b ON a.s = b.s \
                 JOIN (m x LEFT JOIN r y ON y.k = x.k) ON x.k = b.k",
                "",
            ),
        ] {
            let Ok(q) = c.prepare(sql, &[], Limits::default()) else {
                // The parser may not accept a parenthesized join input.
                assert!(first.is_empty(), "{sql}");
                continue;
            };
            for q in [q.clone(), q.optimized()] {
                let expect = q.execute(&c, &d, &none).unwrap();
                for e in &engines {
                    let got = q.execute_vectorized(&c, &d, &none, e).unwrap();
                    assert_eq!(got.rows, expect.rows, "{sql} batch {}", e.batch());
                    compared += 1;
                }
                let orders = q
                    .vectorized_join_orders(&c, &d, &none, &engines[0])
                    .unwrap();
                if first == "?" {
                    assert_eq!(orders.len(), 1, "{sql}");
                } else if !first.is_empty() {
                    assert_eq!(orders.len(), 1, "{sql}");
                    assert_eq!(orders[0][0], first, "{sql}: {orders:?}");
                }
            }
        }
    }
    assert!(compared >= 4 * 4 * 2 * 3, "{compared}");
}

/// The cost-based order puts the cross product last: joining the
/// connected inputs first keeps intermediate results small.
#[test]
fn cost_based_order_prefers_connected_inputs() {
    let c = catalog();
    let d = data(&c, 31, 40, 40);
    let none = BTreeMap::new();
    let q = c
        .prepare(
            "SELECT a.id, b.id AS bid, x.k FROM m x JOIN l a ON a.id < 5 JOIN r b ON b.k = x.k",
            &[],
            Limits::default(),
        )
        .unwrap()
        .optimized();
    let e = VectorEngine::detect();
    let orders = q.vectorized_join_orders(&c, &d, &none, &e).unwrap();
    // x (6 rows) is smallest; b is connected to it by b.k = x.k, while a
    // is connected to nothing (its ON conjunct is a filter), so the cross
    // product with a comes last.
    assert_eq!(orders, vec![vec!["m", "r", "l"]], "{orders:?}");
    let expect = q.execute(&c, &d, &none).unwrap();
    assert_eq!(q.execute_vectorized(&c, &d, &none, &e).unwrap().rows, expect.rows);
}

use std::collections::BTreeMap;
use vesper_quarry::{
    DataSet, ErrorKind, HermeticCatalog, JoinKind, Limits, ParamDef, RelPlan, ScalarType, Value,
};

fn catalog() -> HermeticCatalog {
    let mut c = HermeticCatalog::default();
    c.apply_ddl("CREATE TABLE people (id I64 NOT NULL, team I64, active BOOL, name TEXT NOT NULL); CREATE TABLE teams (id I64 NOT NULL, title TEXT);", Limits::default()).unwrap();
    c
}
fn data(c: &HermeticCatalog) -> DataSet {
    let mut d = DataSet::new(c);
    d.set_rows(
        "people",
        vec![
            vec![
                Value::I64(1),
                Value::I64(10),
                Value::Bool(true),
                Value::Text("O'Neil".into()),
            ],
            vec![
                Value::I64(2),
                Value::I64(20),
                Value::Bool(false),
                Value::Text("Béa".into()),
            ],
            vec![
                Value::I64(3),
                Value::Null,
                Value::Null,
                Value::Text("Carla".into()),
            ],
        ],
        Limits::default(),
    )
    .unwrap();
    d.set_rows(
        "teams",
        vec![
            vec![Value::I64(10), Value::Text("X".into())],
            vec![Value::I64(30), Value::Null],
        ],
        Limits::default(),
    )
    .unwrap();
    d
}
fn check(sql: &str, expected: Vec<Vec<Value>>) {
    let c = catalog();
    let d = data(&c);
    let p = c.prepare(sql, &[], Limits::default()).unwrap();
    assert_eq!(p.execute(&c, &d, &BTreeMap::new()).unwrap().rows, expected);
    assert_eq!(
        p.optimized()
            .execute(&c, &d, &BTreeMap::new())
            .unwrap()
            .rows,
        expected
    );
}
fn error(sql: &str, expected: ErrorKind) {
    assert_eq!(
        catalog()
            .prepare(sql, &[], Limits::default())
            .unwrap_err()
            .kind,
        expected,
        "{sql}"
    );
}
#[test]
fn real_where_aliases_literals_and_typed_bindings() {
    let c = catalog();
    let d = data(&c);
    let p = c.prepare("SELECT p.id AS person, t.title AS team FROM people AS p LEFT JOIN teams t ON p.team = t.id WHERE p.id >= :minimum AND (p.active OR t.title IS NULL)",
        &[ParamDef { name:"minimum".into(),ty:ScalarType::I64,nullable:false }],Limits::default()).unwrap();
    let params = BTreeMap::from([("minimum".into(), Value::I64(2))]);
    assert_eq!(p.output()[0].name, "person");
    assert!(!p.output()[0].nullable);
    assert!(p.output()[1].nullable);
    let expected = vec![
        vec![Value::I64(2), Value::Null],
        vec![Value::I64(3), Value::Null],
    ];
    assert_eq!(p.execute(&c, &d, &params).unwrap().rows, expected);
    assert_eq!(
        p.optimized().execute(&c, &d, &params).unwrap().rows,
        expected
    );
    assert!(p.optimized().plan() != p.plan());
    assert_eq!(
        p.execute(&c, &d, &BTreeMap::new()).unwrap_err().kind,
        ErrorKind::Data
    );
    assert_eq!(
        p.execute(
            &c,
            &d,
            &BTreeMap::from([("minimum".into(), Value::Text("2".into()))])
        )
        .unwrap_err()
        .kind,
        ErrorKind::Type
    );
    check(
        "select id from people where name = 'O''Neil' or name = 'Béa'",
        vec![vec![Value::I64(1)], vec![Value::I64(2)]],
    );
}
#[test]
fn outer_join_null_extension_and_filter_placement() {
    check(
        "SELECT p.id AS person,t.id AS team FROM people p FULL OUTER JOIN teams t ON p.team = t.id",
        vec![
            vec![Value::I64(1), Value::I64(10)],
            vec![Value::I64(2), Value::Null],
            vec![Value::I64(3), Value::Null],
            vec![Value::Null, Value::I64(30)],
        ],
    );
    check(
        "SELECT p.id AS person,t.id AS team FROM people p RIGHT JOIN teams t ON p.team=t.id",
        vec![
            vec![Value::I64(1), Value::I64(10)],
            vec![Value::Null, Value::I64(30)],
        ],
    );
    check(
        "SELECT p.id FROM people p LEFT JOIN teams t ON p.team=t.id WHERE t.id IS NULL",
        vec![vec![Value::I64(2)], vec![Value::I64(3)]],
    );
    check(
        "SELECT p.id FROM people p LEFT JOIN teams t ON p.team=t.id WHERE t.id = 10",
        vec![vec![Value::I64(1)]],
    );
    let c = catalog();
    for (join, left, right) in [
        ("INNER", false, false),
        ("LEFT", false, true),
        ("RIGHT", true, false),
        ("FULL", true, true),
    ] {
        let p = c
            .prepare(
                &format!(
                    "SELECT p.id AS p,t.id AS t FROM people p {join} JOIN teams t ON p.team=t.id"
                ),
                &[],
                Limits::default(),
            )
            .unwrap();
        assert_eq!(p.output()[0].nullable, left);
        assert_eq!(p.output()[1].nullable, right);
    }
    let p = c
        .prepare(
            "SELECT p.id FROM people p LEFT JOIN teams t ON p.team=t.id WHERE t.id IS NULL",
            &[],
            Limits::default(),
        )
        .unwrap()
        .optimized();
    assert!(
        matches!(p.plan(),RelPlan::Filter { input, .. } if matches!(input.as_ref(),RelPlan::Join {kind:JoinKind::Left,..}))
    );
}
#[test]
fn self_joins_have_distinct_bound_identities_and_ast_dependencies() {
    let c = catalog();
    let d = data(&c);
    let p = c.prepare("SELECT a.id AS first,b.id AS second FROM people a JOIN people b ON a.id=b.id WHERE a.id=2",&[],Limits::default()).unwrap();
    assert_eq!(p.columns()[0].binding, 0);
    assert_eq!(p.columns()[4].binding, 1);
    if let RelPlan::Filter { predicate, .. } = p.plan() {
        assert_eq!(
            predicate.dependencies().into_iter().collect::<Vec<_>>(),
            vec![0]
        );
    } else {
        panic!("filter missing");
    }
    assert_eq!(
        p.optimized()
            .execute(&c, &d, &BTreeMap::new())
            .unwrap()
            .rows,
        vec![vec![Value::I64(2), Value::I64(2)]]
    );
    error(
        "select id from people a join people b on a.id=b.id",
        ErrorKind::Ambiguous,
    );
    error("select people.id from people p", ErrorKind::Unknown);
    error(
        "select a.id from people a join people a on a.id=a.id",
        ErrorKind::Duplicate,
    );
    error(
        "select a.id,b.id from people a join people b on a.id=b.id",
        ErrorKind::Duplicate,
    );
}
#[test]
fn sql_null_three_valued_logic_exhaustive() {
    let mut c = HermeticCatalog::default();
    c.apply_ddl("CREATE TABLE truth (a BOOL,b BOOL)", Limits::default())
        .unwrap();
    let vals = [Some(false), Some(true), None];
    let mut rows = Vec::new();
    for a in vals {
        for b in vals {
            rows.push(vec![
                a.map_or(Value::Null, Value::Bool),
                b.map_or(Value::Null, Value::Bool),
            ]);
        }
    }
    let mut d = DataSet::new(&c);
    d.set_rows("truth", rows, Limits::default()).unwrap();
    let p = c.prepare("SELECT a AND b AS conjunction,a OR b AS disjunction,NOT a AS negation,a=b AS equality,a IS NULL AS absent FROM truth",&[],Limits::default()).unwrap();
    let mut expected = Vec::new();
    for a in vals {
        for b in vals {
            let and = if a == Some(false) || b == Some(false) {
                Some(false)
            } else if a == Some(true) && b == Some(true) {
                Some(true)
            } else {
                None
            };
            let or = if a == Some(true) || b == Some(true) {
                Some(true)
            } else if a == Some(false) && b == Some(false) {
                Some(false)
            } else {
                None
            };
            expected.push(vec![
                and.map_or(Value::Null, Value::Bool),
                or.map_or(Value::Null, Value::Bool),
                a.map(|x| !x).map_or(Value::Null, Value::Bool),
                a.zip(b)
                    .map(|(x, y)| x == y)
                    .map_or(Value::Null, Value::Bool),
                Value::Bool(a.is_none()),
            ]);
        }
    }
    assert_eq!(p.execute(&c, &d, &BTreeMap::new()).unwrap().rows, expected);
    check(
        "SELECT id FROM people WHERE NOT active OR active IS NULL",
        vec![vec![Value::I64(2)], vec![Value::I64(3)]],
    );
    check("SELECT id FROM people WHERE team=NULL", vec![]);
}
#[test]
fn outer_join_pushdown_differential_for_many_predicates_and_null_inputs() {
    let mut c = HermeticCatalog::default();
    c.apply_ddl(
        "CREATE TABLE l (id I64); CREATE TABLE r (id I64)",
        Limits::default(),
    )
    .unwrap();
    let candidates = [Value::Null, Value::I64(0), Value::I64(1)];
    let predicates = [
        "a.id=0",
        "b.id=0",
        "a.id IS NULL",
        "b.id IS NULL",
        "a.id IS NOT NULL",
        "b.id IS NOT NULL",
        "a.id=0 AND b.id=0",
        "a.id=0 OR b.id IS NULL",
        "NOT (a.id=b.id)",
        "NULL OR a.id=0",
        "FALSE",
        "TRUE",
        "NULL",
        "(a.id=0 AND b.id=0) OR b.id IS NULL",
    ];
    // All 64 subsets of these small nullable inputs, for every join and predicate.
    for mask in 0..64 {
        let mut d = DataSet::new(&c);
        for (table, shift) in [("l", 0), ("r", 3)] {
            let rows = candidates
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << (i + shift)) != 0)
                .map(|(_, v)| vec![v.clone()])
                .collect();
            d.set_rows(table, rows, Limits::default()).unwrap();
        }
        for kind in ["INNER", "LEFT", "RIGHT", "FULL"] {
            for predicate in predicates {
                let sql = format!("SELECT a.id AS a,b.id AS b FROM l a {kind} JOIN r b ON a.id=b.id WHERE {predicate}");
                let p = c.prepare(&sql, &[], Limits::default()).unwrap();
                let before = p.execute(&c, &d, &BTreeMap::new()).unwrap().rows;
                let after = p
                    .optimized()
                    .execute(&c, &d, &BTreeMap::new())
                    .unwrap()
                    .rows;
                assert_eq!(before, after, "mask={mask}: {sql}");
            }
        }
    }
}
#[test]
fn ddl_atomic_deterministic_and_stale_revision_or_content_rejected() {
    let mut c = catalog();
    let old = c.clone();
    assert_eq!(
        c.apply_ddl(
            "CREATE TABLE x (id I64); CREATE TABLE people (id I64)",
            Limits::default()
        )
        .unwrap_err()
        .kind,
        ErrorKind::Duplicate
    );
    assert_eq!(c, old);
    assert_eq!(
        c.apply_ddl("CREATE TABLE dup (x I64,X TEXT)", Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Duplicate
    );
    assert_eq!(c, old);
    let p = c
        .prepare("SELECT id FROM people", &[], Limits::default())
        .unwrap();
    let d = data(&c);
    c.apply_ddl(
        "DROP TABLE teams; CREATE TABLE teams (id I64 NOT NULL,title TEXT)",
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        p.execute(&c, &d, &BTreeMap::new()).unwrap_err().kind,
        ErrorKind::Stale
    );
    let p2 = c
        .prepare("SELECT id FROM people", &[], Limits::default())
        .unwrap();
    assert_eq!(
        p2.execute(&c, &d, &BTreeMap::new()).unwrap_err().kind,
        ErrorKind::Stale
    );
    let mut different = HermeticCatalog::default();
    different
        .apply_ddl(
            "CREATE TABLE people (id TEXT); CREATE TABLE teams (id I64)",
            Limits::default(),
        )
        .unwrap();
    assert_eq!(different.revision(), old.revision());
    assert_eq!(
        p.execute(&different, &d, &BTreeMap::new())
            .unwrap_err()
            .kind,
        ErrorKind::Stale
    );
    assert_eq!(
        old.tables().keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["people", "teams"]
    );
}
#[test]
fn unsupported_syntax_and_type_errors_are_explicit() {
    for sql in [
        "select count(*) from people",
        "select id from people order by id",
        "select id from people;select id from teams",
        "select id from people -- hi",
        "select id from people where id+1=2",
        "select \"id\" from people",
        "select id from people cross join teams",
        "select id from people where id IN (1,2)",
        "select id from people where id=9223372036854775808",
    ] {
        error(sql, ErrorKind::Syntax);
    }
    error("select NULL AS n from people", ErrorKind::Type);
    error("select id from people where id='1'", ErrorKind::Type);
    error("select id from people where id", ErrorKind::Type);
    error("select id from people where :missing=1", ErrorKind::Unknown);
    error("select id from people where active AND id", ErrorKind::Type);
    error("select id from missing", ErrorKind::Unknown);
    error("select missing from people", ErrorKind::Unknown);
    let defs = [ParamDef {
        name: "p".into(),
        ty: ScalarType::I64,
        nullable: false,
    }];
    assert_eq!(
        catalog()
            .prepare("SELECT id FROM people", &defs, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Unknown
    );
    assert_eq!(
        catalog()
            .prepare(
                "SELECT id FROM people WHERE id=:p",
                &[defs[0].clone(), defs[0].clone()],
                Limits::default()
            )
            .unwrap_err()
            .kind,
        ErrorKind::Duplicate
    );
}
#[test]
fn data_rejection_is_atomic_and_empty_inputs_explicit() {
    let c = catalog();
    let mut d = data(&c);
    let old = d.tables().clone();
    for invalid in [
        vec![vec![Value::I64(4)]],
        vec![vec![
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Text("x".into()),
        ]],
        vec![vec![
            Value::Text("wrong".into()),
            Value::Null,
            Value::Null,
            Value::Text("x".into()),
        ]],
    ] {
        assert_eq!(
            d.set_rows("people", invalid, Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::Data
        );
        assert_eq!(d.tables(), &old);
    }
    let p = c
        .prepare("select id from people", &[], Limits::default())
        .unwrap();
    assert_eq!(
        p.execute(&c, &DataSet::new(&c), &BTreeMap::new())
            .unwrap_err()
            .kind,
        ErrorKind::Data
    );
    d.set_rows("people", vec![], Limits::default()).unwrap();
    assert!(p.execute(&c, &d, &BTreeMap::new()).unwrap().rows.is_empty());
}
#[test]
fn all_scalar_types_and_finite_float_policy() {
    let mut c = HermeticCatalog::default();
    c.apply_ddl(
        "CREATE TABLE scalars (u U64 NOT NULL,f F64 NOT NULL,b BYTES NOT NULL)",
        Limits::default(),
    )
    .unwrap();
    let mut d = DataSet::new(&c);
    assert_eq!(
        d.set_rows(
            "scalars",
            vec![vec![
                Value::U64(u64::MAX),
                Value::F64(f64::NAN),
                Value::Bytes(vec![])
            ]],
            Limits::default()
        )
        .unwrap_err()
        .kind,
        ErrorKind::Data
    );
    d.set_rows(
        "scalars",
        vec![vec![
            Value::U64(u64::MAX),
            Value::F64(-0.0),
            Value::Bytes(vec![0, 255]),
        ]],
        Limits::default(),
    )
    .unwrap();
    let defs = [
        ParamDef {
            name: "u".into(),
            ty: ScalarType::U64,
            nullable: false,
        },
        ParamDef {
            name: "f".into(),
            ty: ScalarType::F64,
            nullable: false,
        },
        ParamDef {
            name: "b".into(),
            ty: ScalarType::Bytes,
            nullable: true,
        },
    ];
    let p = c
        .prepare(
            "SELECT u,f,b FROM scalars WHERE u=:u AND f=:f AND b=:b",
            &defs,
            Limits::default(),
        )
        .unwrap();
    let mut params = BTreeMap::from([
        ("u".into(), Value::U64(u64::MAX)),
        ("f".into(), Value::F64(0.0)),
        ("b".into(), Value::Bytes(vec![0, 255])),
    ]);
    assert_eq!(p.execute(&c, &d, &params).unwrap().rows.len(), 1);
    params.insert("b".into(), Value::Null);
    assert!(p.execute(&c, &d, &params).unwrap().rows.is_empty());
    params.insert("f".into(), Value::F64(f64::INFINITY));
    assert_eq!(
        p.execute(&c, &d, &params).unwrap_err().kind,
        ErrorKind::Type
    );
}
#[test]
fn limits_fail_closed_at_parse_catalog_and_execution() {
    let c = catalog();
    let d = data(&c);
    let sql = "SELECT id FROM people";
    for limits in [
        Limits {
            sql_bytes: sql.len() - 1,
            ..Limits::default()
        },
        Limits {
            tokens: 3,
            ..Limits::default()
        },
        Limits {
            nodes: 0,
            ..Limits::default()
        },
        Limits {
            relations: 0,
            ..Limits::default()
        },
        Limits {
            tables: 1,
            ..Limits::default()
        },
        Limits {
            columns_per_table: 1,
            ..Limits::default()
        },
        Limits {
            work: usize::MAX,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            c.prepare(sql, &[], limits).unwrap_err().kind,
            ErrorKind::Limit
        );
    }
    assert_eq!(
        c.prepare(
            "SELECT id FROM people WHERE (((id=1)))",
            &[],
            Limits {
                nesting: 2,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
    for limits in [
        Limits {
            input_rows: 2,
            ..Limits::default()
        },
        Limits {
            input_bytes: 1,
            ..Limits::default()
        },
        Limits {
            work: 2,
            ..Limits::default()
        },
        Limits {
            intermediate_rows: 1,
            ..Limits::default()
        },
        Limits {
            intermediate_bytes: 1,
            ..Limits::default()
        },
        Limits {
            output_rows: 1,
            ..Limits::default()
        },
        Limits {
            output_bytes: 1,
            ..Limits::default()
        },
    ] {
        let p = c.prepare(sql, &[], limits).unwrap();
        assert_eq!(
            p.execute(&c, &d, &BTreeMap::new()).unwrap_err().kind,
            ErrorKind::Limit,
            "{limits:?}"
        );
    }
    let mut c2 = HermeticCatalog::default();
    let old = c2.clone();
    assert_eq!(
        c2.apply_ddl(
            "CREATE TABLE x(id I64);CREATE TABLE y(id I64)",
            Limits {
                tables: 1,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
    assert_eq!(c2, old);
    assert_eq!(
        c2.apply_ddl(
            "CREATE TABLE x(id I64,a BOOL)",
            Limits {
                columns_per_table: 1,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
}

#[test]
fn nested_outer_joins_preserve_prior_nullable_columns() {
    let mut c = catalog();
    c.apply_ddl(
        "CREATE TABLE labels (id I64 NOT NULL,label TEXT NOT NULL)",
        Limits::default(),
    )
    .unwrap();
    let mut d = data(&c);
    d.set_rows(
        "labels",
        vec![
            vec![Value::I64(30), Value::Text("thirty".into())],
            vec![Value::I64(40), Value::Text("forty".into())],
        ],
        Limits::default(),
    )
    .unwrap();
    let p=c.prepare("SELECT p.id AS person,t.id AS team,x.label FROM people p LEFT JOIN teams t ON p.team=t.id FULL JOIN labels x ON t.id=x.id WHERE p.id IS NULL OR p.id>=2",&[],Limits::default()).unwrap();
    assert!(p.output().iter().all(|c| c.nullable));
    let expected = vec![
        vec![Value::I64(2), Value::Null, Value::Null],
        vec![Value::I64(3), Value::Null, Value::Null],
        vec![Value::Null, Value::Null, Value::Text("thirty".into())],
        vec![Value::Null, Value::Null, Value::Text("forty".into())],
    ];
    assert_eq!(p.execute(&c, &d, &BTreeMap::new()).unwrap().rows, expected);
    assert_eq!(
        p.optimized()
            .execute(&c, &d, &BTreeMap::new())
            .unwrap()
            .rows,
        expected
    );
    let query=c.prepare("SELECT p.id FROM people p LEFT JOIN teams t ON p.team=t.id LEFT JOIN labels x ON t.id=x.id WHERE p.id>=2 AND x.label IS NULL",&[],Limits::default()).unwrap();
    assert_eq!(
        query.execute(&c, &d, &BTreeMap::new()).unwrap().rows,
        query
            .optimized()
            .execute(&c, &d, &BTreeMap::new())
            .unwrap()
            .rows
    );
}
#[test]
fn joined_duplicates_are_bag_rows_and_null_never_equals_null() {
    let mut c = HermeticCatalog::default();
    c.apply_ddl(
        "CREATE TABLE a (x I64);CREATE TABLE b (x I64)",
        Limits::default(),
    )
    .unwrap();
    let mut d = DataSet::new(&c);
    d.set_rows(
        "a",
        vec![vec![Value::I64(1)], vec![Value::I64(1)], vec![Value::Null]],
        Limits::default(),
    )
    .unwrap();
    d.set_rows(
        "b",
        vec![vec![Value::I64(1)], vec![Value::I64(1)], vec![Value::Null]],
        Limits::default(),
    )
    .unwrap();
    let p = c
        .prepare(
            "SELECT a.x AS a,b.x AS b FROM a FULL JOIN b ON a.x=b.x",
            &[],
            Limits::default(),
        )
        .unwrap();
    let rows = p.execute(&c, &d, &BTreeMap::new()).unwrap().rows;
    assert_eq!(rows.len(), 6);
    assert_eq!(rows.iter().filter(|r| r[0] == Value::I64(1)).count(), 4);
    assert_eq!(
        rows.iter()
            .filter(|r| r[0] == Value::Null && r[1] == Value::Null)
            .count(),
        2
    );
}
#[test]
fn cartesian_work_is_bounded_even_when_on_matches_nothing() {
    let mut c = HermeticCatalog::default();
    c.apply_ddl(
        "CREATE TABLE a (x I64);CREATE TABLE b (x I64)",
        Limits::default(),
    )
    .unwrap();
    let mut d = DataSet::new(&c);
    for name in ["a", "b"] {
        d.set_rows(
            name,
            (0..50).map(|x| vec![Value::I64(x)]).collect(),
            Limits::default(),
        )
        .unwrap();
    }
    let p = c
        .prepare(
            "SELECT a.x FROM a JOIN b ON FALSE",
            &[],
            Limits {
                work: 10_000,
                ..Limits::default()
            },
        )
        .unwrap();
    assert_eq!(
        p.execute(&c, &d, &BTreeMap::new()).unwrap_err().kind,
        ErrorKind::Limit
    );
}

#[test]
fn star_projection_bind_nullability_and_exact_sql_boundaries() {
    let c = catalog();
    let d = data(&c);
    let sql = "SELECT * FROM people";
    let p = c
        .prepare(
            sql,
            &[],
            Limits {
                sql_bytes: sql.len(),
                tokens: 4,
                ..Limits::default()
            },
        )
        .unwrap();
    assert_eq!(
        p.output()
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        vec!["id", "team", "active", "name"]
    );
    assert_eq!(
        p.execute(&c, &d, &BTreeMap::new()).unwrap().rows,
        d.tables()["people"]
    );
    let p = c
        .prepare(
            "SELECT :p AS value FROM people WHERE id=1",
            &[ParamDef {
                name: "p".into(),
                ty: ScalarType::Text,
                nullable: true,
            }],
            Limits::default(),
        )
        .unwrap();
    assert!(p.output()[0].nullable);
    assert_eq!(
        p.execute(&c, &d, &BTreeMap::from([("p".into(), Value::Null)]))
            .unwrap()
            .rows,
        vec![vec![Value::Null]]
    );
    assert_eq!(
        p.execute(
            &c,
            &d,
            &BTreeMap::from([("p".into(), Value::Null), ("extra".into(), Value::I64(0))])
        )
        .unwrap_err()
        .kind,
        ErrorKind::Data
    );
}

#[test]
fn malformed_and_excessive_inputs_fail_without_partial_catalog_changes() {
    let malformed = [
        "CREATE TABLE empty ()",
        "CREATE TABLE a (x I64 NOT NULL NULL)",
        "CREATE TABLE a (x I64 PRIMARY KEY)",
        "CREATE TABLE a (x VARCHAR(20))",
        "DROP TABLE absent",
        "CREATE TABLE a (x TEXT DEFAULT 'x')",
    ];
    for sql in malformed {
        let mut c = catalog();
        let before = c.clone();
        assert!(c.apply_ddl(sql, Limits::default()).is_err(), "{sql}");
        assert_eq!(c, before);
    }
    for sql in [
        "SELECT name FROM people WHERE name='unterminated",
        "SELECT id FROM people WHERE : =1",
        "SELECT id FROM people WHERE id=--1",
        "SELECT id FROM people WHERE id=1 != 2",
        "SELECT id FROM people WHERE (((id=1)",
        "SELECT id FROM people WHERE NOT",
        "SELECT id FROM people WHERE name='日本語' trailing",
    ] {
        error(sql, ErrorKind::Syntax);
    }
    let c = catalog();
    let mut d = data(&c);
    let before = d.tables().clone();
    assert_eq!(
        d.set_rows(
            "people",
            vec![vec![]; 11],
            Limits {
                input_rows: 10,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
    assert_eq!(d.tables(), &before);
    assert_eq!(
        d.set_rows(
            "people",
            vec![vec![
                Value::I64(1),
                Value::Null,
                Value::Null,
                Value::Text("large".repeat(100))
            ]],
            Limits {
                input_bytes: 8,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
    assert_eq!(d.tables(), &before);
    let sql = format!("SELECT id FROM people WHERE {}TRUE", "NOT ".repeat(33));
    assert_eq!(
        c.prepare(&sql, &[], Limits::default()).unwrap_err().kind,
        ErrorKind::Limit
    );
    let sql = format!(
        "SELECT id FROM people WHERE {}",
        vec!["TRUE"; 260].join(" OR ")
    );
    assert_eq!(
        c.prepare(&sql, &[], Limits::default()).unwrap_err().kind,
        ErrorKind::Limit
    );
}

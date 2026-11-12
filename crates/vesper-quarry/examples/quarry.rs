use std::collections::BTreeMap;
use vesper_quarry::{DataSet, HermeticCatalog, Limits, ParamDef, ScalarType, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let limits = Limits::default();
    let mut catalog = HermeticCatalog::default();
    catalog.apply_ddl(
        "CREATE TABLE orders (id I64 NOT NULL, org I64 NOT NULL, total I64 NOT NULL, customer I64);\n         CREATE TABLE customers (id I64 NOT NULL, name TEXT NOT NULL);",
        limits,
    )?;
    let query = catalog.prepare(
        "SELECT o.id AS order_id,o.total,c.name AS customer FROM orders o\n         LEFT JOIN customers c ON o.customer=c.id\n         WHERE o.org=:org AND o.total>=:minimum",
        &[
            ParamDef { name: "org".into(), ty: ScalarType::I64, nullable: false },
            ParamDef { name: "minimum".into(), ty: ScalarType::I64, nullable: false },
        ],
        limits,
    )?;
    let mut data = DataSet::new(&catalog);
    data.set_rows(
        "orders",
        vec![
            vec![
                Value::I64(1),
                Value::I64(7),
                Value::I64(500),
                Value::I64(10),
            ],
            vec![Value::I64(2), Value::I64(7), Value::I64(900), Value::Null],
            vec![
                Value::I64(3),
                Value::I64(8),
                Value::I64(999),
                Value::I64(10),
            ],
        ],
        limits,
    )?;
    data.set_rows(
        "customers",
        vec![vec![Value::I64(10), Value::Text("Ada".into())]],
        limits,
    )?;
    let binds = BTreeMap::from([
        ("org".into(), Value::I64(7)),
        ("minimum".into(), Value::I64(400)),
    ]);
    let before = query.execute(&catalog, &data, &binds)?;
    let optimized = query.optimized();
    let after = optimized.execute(&catalog, &data, &binds)?;
    assert_eq!(before.rows, after.rows);
    assert_eq!(
        after.rows,
        vec![
            vec![Value::I64(1), Value::I64(500), Value::Text("Ada".into())],
            vec![Value::I64(2), Value::I64(900), Value::Null]
        ]
    );
    assert!(after.columns[2].nullable);
    assert_ne!(query.plan(), optimized.plan());
    println!(
        "Quarry: {} rows, revision {}, work {} -> {}; outer NULL and tenant bind checks passed",
        after.rows.len(),
        catalog.revision(),
        before.work,
        after.work
    );
    for row in after.rows {
        println!("{row:?}");
    }
    Ok(())
}

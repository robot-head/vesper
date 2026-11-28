//! The type-directed JSON value codec against a checked world from
//! `vesper sim-host`.
use serde_json::json;
use vesper_sim_session::{
    CheckedProgram, Type, Value, World,
    value::{decode, encode},
};

fn world() -> World {
    CheckedProgram::new(
        "copy struct Point { x: i64, y: i64 } copy enum Shape { Dot, Empty {}, Line(Point, Point), Named { label: String } } linear effect E { fn f(p: Option[(u8, Shape)]) -> bool; }",
    )
    .unwrap_or_else(|e| panic!("{e}"))
    .world()
    .clone()
}

#[test]
fn round_trips_nested_generic_values() {
    let world = world();
    let ty = world.effects["E"].ops[0].params[0].ty.clone();
    for json in [
        json!("None"),
        json!({"Some": [255, "Dot"]}),
        json!({"Some": [7, "Empty"]}),
        json!({"Some": [1, {"Line": [{"x": -1, "y": 2}, {"x": 3, "y": 4}]}]}),
        json!({"Some": [0, {"Named": {"label": "a"}}]}),
    ] {
        let value = decode(&world, &ty, &json).unwrap();
        assert_eq!(encode(&world, &ty, &value).unwrap(), json);
    }
}

#[test]
fn rejects_shape_and_range_errors() {
    let world = world();
    let ty = world.effects["E"].ops[0].params[0].ty.clone();
    for json in [
        json!({"Some": [256, "Dot"]}),
        json!({"Some": [1]}),
        json!({"Some": [1, "Square"]}),
        json!({"Some": [1, {"Dot": null}]}),
        json!({"Some": [1, {"Named": {"label": "a", "extra": 1}}]}),
        json!({"Some": [1, {"Line": [{"x": 1}, {"x": 3, "y": 4}]}]}),
        json!("Some"),
        json!(null),
    ] {
        assert!(decode(&world, &ty, &json).is_err(), "{json}");
    }
    assert!(decode(&world, &Type::named("Never"), &json!(null)).is_err());
    assert!(decode(&world, &Type::named("Unknown"), &json!(1)).is_err());
    assert!(encode(&world, &Type::named("u32"), &Value::UInt(1, 64)).is_err());
    assert!(encode(&world, &Type::named("u64"), &Value::Opaque("<fn f>".into())).is_err());
}

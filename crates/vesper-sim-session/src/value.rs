//! Type-directed conversion between scenario JSON and reference-evaluator values.
//!
//! Every host-supplied value is decoded against the checked operation signature
//! before it can reach a continuation. Unsupported types (functions, rows,
//! `Never`, unresolved generics) are rejected rather than passed through.
use serde_json::{Map, Number, Value as Json};
use std::collections::BTreeMap;
use vesper::{
    ast::{Type, substitute},
    check::World,
    runtime::Value,
};

const MAX_DEPTH: usize = 64;

/// Canonical display of a checked type, used to compare declared signatures.
pub fn type_name(ty: &Type) -> String {
    if is_unit(ty) {
        "()".into()
    } else {
        ty.to_string()
    }
}

pub(crate) fn is_unit(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(items) if items.is_empty()) || ty.name() == "Unit"
}

pub fn unit() -> Value {
    Value::Tuple(vec![])
}

/// Decode `json` as a value of `ty`, rejecting any shape mismatch.
pub fn decode(world: &World, ty: &Type, json: &Json) -> Result<Value, String> {
    decode_at(world, ty, json, 0)
}

fn decode_at(world: &World, ty: &Type, json: &Json, depth: usize) -> Result<Value, String> {
    if depth > MAX_DEPTH {
        return Err("value nesting exceeds the scenario limit".into());
    }
    let mismatch = || format!("expected a value of type {}, found {json}", type_name(ty));
    if is_unit(ty) {
        return if json.is_null() {
            Ok(unit())
        } else {
            Err(mismatch())
        };
    }
    match ty {
        Type::Tuple(items) => {
            let values = json.as_array().ok_or_else(mismatch)?;
            if values.len() != items.len() {
                return Err(mismatch());
            }
            items
                .iter()
                .zip(values)
                .map(|(t, v)| decode_at(world, t, v, depth + 1))
                .collect::<Result<_, _>>()
                .map(Value::Tuple)
        }
        Type::Named(name, args) => match name.as_str() {
            "u8" | "u32" | "u64" => {
                let n = json.as_u64().ok_or_else(mismatch)?;
                let bits = match name.as_str() {
                    "u8" => 8,
                    "u32" => 32,
                    _ => 64,
                };
                if bits < 64 && n >> bits != 0 {
                    return Err(format!("{n} is out of range for {name}"));
                }
                Ok(Value::UInt(n, bits))
            }
            "i64" => json.as_i64().map(Value::Int).ok_or_else(mismatch),
            "f64" => json.as_f64().map(Value::Float).ok_or_else(mismatch),
            "bool" => json.as_bool().map(Value::Bool).ok_or_else(mismatch),
            "String" => json
                .as_str()
                .map(|s| Value::String(s.into()))
                .ok_or_else(mismatch),
            "Never" => Err("no value inhabits Never".into()),
            _ => {
                let adt = world
                    .adts
                    .get(name)
                    .ok_or_else(|| format!("unsupported type {name} in a host value"))?;
                if adt.generics.len() != args.len() {
                    return Err(format!("type {} has unresolved generics", type_name(ty)));
                }
                let vars: BTreeMap<_, _> = adt
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                let (variant, payload) = if adt.is_enum {
                    match json {
                        Json::String(tag) => (tag.as_str(), None),
                        Json::Object(map) if map.len() == 1 => {
                            let (tag, payload) = map.iter().next().unwrap();
                            (tag.as_str(), Some(payload))
                        }
                        _ => return Err(mismatch()),
                    }
                } else {
                    (adt.variants[0].name.as_str(), Some(json))
                };
                let variant = adt
                    .variants
                    .iter()
                    .find(|v| v.name == variant)
                    .ok_or_else(|| format!("type {name} has no constructor {variant}"))?;
                let mut fields = BTreeMap::new();
                let field_ty = |i: usize| substitute(&variant.fields[i].ty, &vars);
                match (variant.fields.len(), payload) {
                    (0, None) => {}
                    (0, Some(_)) if adt.is_enum => {
                        return Err(format!("constructor {} takes no payload", variant.name));
                    }
                    (0, Some(Json::Object(map))) if map.is_empty() => {}
                    (_, None) => {
                        return Err(format!("constructor {} needs a payload", variant.name));
                    }
                    (n, Some(payload)) if variant.named => {
                        let map = payload.as_object().ok_or_else(mismatch)?;
                        if map.len() != n {
                            return Err(mismatch());
                        }
                        for (i, field) in variant.fields.iter().enumerate() {
                            let v = map
                                .get(&field.name)
                                .ok_or_else(|| format!("missing field {}", field.name))?;
                            fields.insert(
                                field.name.clone(),
                                decode_at(world, &field_ty(i), v, depth + 1)?,
                            );
                        }
                    }
                    (1, Some(payload)) => {
                        let v = decode_at(world, &field_ty(0), payload, depth + 1)?;
                        fields.insert(variant.fields[0].name.clone(), v);
                    }
                    (n, Some(payload)) => {
                        let items = payload.as_array().ok_or_else(mismatch)?;
                        if items.len() != n {
                            return Err(mismatch());
                        }
                        for (i, (field, v)) in variant.fields.iter().zip(items).enumerate() {
                            fields.insert(
                                field.name.clone(),
                                decode_at(world, &field_ty(i), v, depth + 1)?,
                            );
                        }
                    }
                }
                Ok(Value::Data {
                    ty: adt.name.clone(),
                    constructor: variant.name.clone(),
                    fields,
                })
            }
        },
        _ => Err(format!(
            "unsupported type {} in a host value",
            type_name(ty)
        )),
    }
}

/// Encode a runtime value of `ty` in the same canonical form `decode` reads.
/// A value that does not conform to the type is an error, never a lossy guess.
pub fn encode(world: &World, ty: &Type, value: &Value) -> Result<Json, String> {
    encode_at(world, ty, value, 0)
}

fn encode_at(world: &World, ty: &Type, value: &Value, depth: usize) -> Result<Json, String> {
    if depth > MAX_DEPTH {
        return Err("value nesting exceeds the scenario limit".into());
    }
    let mismatch = || {
        format!(
            "runtime value {value} does not conform to {}",
            type_name(ty)
        )
    };
    if is_unit(ty) {
        return match value {
            Value::Tuple(v) if v.is_empty() => Ok(Json::Null),
            _ => Err(mismatch()),
        };
    }
    match (ty, value) {
        (Type::Tuple(items), Value::Tuple(values)) if items.len() == values.len() => items
            .iter()
            .zip(values)
            .map(|(t, v)| encode_at(world, t, v, depth + 1))
            .collect::<Result<_, _>>()
            .map(Json::Array),
        (Type::Named(name, args), value) => match (name.as_str(), value) {
            ("u8", Value::UInt(n, 8))
            | ("u32", Value::UInt(n, 32))
            | ("u64", Value::UInt(n, 64)) => Ok(Json::from(*n)),
            ("i64", Value::Int(n)) => Ok(Json::from(*n)),
            ("f64", Value::Float(f)) => Number::from_f64(*f)
                .map(Json::Number)
                .ok_or_else(|| "non-finite floats are not representable".into()),
            ("bool", Value::Bool(b)) => Ok(Json::Bool(*b)),
            ("String", Value::String(s)) => Ok(Json::String(s.clone())),
            (
                _,
                Value::Data {
                    ty: data_ty,
                    constructor,
                    fields,
                },
            ) if data_ty == name => {
                let adt = world.adts.get(name).ok_or_else(mismatch)?;
                if adt.generics.len() != args.len() {
                    return Err(mismatch());
                }
                let vars: BTreeMap<_, _> = adt
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                let variant = adt
                    .variants
                    .iter()
                    .find(|v| &v.name == constructor)
                    .ok_or_else(mismatch)?;
                if fields.len() != variant.fields.len() {
                    return Err(mismatch());
                }
                let mut encoded = vec![];
                for field in &variant.fields {
                    let v = fields.get(&field.name).ok_or_else(mismatch)?;
                    encoded.push((
                        field.name.clone(),
                        encode_at(world, &substitute(&field.ty, &vars), v, depth + 1)?,
                    ));
                }
                let payload = if variant.named {
                    Some(Json::Object(encoded.into_iter().collect::<Map<_, _>>()))
                } else if encoded.len() == 1 {
                    encoded.pop().map(|(_, v)| v)
                } else if encoded.is_empty() {
                    None
                } else {
                    Some(Json::Array(encoded.into_iter().map(|(_, v)| v).collect()))
                };
                Ok(if adt.is_enum {
                    match payload {
                        None => Json::String(constructor.clone()),
                        Some(p) => Json::Object([(constructor.clone(), p)].into_iter().collect()),
                    }
                } else {
                    payload.unwrap_or_else(|| Json::Object(Map::new()))
                })
            }
            _ => Err(mismatch()),
        },
        _ => Err(mismatch()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn world() -> World {
        let parsed = vesper::parser::parse(
            "copy struct Point { x: i64, y: i64 } copy enum Shape { Dot, Line(Point, Point), Named { label: String } } linear effect E { fn f(p: Option[(u8, Shape)]) -> bool; }",
        );
        assert!(parsed.diagnostics.is_empty());
        let checked = vesper::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        checked.world
    }

    #[test]
    fn round_trips_nested_generic_values() {
        let world = world();
        let ty = world.effects["E"].ops[0].params[0].ty.clone();
        for json in [
            json!("None"),
            json!({"Some": [255, "Dot"]}),
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
        assert!(encode(&world, &Type::named("u64"), &Value::Function("f".into())).is_err());
    }
}

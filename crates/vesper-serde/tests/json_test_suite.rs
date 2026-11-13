//! Parsed values against `serde_json`, on the JSONTestSuite parsing corpus and
//! on random documents, for the scalar parser and every stage 1 path.
mod common;

use common::{Rng, document};
use serde_json::Value;
use std::path::Path;
use vesper_serde::simd::Isa;
use vesper_serde::{ErrorKind, FloatLimits, IndexedJson, Kind, Limits, Node};

/// Our view of a node as a serde_json value; `None` if a number has no finite
/// binary64 value (serde_json rejects those too).
fn to_serde(node: Node<'_, '_>) -> Option<Value> {
    Some(match node.kind() {
        Kind::Null => Value::Null,
        Kind::Bool => Value::Bool(node.as_bool().unwrap()),
        Kind::Number => {
            if let Ok(n) = node.as_u64() {
                Value::from(n)
            } else if let Ok(n) = node.as_i64() {
                Value::from(n)
            } else {
                let f = node.as_f64(FloatLimits::default()).ok()?;
                Value::from(f)
            }
        }
        Kind::String => {
            let s = node.string().unwrap();
            let mut scratch = vec![0u8; s.decoded_len()];
            Value::String(s.decode(&mut scratch).unwrap().as_str().to_owned())
        }
        Kind::Array => {
            let mut out = Vec::new();
            let mut cursor = node.elements().unwrap();
            while let Ok(child) = cursor.read_raw() {
                out.push(to_serde(child)?);
            }
            Value::Array(out)
        }
        Kind::Object => {
            let mut map = serde_json::Map::new();
            for (k, v) in node.fields().unwrap() {
                let mut scratch = vec![0u8; k.decoded_len()];
                let key = k.decode(&mut scratch).unwrap().as_str().to_owned();
                map.insert(key, to_serde(v)?);
            }
            Value::Object(map)
        }
    })
}

/// Structural equality. Floats may differ by a few ulps: serde_json's default
/// float parser (without `float_roundtrip`) is not correctly rounded, while
/// ours is Rust's correctly rounded `str::parse`, checked separately.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            if x.is_f64() || y.is_f64() {
                let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
                x == y || (x.signum() == y.signum() && x.to_bits().abs_diff(y.to_bits()) <= 4)
            } else {
                x == y
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Accepted(Option<Value>),
    Rejected(ErrorKind),
}

/// Parse on every engine, insist they agree, and return the shared outcome.
fn ours(input: &[u8], limits: Limits) -> Outcome {
    let scalar = IndexedJson::parse(input, limits);
    for isa in Isa::available() {
        let simd = IndexedJson::parse_with_isa(input, limits, isa);
        match (&scalar, &simd) {
            (Ok(a), Ok(b)) => assert!(a.same_tape(b), "{isa:?}"),
            (Err(a), Err(b)) => assert_eq!(a.kind, b.kind, "{isa:?} on {input:?}"),
            _ => panic!("{isa:?} disagrees with scalar on {input:?}"),
        }
    }
    match scalar {
        Ok(doc) => Outcome::Accepted(to_serde(doc.root())),
        Err(e) => Outcome::Rejected(e.kind),
    }
}

fn percent_decode(stored: &str) -> String {
    let bytes = stored.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
            out.push(u8::from_str_radix(hex, 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn corpus() -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/JSONTestSuite/test_parsing");
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| {
            let path = entry.unwrap().path();
            // Stored percent-encoded (`+` and `#` are not portable in Bazel
            // runfiles); restore the upstream name.
            let name = percent_decode(&path.file_name().unwrap().to_string_lossy());
            (name, std::fs::read(&path).unwrap())
        })
        .collect();
    files.push((
        "n_structure_100000_opening_arrays.json".into(),
        b"[".repeat(100_000),
    ));
    let mut open = b"[{\"\":".repeat(50_000);
    open.push(b'\n');
    files.push(("n_structure_open_array_object.json".into(), open));
    files.sort();
    assert!(
        files
            .iter()
            .any(|(n, _)| n == "n_structure_trailing_#.json")
    );
    files
}

/// Valid JSON this crate rejects by policy: duplicate keys are ambiguous.
const POLICY_REJECTS: &[(&str, ErrorKind)] = &[
    ("y_object_duplicated_key.json", ErrorKind::DuplicateKey),
    (
        "y_object_duplicated_key_and_value.json",
        ErrorKind::DuplicateKey,
    ),
];

#[test]
fn json_test_suite_corpus() {
    let files = corpus();
    assert_eq!(files.len(), 318, "JSONTestSuite has 318 parsing cases");
    let limits = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    let (mut y, mut n, mut i_accept, mut i_reject) = (0, 0, 0, 0);
    for (name, bytes) in &files {
        let outcome = ours(bytes, limits);
        let serde = serde_json::from_slice::<Value>(bytes);
        match &name[..2] {
            "y_" => {
                if let Some((_, kind)) = POLICY_REJECTS.iter().find(|(n, _)| n == name) {
                    assert_eq!(outcome, Outcome::Rejected(*kind), "{name}");
                    continue;
                }
                let Outcome::Accepted(Some(value)) = outcome else {
                    panic!("{name}: must accept, got {outcome:?}");
                };
                let expected = serde.unwrap_or_else(|e| panic!("{name}: serde_json {e}"));
                assert!(same(&value, &expected), "{name}: {value} vs {expected}");
                y += 1;
            }
            "n_" => {
                assert!(
                    matches!(outcome, Outcome::Rejected(_)),
                    "{name}: must reject"
                );
                n += 1;
            }
            "i_" => match (outcome, serde) {
                (Outcome::Accepted(Some(v)), Ok(w)) => {
                    assert!(same(&v, &w), "{name}");
                    i_accept += 1;
                }
                (Outcome::Accepted(_), _) => i_accept += 1,
                (Outcome::Rejected(_), _) => i_reject += 1,
            },
            _ => panic!("unexpected corpus file {name}"),
        }
    }
    eprintln!(
        "JSONTestSuite: {y} y_ accepted and equal to serde_json, {} y_ rejected by duplicate-key policy, \
         {n} n_ rejected, i_ accepted {i_accept} / rejected {i_reject}",
        POLICY_REJECTS.len()
    );
    assert_eq!(y + POLICY_REJECTS.len(), 95);
    assert_eq!(n, 188);
}

#[test]
fn random_documents_equal_serde_json() {
    let mut rng = Rng(0xabad_1dea);
    for _ in 0..3000 {
        let doc = document(&mut rng);
        let Outcome::Accepted(value) = ours(&doc, Limits::default()) else {
            panic!(
                "generated document rejected: {:?}",
                String::from_utf8_lossy(&doc)
            );
        };
        let expected: Value = serde_json::from_slice(&doc).unwrap();
        let value = value.expect("generated numbers are finite");
        assert!(same(&value, &expected), "{value} vs {expected}");
    }
}

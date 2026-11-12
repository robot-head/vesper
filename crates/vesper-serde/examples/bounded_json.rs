//! Run: cargo run -p vesper-serde --example bounded_json
use vesper_serde::{EncodeLimits, IndexedJson, JsonStr, Limits, Value, encode_into};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = br#"{"id":42,"topic":"build\u0020complete","raw_meta":{"ok":true,"n":[1,2,3]}}"#;
    let document = IndexedJson::parse(input, Limits::default())?;
    let root = document.root();
    assert_eq!(root.field("id", 4096)?.unwrap().as_u64()?, 42);
    let topic = root.field("topic", 4096)?.unwrap().string()?;
    let mut scratch = [0; 32];
    let decoded = topic.decode(&mut scratch)?;
    assert!(matches!(decoded, JsonStr::Scratch(_)));
    assert_eq!(decoded.as_str(), "build complete");
    let fields = [
        ("id", Value::U64(42)),
        ("topic", Value::String(decoded.as_str())),
        (
            "raw_meta",
            Value::Raw(root.field("raw_meta", 4096)?.unwrap()),
        ),
    ];
    let value = Value::Object(&fields);
    let mut output = [0; 256];
    let len = encode_into(&value, &mut output, EncodeLimits::default())?;
    let output_doc = IndexedJson::parse(&output[..len], Limits::default())?;
    assert!(
        output_doc
            .root()
            .field("topic", 4096)?
            .unwrap()
            .string()?
            .equals("build complete")
    );
    assert!(IndexedJson::parse(br#"{"unknown":[01]}"#, Limits::default()).is_err());
    assert!(IndexedJson::parse(br#"{"id":1,"\u0069d":2}"#, Limits::default()).is_err());
    println!("{}", core::str::from_utf8(&output[..len])?);
    println!(
        "validated {} tokens; scalar host codec",
        document.token_count()
    );
    Ok(())
}

//! Run: cargo run -p vesper-grammar --example codec
use vesper_grammar::grammar::{
    Branch, Grammar, Rule,
    codec::{Value, decode_exact, encode_into, encoded_len},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A two-byte fixed magic, exact frame and canonical record count. Distinct
    // tag bytes make the independently constructed alternatives deterministic.
    let rules = vec![
        Rule::Sequence(vec![1, 2]),
        Rule::Repeat { rule: 3, count: 2 },
        Rule::Frame(4),
        Rule::Byte(b'V'),
        Rule::Counted(5),
        Rule::Choice(vec![Branch { id: 11, rule: 6 }, Branch { id: 22, rule: 9 }]),
        Rule::Sequence(vec![7, 8]),
        Rule::Byte(0x10),
        Rule::Uleb128,
        Rule::Sequence(vec![10, 11]),
        Rule::Byte(0x20),
        Rule::Blob,
    ];
    let grammar = Grammar::validate(&rules, 0, Default::default())?;
    let value = Value::Sequence(vec![
        Value::Repeat(vec![Value::Unit, Value::Unit]),
        Value::Frame(Box::new(Value::Repeat(vec![
            Value::Choice {
                branch: 11,
                value: Box::new(Value::Sequence(vec![Value::Unit, Value::Number(42)])),
            },
            Value::Choice {
                branch: 22,
                value: Box::new(Value::Sequence(vec![Value::Unit, Value::Bytes(b"hello")])),
            },
        ]))),
    ]);
    let expected = b"VV\x0a\x02\x10\x2a\x20\x05hello";
    let needed = encoded_len(&grammar, &value, Default::default())?;
    assert_eq!(needed, expected.len());
    let mut output = [0xa5; 64];
    let written = encode_into(&grammar, &value, &mut output, Default::default())?;
    assert_eq!(&output[..written], expected);
    assert!(output[written..].iter().all(|&b| b == 0xa5));
    let decoded = decode_exact(&grammar, &output[..written], Default::default())?;
    assert_eq!(decoded, value);
    // Serialization consumes logical values rather than retained input bytes.
    let number_rules = [Rule::Uleb128];
    let number = Grammar::validate(&number_rules, 0, Default::default())?;
    let mut changed = [0; 10];
    let len = encode_into(
        &number,
        &Value::Number(300),
        &mut changed,
        Default::default(),
    )?;
    assert_eq!(&changed[..len], &[0xac, 0x02]);
    println!("Canonical batch: {written} bytes, two logical records; changed value300 -> ac02");
    Ok(())
}

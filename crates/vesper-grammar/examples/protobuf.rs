//! Run: cargo run -p vesper-grammar --example protobuf
use vesper_grammar::protobuf::{FieldSchema, Kind, Limits, MessageSchema, PROFILE, Schema, Value};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let messages = [
        MessageSchema {
            fields: vec![
                FieldSchema {
                    number: 1,
                    kind: Kind::U64,
                    repeated: false,
                },
                FieldSchema {
                    number: 2,
                    kind: Kind::String,
                    repeated: false,
                },
                FieldSchema {
                    number: 3,
                    kind: Kind::Message(1),
                    repeated: false,
                },
                FieldSchema {
                    number: 4,
                    kind: Kind::U32,
                    repeated: true,
                },
            ],
        },
        MessageSchema {
            fields: vec![FieldSchema {
                number: 1,
                kind: Kind::U64,
                repeated: false,
            }],
        },
    ];
    let schema = Schema::validate(&messages, 0, Limits::default())?;
    let input = [
        0x08, 0x96, 0x01, 0x12, 0x07, b't', b'e', b's', b't', b'i', b'n', b'g', 0x1a, 0x03, 0x08,
        0x96, 0x01, 0x22, 0x06, 0x03, 0x8e, 0x02, 0x9e, 0xa7, 0x05,
    ];
    let message = schema.parse_exact(&input)?;
    assert_eq!(message.fields[0].value, Value::U64(150));
    assert_eq!(message.fields[1].value, Value::String("testing"));
    assert_eq!(message.fields.len(), 6);
    let mut output = [0xab; 64];
    let len = schema.encode_into(&message, &mut output)?;
    assert_eq!(output[..len], input);
    assert!(output[len..].iter().all(|b| *b == 0xab));
    let mut short = [0xab; 2];
    assert!(schema.encode_into(&message, &mut short).is_err());
    assert_eq!(short, [0xab; 2]);
    assert!(schema.parse_exact(&[0x08, 0x81, 0]).is_err());
    assert!(schema.parse_exact(&[0x22, 0]).is_err());
    println!(
        "{PROFILE}: {} canonical bytes; {} root field elements",
        len,
        message.fields.len()
    );
    println!(
        "Borrowed string, nested message, packed values and transactional failure checks passed"
    );
    Ok(())
}

//! Run: cargo run -p vesper-grammar --example grammar
use vesper_grammar::{
    arrow::{AlignmentProfile, BufferDesc, FieldNode, I64Column},
    framing,
    grammar::{Branch, Grammar, Rule},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut encoded = [0; 32];
    let len = framing::encode_frame_into(b"hello", &mut encoded, framing::Limits::default())?;
    assert_eq!(&encoded[..len], b"\x05hello");
    assert_eq!(
        framing::parse_frame_exact(&encoded[..len], framing::Limits::default())?,
        b"hello"
    );
    assert!(framing::parse_u64_exact(&[0x81, 0], framing::Limits::default()).is_err());
    let first = framing::parse_frame(b"\x01a\x01b", framing::Limits::default())?;
    let second = first.tail.next_frame(framing::Limits::default())?;
    assert_eq!(second.payload, b"b");
    second.tail.assert_exact_eof()?;

    let rules = [
        Rule::Choice(vec![Branch { id: 7, rule: 1 }, Branch { id: 8, rule: 4 }]),
        Rule::Sequence(vec![2, 3]),
        Rule::Byte(b'A'),
        Rule::Uleb128,
        Rule::Sequence(vec![5, 6]),
        Rule::Byte(b'B'),
        Rule::Frame(3),
    ];
    let grammar = Grammar::validate(&rules, 0, Default::default())?;
    let captures = grammar.parse_exact(b"B\x01\x2a")?;
    assert_eq!(captures[0].branch, Some(8));
    assert_eq!(captures.last().unwrap().number, Some(42));

    let mut body = [0; 88];
    body[0] = 0b101;
    for (row, value) in [10_i64, i64::MAX, -4].iter().enumerate() {
        body[64 + row * 8..72 + row * 8].copy_from_slice(&value.to_le_bytes());
    }
    let column = I64Column::validate(
        &body,
        FieldNode {
            row_length: 3,
            null_count: 1,
        },
        BufferDesc {
            offset_bytes: 0,
            byte_length: 1,
        },
        BufferDesc {
            offset_bytes: 64,
            byte_length: 24,
        },
        AlignmentProfile::Relative64,
        Default::default(),
    )?;
    assert_eq!(column.checked_sum(3)?, Some(6));
    println!("Canonical frame: {len} bytes; pipeline remainder consumed exactly");
    println!("Grammar branch 8 decoded nested value 42; Arrow 3 rows, 1 null, sum 6");
    Ok(())
}

use std::sync::Arc;
use vesper_span::{ByteSpan, Limits, LineIndex, Position, SourceEdit, SourceEdits};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let index = LineIndex::new(Arc::from("hello 🦀\r\n"), Limits::default())?;
    assert_eq!(
        index.offset(Position {
            line: 0,
            character: 8
        })?,
        10
    );
    assert!(
        index
            .offset(Position {
                line: 0,
                character: 7
            })
            .is_err()
    );
    let edits = SourceEdits::new(
        &index,
        &[SourceEdit {
            span: ByteSpan { start: 6, end: 10 },
            expected: "🦀".into(),
            replacement: "world".into(),
        }],
        Limits::default(),
    )?;
    assert_eq!(edits.apply()?, "hello world\r\n");
    assert_eq!(index.text(), "hello 🦀\r\n");
    println!("strict UTF-16 coordinates and transactional edits passed");
    Ok(())
}

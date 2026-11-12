//! Self-checking real PyArrow-produced IPC stream example.
use vesper_grammar::ipc::{Limits, parse_exact};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = include_bytes!("../tests/fixtures/ipc/modern-v5.arrow");
    let stream = parse_exact(bytes, Limits::default())?;
    assert_eq!(stream.schema().fields()[0].name(), "nullable");
    assert_eq!(stream.batches().len(), 3);
    let first = &stream.batches()[0];
    assert_eq!(first.rows(), 9);
    assert_eq!(first.columns()[0].get(1), Some(None));
    assert_eq!(first.columns()[0].get(4), Some(Some(1 << 40)));
    assert_eq!(first.columns()[0].checked_sum(9)?, Some(11));
    assert_eq!(first.columns()[1].checked_sum(9)?, Some(450));
    assert_eq!(stream.batches()[1].rows(), 0);
    assert_eq!(stream.batches()[2].columns()[0].checked_sum(4)?, None);
    assert_eq!(stream.batches()[2].columns()[1].checked_sum(4)?, Some(12));
    println!(
        "checked {} fields, {} batches, 13 rows; sums 11/450 and null/12",
        stream.schema().fields().len(),
        stream.batches().len()
    );
    Ok(())
}

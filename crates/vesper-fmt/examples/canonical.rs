use vesper_fmt::{Limits, format_source};
fn main() {
    let source = include_str!("../../../examples/projections.ves");
    let formatted = format_source(source, Limits::default()).unwrap();
    assert!(
        !format_source(&formatted.output, Limits::default())
            .unwrap()
            .changed
    );
    print!("{}", formatted.output);
}

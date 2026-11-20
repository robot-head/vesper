//! The port's character tables are generated from the crates the Rust build
//! uses: `vesper/src/width.ves` must equal the output of
//! `examples/width_table.rs`.
#[path = "../examples/width_table.rs"]
mod width_table;

#[test]
fn width_table_is_generated() {
    let committed = include_str!("../vesper/src/width.ves").replace("\r\n", "\n");
    assert!(
        committed == width_table::generate(),
        "vesper/src/width.ves is stale: run `cargo run -p vesper-diagnostics-seed \
         --example width_table > crates/vesper-diagnostics/vesper/src/width.ves`"
    );
}

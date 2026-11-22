//! `vesper verify --refute`: see crates/vesper-tools/src/fuzz.rs.
fn main() {
    vesper_tools::main(vesper_tools::fuzz::refute)
}

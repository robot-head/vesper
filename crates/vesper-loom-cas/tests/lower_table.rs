//! The Vesper port's lowercase table (`vesper/src/lower.ves`) must equal the
//! output of `examples/lower_table.rs`, and the runs must give
//! `char::to_lowercase` for every char, on a toolchain with the table's
//! Unicode version (Bazel's pinned Rust).
#[path = "../examples/lower_table.rs"]
mod lower_table;

#[test]
fn lower_table_is_generated() {
    let committed = include_str!("../vesper/src/lower.ves").replace("\r\n", "\n");
    if !lower_table::same_unicode(&committed) {
        return;
    }
    assert!(
        committed == lower_table::generate(),
        "vesper/src/lower.ves is stale: run `cargo run -p vesper-loom-cas --example \
         lower_table > crates/vesper-loom-cas/vesper/src/lower.ves`"
    );
}

#[test]
fn runs_lower_every_char() {
    if !lower_table::same_unicode(include_str!("../vesper/src/lower.ves")) {
        return;
    }
    let runs = lower_table::runs();
    for c in (0..=0x10_ffffu32).filter_map(char::from_u32) {
        if c == '\u{130}' {
            continue;
        }
        let cp = c as u64;
        let i = runs.partition_point(|r| u64::from(r.0) <= cp);
        let lowered = match i.checked_sub(1).map(|i| runs[i]) {
            Some((first, last, delta, step))
                if cp <= u64::from(last)
                    && (cp - u64::from(first)).is_multiple_of(u64::from(step)) =>
            {
                cp.wrapping_add(delta)
            }
            _ => cp,
        };
        let expected: Vec<char> = c.to_lowercase().collect();
        assert_eq!(expected, [char::from_u32(lowered as u32).unwrap()], "{c:?}");
    }
}

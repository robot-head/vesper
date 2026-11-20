//! The port's runtime frees everything each call allocates, in both modes
//! and when it refuses a source. Its own test binary, since the allocation
//! counter is shared by every thread.
#![cfg(vesper_port)]
use vesper_highlight::{port_live_allocations, port_tier0};

#[test]
fn port_calls_do_not_leak() {
    let many = "x ".repeat(40_000);
    let cases = [
        "fn f(inout a.{b}: S) -> u64 eff [Fail[E]] { handle E in { 1 } with {} }",
        include_str!("../../../examples/projections.ves"),
        "fn # x \"unterminated",
        "",
        many.as_str(),
    ];
    let _ = port_tier0(cases[0], true);
    let before = port_live_allocations();
    for _ in 0..3 {
        for src in cases {
            let _ = port_tier0(src, true);
            let _ = port_tier0(src, false);
        }
    }
    assert_eq!(port_live_allocations(), before, "the port leaked");
}

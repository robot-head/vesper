//! The port's runtime frees everything each kernel call allocates, on
//! success and on every error path. Its own test binary, since the
//! allocation counter is shared by every thread.
#![cfg(feature = "vesper-port")]

#[macro_use]
#[path = "corpus/mod.rs"]
mod corpus;

use corpus::{Case, case};

world!(port, vesper_forge, crate::Case);

#[test]
fn kernel_calls_do_not_leak() {
    // Warm up, then count: the same cases must leave the count unchanged.
    for s in 0..200 {
        port::run(&case(s));
    }
    let before = vesper_forge::port_live_allocations();
    for _ in 0..3 {
        for s in 0..2000 {
            port::run(&case(s));
        }
    }
    assert_eq!(vesper_forge::port_live_allocations(), before);
}

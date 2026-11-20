//! The port's runtime frees everything each call allocates, on success and
//! on every error path. Its own test binary, since the allocation counter is
//! shared by every thread.
#![cfg(vesper_port)]
use vesper_fmt::{Limits, port_format_source, port_live_allocations};

#[test]
fn port_calls_do_not_leak() {
    let base = Limits::default();
    let cases = [
        ("fn main( )->u64{let x=1;x}", base),
        (include_str!("../../../examples/projections.ves"), base),
        // Syntax: a lexer error, a parser error and an unclosed delimiter.
        ("fn main(){🦀}", base),
        ("fn main(){let x=}", base),
        ("fn main(){", base),
        // Two lexer errors, the chain and depth preflights.
        ("fn # main # ()", base),
        (
            "fn main(){1+1+1+1}",
            Limits {
                max_depth: 1,
                ..base
            },
        ),
        // Limits inside the document and the renderer.
        (
            "fn main(){1}",
            Limits {
                max_doc_nodes: 5,
                ..base
            },
        ),
        (
            "fn main(){1}",
            Limits {
                max_output_bytes: 6,
                ..base
            },
        ),
        (
            "fn main(){1}",
            Limits {
                max_work: 200,
                ..base
            },
        ),
        (
            "fn main(){1}",
            Limits {
                max_work: 900,
                ..base
            },
        ),
        (
            "fn main(){1}",
            Limits {
                max_tokens: 3,
                ..base
            },
        ),
        (
            "fn main(){1}",
            Limits {
                max_input_bytes: 3,
                ..base
            },
        ),
        // The macro profile is handed back to Rust.
        ("fn main(){m!(1)}", base),
    ];
    // Warm up anything the runtime allocates once.
    let _ = port_format_source(cases[0].0, base);
    let before = port_live_allocations();
    for _ in 0..3 {
        for (source, limits) in cases {
            let _ = port_format_source(source, limits);
        }
    }
    assert_eq!(port_live_allocations(), before, "the port leaked");
}

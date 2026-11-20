//! The port's runtime frees everything each call allocates, on success and
//! on every error path. Its own test binary, since the allocation counter is
//! shared by every thread.
#![cfg(vesper_port)]
use std::sync::Arc;
use vesper_core::ast::Diagnostic;
use vesper_diagnostics::*;
use vesper_span::LineIndex;

#[test]
fn port_calls_do_not_leak() {
    let before = port_live_allocations();
    let text = "a😀\r\nb\tc\n名前\n";
    let index = LineIndex::new(Arc::from(text), vesper_span::Limits::default()).unwrap();
    let data = [
        Diagnostic::new("E1", "first", 0..1)
            .with_related(1..9, "multi-line")
            .with_related(8..9, "tab")
            .with_help("help\r\nsecond line"),
        Diagnostic::new("E2", "second", 11..14),
    ];
    for _ in 0..3 {
        let p = prepare(&index, &data, Limits::default()).unwrap();
        assert!(!p.terminal("main.vsp").unwrap().is_empty());
        assert!(!p.terminal_colored("main.vsp").unwrap().is_empty());
        assert_eq!(p.to_lsp("untitled:x").unwrap().len(), 2);
        // Errors: names, output and budgets.
        assert!(p.terminal("bad\nname").is_err());
        assert!(p.lsp_json("").is_err());
        let small = Limits {
            span: vesper_span::Limits {
                max_output_bytes: 64,
                ..Limits::default().span
            },
            ..Limits::default()
        };
        let p = prepare(&index, &data, small).unwrap();
        assert_eq!(p.terminal("main.vsp").unwrap_err(), Error::Output);
        assert_eq!(p.lsp_json("x").unwrap_err(), Error::Output);
        assert!(
            prepare(
                &index,
                &[Diagnostic::new("E", "bad", 2..3)],
                Limits::default()
            )
            .is_err()
        );
        let tight = Limits {
            span: vesper_span::Limits {
                max_labels: 1,
                ..Limits::default().span
            },
            ..Limits::default()
        };
        assert!(prepare(&index, &data, tight).is_err());
    }
    assert_eq!(port_live_allocations(), before);
}

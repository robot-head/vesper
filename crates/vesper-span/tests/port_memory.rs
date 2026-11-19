//! The port's runtime frees everything each call allocates, on success and
//! on every error path. Its own test binary, since the allocation counter is
//! shared by every thread.
#![cfg(vesper_port)]
use std::sync::Arc;
use vesper_span::*;

#[test]
fn port_calls_do_not_leak() {
    let before = port_live_allocations();
    let text = "a😀\r\nb\n";
    for _ in 0..3 {
        let i = LineIndex::new(Arc::from(text), Limits::default()).unwrap();
        let _ = i.position(3);
        let _ = i.position(2);
        let _ = i.offset(Position {
            line: 9,
            character: 0,
        });
        let edit = |start, end, expected: &str| SourceEdit {
            span: ByteSpan { start, end },
            expected: expected.into(),
            replacement: "名".into(),
        };
        let edits = SourceEdits::new(&i, &[edit(0, 1, "a")], Limits::default()).unwrap();
        assert_eq!(edits.apply().unwrap(), "名😀\r\nb\n");
        assert!(SourceEdits::new(&i, &[edit(0, 1, "b")], Limits::default()).is_err());
        assert!(
            SourceEdits::new(&i, &[edit(0, 1, "a"), edit(0, 1, "a")], Limits::default()).is_err()
        );
        let _ = Diagnostics::new(&i, &[], Limits::default()).unwrap();
        // Errors after the line table is built.
        for limits in [
            Limits {
                max_lines: 1,
                ..Default::default()
            },
            Limits {
                max_metadata_bytes: 1,
                ..Default::default()
            },
        ] {
            assert!(LineIndex::new(Arc::from(text), limits).is_err());
        }
    }
    assert_eq!(port_live_allocations(), before);
}

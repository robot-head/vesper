//! The Vesper port (`vesper_span`, this package) against the frozen Rust
//! seed (`span_seed`) on generated sources, coordinates, edits, diagnostics
//! and budgets. Every result, error included, must be the same.
use std::sync::Arc;

use span_seed as rust;
use vesper_span as port;

const PIECES: [&str; 9] = ["a", "😀", "名", "é", "\r", "\n", "\r\n", "e\u{301}", " "];

/// A small xorshift generator, so failures reproduce.
struct Rng(u64);
impl Rng {
    fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.step() % n as u64) as usize
    }
}

fn source(rng: &mut Rng, len: usize) -> String {
    (0..len).map(|_| PIECES[rng.below(PIECES.len())]).collect()
}

fn limits(l: rust::Limits) -> port::Limits {
    port::Limits {
        max_source_bytes: l.max_source_bytes,
        max_lines: l.max_lines,
        max_metadata_bytes: l.max_metadata_bytes,
        max_work: l.max_work,
        max_edits: l.max_edits,
        max_output_bytes: l.max_output_bytes,
        max_diagnostics: l.max_diagnostics,
        max_labels: l.max_labels,
        max_fixes: l.max_fixes,
        max_text_bytes: l.max_text_bytes,
    }
}

/// Defaults with one budget shrunk (or none, for `which` past the end).
fn shrunk(which: usize, to: usize) -> rust::Limits {
    let mut l = rust::Limits::default();
    match which {
        0 => l.max_source_bytes = to,
        1 => l.max_lines = to,
        2 => l.max_metadata_bytes = to,
        3 => l.max_work = to,
        4 => l.max_edits = to,
        5 => l.max_output_bytes = to,
        6 => l.max_diagnostics = to,
        7 => l.max_labels = to,
        8 => l.max_fixes = to,
        9 => l.max_text_bytes = to,
        _ => {}
    }
    l
}

fn same<A: std::fmt::Debug, B: std::fmt::Debug>(a: A, b: B, what: &str) {
    assert_eq!(format!("{a:?}"), format!("{b:?}"), "{what}");
}

#[test]
fn builds_are_the_expected_ones() {
    assert_eq!(port::IMPLEMENTATION, "vesper");
    assert_eq!(rust::IMPLEMENTATION, "rust");
}

#[test]
fn every_offset_and_position_agrees() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for round in 0..300 {
        let text = source(&mut rng, round % 23);
        let r = rust::LineIndex::new(Arc::from(text.as_str()), rust::Limits::default()).unwrap();
        let p = port::LineIndex::new(Arc::from(text.as_str()), port::Limits::default()).unwrap();
        for at in 0..=text.len() + 2 {
            same(
                r.position(at),
                p.position(at),
                &format!("{text:?} position {at}"),
            );
        }
        for line in 0..8 {
            for character in 0..24 {
                same(
                    r.offset(rust::Position { line, character }),
                    p.offset(port::Position { line, character }),
                    &format!("{text:?} offset {line}:{character}"),
                );
            }
        }
        for _ in 0..40 {
            let (a, b) = (rng.below(text.len() + 2), rng.below(text.len() + 2));
            same(
                r.range(rust::ByteSpan { start: a, end: b }),
                p.range(port::ByteSpan { start: a, end: b }),
                &format!("{text:?} range {a}..{b}"),
            );
            let (l0, c0, l1, c1) = (
                rng.below(4) as u32,
                rng.below(8) as u32,
                rng.below(4) as u32,
                rng.below(8) as u32,
            );
            same(
                r.span(rust::Range {
                    start: rust::Position {
                        line: l0,
                        character: c0,
                    },
                    end: rust::Position {
                        line: l1,
                        character: c1,
                    },
                }),
                p.span(port::Range {
                    start: port::Position {
                        line: l0,
                        character: c0,
                    },
                    end: port::Position {
                        line: l1,
                        character: c1,
                    },
                }),
                &format!("{text:?} span {l0}:{c0}..{l1}:{c1}"),
            );
        }
    }
}

#[test]
fn index_budgets_agree() {
    let mut rng = Rng(7);
    for round in 0..400 {
        let text = source(&mut rng, round % 31);
        let which = if round % 2 == 0 { 3 } else { rng.below(11) };
        let to = rng.below(text.len() * 3 + 40);
        let l = shrunk(which, to);
        let r = rust::LineIndex::new(Arc::from(text.as_str()), l);
        let p = port::LineIndex::new(Arc::from(text.as_str()), limits(l));
        same(
            r.as_ref().err(),
            p.as_ref().err(),
            &format!("{text:?} {l:?}"),
        );
        if let (Ok(r), Ok(p)) = (r, p) {
            for at in 0..=text.len() {
                same(
                    r.position(at),
                    p.position(at),
                    &format!("{text:?} {l:?} at {at}"),
                );
            }
            for line in 0..4 {
                for character in 0..12 {
                    same(
                        r.offset(rust::Position { line, character }),
                        p.offset(port::Position { line, character }),
                        &format!("{text:?} {l:?} offset {line}:{character}"),
                    );
                }
            }
        }
    }
    let over = rust::Limits {
        max_work: usize::MAX,
        ..Default::default()
    };
    same(
        rust::LineIndex::new(Arc::from(""), over).err(),
        port::LineIndex::new(Arc::from(""), limits(over)).err(),
        "invalid limits",
    );
}

fn edit_list(rng: &mut Rng, text: &str) -> Vec<(usize, usize, String, String)> {
    (0..rng.below(5))
        .map(|_| {
            let a = rng.below(text.len() + 2);
            let b = if rng.below(3) == 0 {
                a
            } else {
                rng.below(text.len() + 2)
            };
            let expected = text
                .get(a.min(b)..a.max(b))
                .filter(|_| rng.below(8) != 0)
                .unwrap_or("x")
                .to_string();
            let n = rng.below(3);
            (a, b, expected, source(rng, n))
        })
        .collect()
}

fn rust_edits(list: &[(usize, usize, String, String)]) -> Vec<rust::SourceEdit> {
    list.iter()
        .map(|(a, b, e, r)| rust::SourceEdit {
            span: rust::ByteSpan { start: *a, end: *b },
            expected: e.clone(),
            replacement: r.clone(),
        })
        .collect()
}

fn port_edits(list: &[(usize, usize, String, String)]) -> Vec<port::SourceEdit> {
    list.iter()
        .map(|(a, b, e, r)| port::SourceEdit {
            span: port::ByteSpan { start: *a, end: *b },
            expected: e.clone(),
            replacement: r.clone(),
        })
        .collect()
}

#[test]
fn edits_agree() {
    let mut rng = Rng(11);
    let mut applied = 0;
    for round in 0..3000 {
        let text = source(&mut rng, round % 17);
        let ri = rust::LineIndex::new(Arc::from(text.as_str()), rust::Limits::default()).unwrap();
        let pi = port::LineIndex::new(Arc::from(text.as_str()), port::Limits::default()).unwrap();
        let list = edit_list(&mut rng, &text);
        let which = if rng.below(3) == 0 { rng.below(10) } else { 99 };
        let l = shrunk(which, rng.below(text.len() * 4 + 30));
        let r = rust::SourceEdits::new(&ri, &rust_edits(&list), l);
        let p = port::SourceEdits::new(&pi, &port_edits(&list), limits(l));
        same(
            r.as_ref().err(),
            p.as_ref().err(),
            &format!("{text:?} {list:?} {l:?}"),
        );
        if let (Ok(r), Ok(p)) = (r, p) {
            same(r.edits(), p.edits(), "sorted edits");
            assert_eq!(
                r.apply(),
                p.apply().map_err(|_| unreachable!()),
                "{text:?} {list:?}"
            );
            assert_eq!(p.source(), text);
            applied += 1;
        }
    }
    assert!(applied > 300, "only {applied} edit lists applied");
}

#[test]
fn diagnostics_agree() {
    let mut rng = Rng(13);
    let mut accepted = 0;
    for round in 0..2000 {
        let text = source(&mut rng, round % 13);
        let ri = rust::LineIndex::new(Arc::from(text.as_str()), rust::Limits::default()).unwrap();
        let pi = port::LineIndex::new(Arc::from(text.as_str()), port::Limits::default()).unwrap();
        let mut rd = Vec::new();
        let mut pd = Vec::new();
        for _ in 0..rng.below(4) {
            let code = ["", "E1", "E0300"][rng.below(3)].to_string();
            let message = ["", "bad", "already consumed"][rng.below(3)].to_string();
            let help = [None, Some(String::new()), Some("help".to_string())][rng.below(3)].clone();
            let mut rl = Vec::new();
            let mut pl = Vec::new();
            for i in 0..rng.below(4) {
                let a = rng.below(text.len() + 2);
                let b = if rng.below(2) == 0 {
                    a
                } else {
                    rng.below(text.len() + 2)
                };
                let n = rng.below(3);
                let message = source(&mut rng, n);
                let primary = i == 0 && rng.below(6) != 0;
                rl.push(rust::Label {
                    span: rust::ByteSpan { start: a, end: b },
                    message: message.clone(),
                    primary,
                });
                pl.push(port::Label {
                    span: port::ByteSpan { start: a, end: b },
                    message,
                    primary,
                });
            }
            let mut rf = Vec::new();
            let mut pf = Vec::new();
            for _ in 0..rng.below(3) {
                let id = ["", "fix"][rng.below(2)].to_string();
                let title = ["", "Fix it"][rng.below(2)].to_string();
                let list = edit_list(&mut rng, &text);
                rf.push(rust::Fix {
                    id: id.clone(),
                    title: title.clone(),
                    edits: rust_edits(&list),
                });
                pf.push(port::Fix {
                    id,
                    title,
                    edits: port_edits(&list),
                });
            }
            let severity = rng.below(4);
            rd.push(rust::DiagnosticData {
                code: code.clone(),
                message: message.clone(),
                severity: [
                    rust::Severity::Error,
                    rust::Severity::Warning,
                    rust::Severity::Information,
                    rust::Severity::Hint,
                ][severity],
                labels: rl,
                help: help.clone(),
                fixes: rf,
            });
            pd.push(port::DiagnosticData {
                code,
                message,
                severity: [
                    port::Severity::Error,
                    port::Severity::Warning,
                    port::Severity::Information,
                    port::Severity::Hint,
                ][severity],
                labels: pl,
                help,
                fixes: pf,
            });
        }
        let which = if rng.below(3) == 0 { rng.below(10) } else { 99 };
        let l = shrunk(which, rng.below(text.len() * 6 + 300));
        let r = rust::Diagnostics::new(&ri, &rd, l);
        let p = port::Diagnostics::new(&pi, &pd, limits(l));
        same(
            r.as_ref().err(),
            p.as_ref().err(),
            &format!("{text:?} {rd:?} {l:?}"),
        );
        if let (Ok(r), Ok(p)) = (r, p) {
            same(r.data(), p.data(), "copied diagnostics");
            accepted += 1;
        }
    }
    assert!(accepted > 200, "only {accepted} diagnostic sets accepted");
}

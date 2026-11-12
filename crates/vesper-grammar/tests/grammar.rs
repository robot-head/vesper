use vesper_grammar::{
    ErrorKind,
    grammar::{Branch, Grammar, Limits, Rule},
};
fn rules() -> Vec<Rule> {
    vec![
        Rule::Choice(vec![Branch { id: 10, rule: 1 }, Branch { id: 20, rule: 4 }]),
        Rule::Sequence(vec![2, 3]),
        Rule::Byte(b'A'),
        Rule::Uleb128,
        Rule::Sequence(vec![5, 6]),
        Rule::Byte(b'B'),
        Rule::Blob,
    ]
}
#[test]
fn executable_deterministic_choice_dispatch_and_semantic_captures() {
    let rules = rules();
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    assert_eq!(grammar.first_contains(0, b'A'), Some(true));
    assert_eq!(grammar.first_contains(0, b'B'), Some(true));
    assert_eq!(grammar.first_contains(0, b'C'), Some(false));
    assert_eq!(grammar.nullable(0), Some(false));
    let parsed = grammar.parse_exact(b"A\xe5\x8e\x26").unwrap();
    assert_eq!(parsed[0].branch, Some(10));
    assert_eq!(parsed[0].range, 0..4);
    assert_eq!(parsed.last().unwrap().number, Some(624485));
    let input = b"B\x03xyz";
    let parsed = grammar.parse_exact(input).unwrap();
    assert_eq!(parsed[0].branch, Some(20));
    assert_eq!(
        &input[parsed.last().unwrap().payload.clone().unwrap()],
        b"xyz"
    );
    for input in [&b"C"[..], &b"A\x80\x00"[..], &b"B\x04xyz"[..]] {
        assert!(grammar.parse_exact(input).is_err());
    }
    let parsed = grammar.parse_prefix(b"A\x01tail").unwrap();
    let failed = parsed.tail.assert_exact_eof().unwrap_err();
    assert_eq!(failed.tail.remaining_bytes(), b"tail");
    assert_eq!(
        grammar.parse_exact(b"A\x01tail").unwrap_err().kind,
        ErrorKind::TrailingBytes
    );
}
#[test]
fn nullable_sequences_derive_first_but_nullable_choices_are_unsupported() {
    let rules = vec![
        Rule::Sequence(vec![1, 2]),
        Rule::Empty,
        Rule::Literal(b"ok".to_vec()),
    ];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    assert_eq!(grammar.nullable(1), Some(true));
    assert_eq!(grammar.first_contains(0, b'o'), Some(true));
    grammar.parse_exact(b"ok").unwrap();
    let rules = vec![
        Rule::Choice(vec![Branch { id: 1, rule: 1 }, Branch { id: 2, rule: 2 }]),
        Rule::Empty,
        Rule::Byte(1),
    ];
    assert_eq!(
        Grammar::validate(&rules, 0, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::NullableChoice
    );
    let rules = [Rule::Sequence(vec![])];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    assert!(grammar.nullable(0).unwrap());
    grammar.parse_exact(b"").unwrap();
}
#[test]
fn ambiguity_duplicate_branch_ids_bad_references_and_recursion_fail() {
    for (rules, kind) in [
        (
            vec![
                Rule::Choice(vec![Branch { id: 1, rule: 1 }, Branch { id: 2, rule: 2 }]),
                Rule::Literal(b"ab".to_vec()),
                Rule::Literal(b"ac".to_vec()),
            ],
            ErrorKind::AmbiguousChoice,
        ),
        (
            vec![
                Rule::Choice(vec![Branch { id: 1, rule: 1 }, Branch { id: 1, rule: 2 }]),
                Rule::Byte(1),
                Rule::Byte(2),
            ],
            ErrorKind::DuplicateBranch,
        ),
        (vec![Rule::Sequence(vec![99])], ErrorKind::Rule),
        (vec![Rule::Frame(0)], ErrorKind::RecursiveRule),
        (
            vec![Rule::Sequence(vec![1]), Rule::Sequence(vec![0])],
            ErrorKind::RecursiveRule,
        ),
        (vec![Rule::Choice(vec![])], ErrorKind::Rule),
        (
            vec![
                Rule::Choice(vec![Branch { id: 1, rule: 1 }, Branch { id: 2, rule: 2 }]),
                Rule::Uleb128,
                Rule::Byte(1),
            ],
            ErrorKind::AmbiguousChoice,
        ),
        // Even unreachable recursive definitions fail validation.
        (
            vec![Rule::Byte(0), Rule::Frame(1)],
            ErrorKind::RecursiveRule,
        ),
    ] {
        assert_eq!(
            Grammar::validate(&rules, 0, Limits::default())
                .unwrap_err()
                .kind,
            kind
        );
    }
    assert_eq!(
        Grammar::validate(&[], 0, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Rule
    );
    assert_eq!(
        Grammar::validate(&[Rule::Empty], 1, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Rule
    );
}
#[test]
fn nested_dependent_frames_execute_children_with_exact_bounded_payloads() {
    let rules = [Rule::Frame(1), Rule::Frame(2), Rule::Uleb128];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    let captures = grammar.parse_exact(&[2, 1, 42]).unwrap();
    assert_eq!(captures[0].payload, Some(1..3));
    assert_eq!(captures[1].payload, Some(2..3));
    assert_eq!(captures[2].number, Some(42));
    assert_eq!(
        grammar.parse_exact(&[3, 2, 42, 0]).unwrap_err().kind,
        ErrorKind::TrailingBytes
    ); // inner frame must consume its own payload
    assert_eq!(
        grammar.parse_exact(&[1, 2, 42]).unwrap_err().kind,
        ErrorKind::UnexpectedEof
    ); // inner frame cannot escape enclosing range
    for end in 0..3 {
        assert!(grammar.parse_exact(&[2, 1, 42][..end]).is_err());
    }
    assert_eq!(
        Grammar::validate(
            &rules,
            0,
            Limits {
                max_depth: 2,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::DepthLimit
    );
    assert_eq!(
        Grammar::validate(
            &rules,
            0,
            Limits {
                max_depth: 3,
                ..Limits::default()
            }
        )
        .unwrap()
        .parse_exact(&[2, 1, 42])
        .unwrap()
        .len(),
        3
    );
    let grammar = Grammar::validate(
        &rules,
        0,
        Limits {
            max_frame_bytes: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        grammar.parse_exact(&[2, 1, 42]).unwrap_err().kind,
        ErrorKind::PayloadLimit
    );
}
#[test]
fn grammar_definition_execution_and_output_budgets_are_real() {
    let rules = rules();
    let base = Limits::default();
    for (limits, kind) in [
        (
            Limits {
                max_rules: 6,
                ..base
            },
            ErrorKind::CountLimit,
        ),
        (
            Limits {
                max_edges: 1,
                ..base
            },
            ErrorKind::CountLimit,
        ),
        (
            Limits {
                max_metadata_bytes: 0,
                ..base
            },
            ErrorKind::AllocationLimit,
        ),
        (
            Limits {
                max_work: 0,
                ..base
            },
            ErrorKind::WorkLimit,
        ),
        (
            Limits {
                max_depth: 1,
                ..base
            },
            ErrorKind::DepthLimit,
        ),
    ] {
        assert_eq!(Grammar::validate(&rules, 0, limits).unwrap_err().kind, kind);
    }
    for (limits, kind) in [
        (
            Limits {
                max_input_bytes: 1,
                ..base
            },
            ErrorKind::InputLimit,
        ),
        (
            Limits {
                max_events: 1,
                ..base
            },
            ErrorKind::CountLimit,
        ),
        (
            Limits {
                max_capture_bytes: 0,
                ..base
            },
            ErrorKind::AllocationLimit,
        ),
    ] {
        let grammar = Grammar::validate(&rules, 0, limits).unwrap();
        assert_eq!(grammar.parse_exact(b"A\x01").unwrap_err().kind, kind);
    }
    let rule = [Rule::Literal(vec![1, 2])];
    assert_eq!(
        Grammar::validate(
            &rule,
            0,
            Limits {
                max_literal_bytes: 1,
                ..base
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::InputLimit
    );
    assert_eq!(
        Grammar::validate(
            &rule,
            0,
            Limits {
                max_total_literal_bytes: 1,
                ..base
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::InputLimit
    );
}
#[test]
fn aliased_dag_expansion_cannot_bypass_capture_or_work_budgets() {
    let mut rules = vec![Rule::Empty];
    for child in 0..15 {
        rules.push(Rule::Sequence(vec![child, child]));
    }
    let grammar = Grammar::validate(
        &rules,
        15,
        Limits {
            max_capture_bytes: 1024 * 1024,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        grammar.parse_exact(b"").unwrap_err().kind,
        ErrorKind::CountLimit
    );
    let limits = Limits {
        max_work: 100,
        max_events: 100_000,
        max_capture_bytes: 10_000_000,
        ..Limits::default()
    };
    let grammar = Grammar::validate(&rules, 15, limits).unwrap();
    assert_eq!(
        grammar.parse_exact(b"").unwrap_err().kind,
        ErrorKind::WorkLimit
    );
    // A memoized child's own height must still count on a deeper referencing path.
    let rules = [Rule::Empty, Rule::Frame(0), Rule::Frame(1), Rule::Frame(2)];
    assert_eq!(
        Grammar::validate(
            &rules,
            3,
            Limits {
                max_depth: 3,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::DepthLimit
    );
}
#[test]
fn exhaustive_dispatch_matches_independent_byte_and_value_reference() {
    let mut rules = vec![Rule::Choice(
        (0..=255)
            .map(|b| Branch {
                id: b,
                rule: 1 + b as usize,
            })
            .collect(),
    )];
    rules.extend((0..=255).map(|b| Rule::Byte(b as u8)));
    let grammar = Grammar::validate(
        &rules,
        0,
        Limits {
            max_rules: 257,
            ..Limits::default()
        },
    )
    .unwrap();
    for byte in 0..=255 {
        let captures = grammar.parse_exact(&[byte]).unwrap();
        assert_eq!(captures[0].branch, Some(u16::from(byte)));
        assert_eq!(captures[1].rule, 1 + byte as usize);
    }
}

#[test]
fn hard_depth_and_every_definition_byte_budget_reject_before_execution() {
    let mut rules = vec![Rule::Empty];
    for child in 0..64 {
        rules.push(Rule::Frame(child));
    }
    assert_eq!(
        Grammar::validate(
            &rules,
            64,
            Limits {
                max_depth: usize::MAX,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::DepthLimit
    );
    let rules = [Rule::Literal(vec![0; 4]), Rule::Literal(vec![0; 4])];
    assert_eq!(
        Grammar::validate(
            &rules,
            0,
            Limits {
                max_total_literal_bytes: 7,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::InputLimit
    );
    let grammar = Grammar::validate(
        &rules,
        0,
        Limits {
            max_total_literal_bytes: 8,
            ..Limits::default()
        },
    )
    .unwrap();
    grammar.parse_exact(&[0; 4]).unwrap();
    assert_eq!(grammar.first_contains(99, 0), None);
}

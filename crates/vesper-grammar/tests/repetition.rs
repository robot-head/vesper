use vesper_grammar::{
    ErrorKind,
    grammar::{Branch, Capture, Grammar, HARD_MAX_REPETITIONS, Limits, Rule},
};

fn counted(child: Rule) -> Vec<Rule> {
    vec![Rule::Counted(1), child]
}

#[test]
fn fixed_repetition_consumes_exact_children_and_preserves_prefix_tail() {
    let rules = [Rule::Repeat { rule: 1, count: 3 }, Rule::Byte(b'a')];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    let parsed = grammar.parse_prefix(b"aaatail").unwrap();
    assert_eq!(parsed.captures.len(), 4);
    assert_eq!(parsed.captures[0].range, 0..3);
    assert_eq!(parsed.captures[0].number, None);
    for (index, event) in parsed.captures[1..].iter().enumerate() {
        assert_eq!(event.rule, 1);
        assert_eq!(event.range, index..index + 1);
    }
    let failed = parsed.tail.assert_exact_eof().unwrap_err();
    assert_eq!(failed.tail.offset(), 3);
    assert_eq!(failed.tail.remaining_bytes(), b"tail");
    assert_eq!(
        grammar.parse_exact(b"aa").unwrap_err().kind,
        ErrorKind::UnexpectedEof
    );
    assert_eq!(
        grammar.parse_exact(b"aab").unwrap_err().kind,
        ErrorKind::Mismatch
    );
    assert_eq!(
        grammar.parse_exact(b"aaaa").unwrap_err().kind,
        ErrorKind::TrailingBytes
    );
}

#[test]
fn counted_capture_keeps_the_actual_canonical_count_and_child_ranges() {
    let rules = counted(Rule::Byte(b'x'));
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    let events = grammar.parse_exact(b"\x03xxx").unwrap();
    assert_eq!(events[0].rule, 0);
    assert_eq!(events[0].number, Some(3));
    assert_eq!(events[0].range, 0..4);
    assert_eq!(events[0].branch, None);
    assert_eq!(events[0].payload, None);
    assert_eq!(events.len(), 4);
    for (index, event) in events[1..].iter().enumerate() {
        assert_eq!(event.range, index + 1..index + 2);
    }
    for input in [b"".as_slice(), b"\x03", b"\x03x", b"\x03xx"] {
        assert_eq!(
            grammar.parse_exact(input).unwrap_err().kind,
            ErrorKind::UnexpectedEof
        );
    }
    assert_eq!(
        grammar.parse_exact(b"\x03xxxx").unwrap_err().kind,
        ErrorKind::TrailingBytes
    );
}

#[test]
fn zero_counts_and_nullable_children_remain_finite_and_observable() {
    for child in [Rule::Empty, Rule::Literal(vec![]), Rule::Sequence(vec![])] {
        let rules = counted(child);
        let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
        for count in [0_u16, 1, 127, 128, 1024] {
            // Independent division/remainder oracle for the count prefix.
            let mut number = count;
            let mut bytes = Vec::new();
            loop {
                let digit = u8::try_from(number % 128).unwrap();
                number /= 128;
                bytes.push(digit | if number == 0 { 0 } else { 128 });
                if number == 0 {
                    break;
                }
            }
            let events = grammar.parse_exact(&bytes).unwrap();
            assert_eq!(events.len(), usize::from(count) + 1);
            assert_eq!(events[0].number, Some(u64::from(count)));
            for event in &events[1..] {
                assert_eq!(event.range, bytes.len()..bytes.len());
            }
        }
    }
    let rules = [Rule::Repeat { rule: 1, count: 0 }, Rule::Byte(b'a')];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    assert_eq!(grammar.parse_exact(b"").unwrap().len(), 1);
    assert_eq!(
        grammar.parse_exact(b"a").unwrap_err().kind,
        ErrorKind::TrailingBytes
    );
}

#[test]
fn first_and_nullable_are_derived_for_zero_positive_and_wire_counts() {
    let rules = [
        Rule::Sequence(vec![1, 3]),
        Rule::Repeat { rule: 2, count: 0 },
        Rule::Byte(b'a'),
        Rule::Repeat { rule: 4, count: 2 },
        Rule::Byte(b'b'),
        Rule::Counted(6),
        Rule::Empty,
        Rule::Repeat { rule: 6, count: 4 },
    ];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    assert_eq!(grammar.nullable(0), Some(false));
    assert_eq!(grammar.nullable(1), Some(true));
    assert_eq!(grammar.nullable(3), Some(false));
    assert_eq!(grammar.nullable(5), Some(false));
    assert_eq!(grammar.nullable(7), Some(true));
    for byte in 0_u8..=255 {
        assert_eq!(grammar.first_contains(0, byte), Some(byte == b'b'));
        assert_eq!(grammar.first_contains(1, byte), Some(false));
        assert_eq!(grammar.first_contains(5, byte), Some(true));
    }
    grammar.parse_exact(b"bb").unwrap();
}

#[test]
fn choices_keep_existing_disjoint_and_nonnullable_requirements() {
    let rules = [
        Rule::Choice(vec![Branch { id: 4, rule: 1 }, Branch { id: 9, rule: 3 }]),
        Rule::Repeat { rule: 2, count: 2 },
        Rule::Byte(b'a'),
        Rule::Byte(b'b'),
    ];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    assert_eq!(grammar.parse_exact(b"aa").unwrap()[0].branch, Some(4));
    assert_eq!(grammar.parse_exact(b"b").unwrap()[0].branch, Some(9));
    for child in [
        Rule::Repeat { rule: 3, count: 0 },
        Rule::Repeat { rule: 4, count: 1 },
    ] {
        let rules = [
            Rule::Choice(vec![Branch { id: 1, rule: 1 }, Branch { id: 2, rule: 2 }]),
            child,
            Rule::Byte(b'z'),
            Rule::Byte(b'a'),
            Rule::Empty,
        ];
        assert_eq!(
            Grammar::validate(&rules, 0, Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::NullableChoice
        );
    }
    let rules = [
        Rule::Choice(vec![Branch { id: 1, rule: 1 }, Branch { id: 2, rule: 2 }]),
        Rule::Counted(3),
        Rule::Byte(b'a'),
        Rule::Empty,
    ];
    assert_eq!(
        Grammar::validate(&rules, 0, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::AmbiguousChoice
    );
}

#[test]
fn zero_and_unreachable_references_still_validate_cycles_and_height() {
    for (rules, kind) in [
        (vec![Rule::Repeat { rule: 99, count: 0 }], ErrorKind::Rule),
        (
            vec![Rule::Repeat { rule: 0, count: 0 }],
            ErrorKind::RecursiveRule,
        ),
        (
            vec![Rule::Empty, Rule::Counted(1)],
            ErrorKind::RecursiveRule,
        ),
        (
            vec![Rule::Empty, Rule::Repeat { rule: 99, count: 0 }],
            ErrorKind::Rule,
        ),
    ] {
        assert_eq!(
            Grammar::validate(&rules, 0, Limits::default())
                .unwrap_err()
                .kind,
            kind
        );
    }
    let rules = [
        Rule::Repeat { rule: 1, count: 0 },
        Rule::Frame(2),
        Rule::Empty,
    ];
    assert_eq!(
        Grammar::validate(
            &rules,
            0,
            Limits {
                max_depth: 2,
                ..Default::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::DepthLimit
    );
    Grammar::validate(
        &rules,
        0,
        Limits {
            max_depth: 3,
            ..Default::default()
        },
    )
    .unwrap();
    let mut rules = vec![Rule::Repeat { rule: 1, count: 0 }];
    rules.extend((1..63).map(|i| Rule::Frame(i + 1)));
    rules.push(Rule::Empty);
    Grammar::validate(
        &rules,
        0,
        Limits {
            max_depth: usize::MAX,
            ..Default::default()
        },
    )
    .unwrap();
    rules[63] = Rule::Frame(64);
    rules.push(Rule::Empty);
    assert_eq!(
        Grammar::validate(
            &rules,
            0,
            Limits {
                max_depth: usize::MAX,
                ..Default::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::DepthLimit
    );
}

#[test]
fn wire_count_rejects_overlong_truncated_overflowing_and_excessive_prefixes() {
    let rules = counted(Rule::Empty);
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    for input in [b"\x80\x00".as_slice(), b"\x81\x00", b"\x80\x80\x00"] {
        assert_eq!(
            grammar.parse_exact(input).unwrap_err().kind,
            ErrorKind::NonCanonical
        );
    }
    assert_eq!(
        grammar.parse_exact(b"\x80").unwrap_err().kind,
        ErrorKind::UnexpectedEof
    );
    assert_eq!(
        grammar.parse_exact(&[0x80; 10]).unwrap_err().kind,
        ErrorKind::Overflow
    );
    assert_eq!(
        grammar.parse_exact(b"\x81\x08").unwrap_err().kind,
        ErrorKind::CountLimit
    );
    let mut maximum = [0xff; 10];
    maximum[9] = 1;
    let expected = if usize::BITS < 64 {
        ErrorKind::Overflow
    } else {
        ErrorKind::CountLimit
    };
    assert_eq!(grammar.parse_exact(&maximum).unwrap_err().kind, expected);
}

#[test]
fn count_event_capture_and_work_limits_refuse_before_any_child_mismatch() {
    let rules = counted(Rule::Byte(b'a'));
    for (limits, input, expected) in [
        (
            Limits {
                max_repetitions: 2,
                ..Default::default()
            },
            b"\x03!".as_slice(),
            ErrorKind::CountLimit,
        ),
        (
            Limits {
                max_events: 3,
                ..Default::default()
            },
            b"\x03!",
            ErrorKind::CountLimit,
        ),
        (
            Limits {
                max_capture_bytes: 3 * size_of::<Capture>(),
                ..Default::default()
            },
            b"\x03!",
            ErrorKind::AllocationLimit,
        ),
        (
            Limits {
                max_work: 6,
                ..Default::default()
            },
            b"\x05!",
            ErrorKind::WorkLimit,
        ),
    ] {
        let grammar = Grammar::validate(&rules, 0, limits).unwrap();
        assert_eq!(grammar.parse_exact(input).unwrap_err().kind, expected);
    }
}

#[test]
fn count_zero_policy_and_hard_count_boundary_are_explicit() {
    let rules = counted(Rule::Empty);
    let grammar = Grammar::validate(
        &rules,
        0,
        Limits {
            max_repetitions: 0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(grammar.parse_exact(b"\x00").unwrap().len(), 1);
    assert_eq!(
        grammar.parse_exact(b"\x01").unwrap_err().kind,
        ErrorKind::CountLimit
    );
    let zero = [Rule::Repeat { rule: 1, count: 0 }, Rule::Empty];
    Grammar::validate(
        &zero,
        0,
        Limits {
            max_repetitions: 0,
            ..Default::default()
        },
    )
    .unwrap()
    .parse_exact(b"")
    .unwrap();
    let fixed = [Rule::Repeat { rule: 1, count: 1 }, Rule::Empty];
    assert_eq!(
        Grammar::validate(
            &fixed,
            0,
            Limits {
                max_repetitions: 0,
                ..Default::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::CountLimit
    );
    assert_eq!(HARD_MAX_REPETITIONS, 4096);
    assert_eq!(
        Grammar::validate(
            &rules,
            0,
            Limits {
                max_repetitions: HARD_MAX_REPETITIONS + 1,
                ..Default::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Rule
    );
    let grammar = Grammar::validate(
        &rules,
        0,
        Limits {
            max_repetitions: HARD_MAX_REPETITIONS,
            max_events: HARD_MAX_REPETITIONS + 1,
            max_capture_bytes: 1024 * 1024,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        grammar.parse_exact(b"\x80\x20").unwrap().len(),
        HARD_MAX_REPETITIONS + 1
    );
}

#[test]
fn repetition_stays_inside_actual_frame_boundaries() {
    let rules = [Rule::Frame(1), Rule::Counted(2), Rule::Byte(b'a')];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    let events = grammar.parse_exact(b"\x03\x02aa").unwrap();
    assert_eq!(events[0].payload, Some(1..4));
    assert_eq!(events[1].range, 1..4);
    assert_eq!(events[1].number, Some(2));
    assert_eq!(
        grammar.parse_exact(b"\x02\x02aa").unwrap_err().kind,
        ErrorKind::UnexpectedEof
    );
    assert_eq!(
        grammar.parse_exact(b"\x04\x02aax").unwrap_err().kind,
        ErrorKind::TrailingBytes
    );
    assert_eq!(
        grammar.parse_exact(b"\x03\x02aaextra").unwrap_err().kind,
        ErrorKind::TrailingBytes
    );
    assert_eq!(
        Grammar::validate(
            &rules,
            0,
            Limits {
                max_frame_bytes: 2,
                ..Default::default()
            }
        )
        .unwrap()
        .parse_exact(b"\x03\x02aa")
        .unwrap_err()
        .kind,
        ErrorKind::PayloadLimit
    );
    assert_eq!(
        Grammar::validate(
            &rules,
            0,
            Limits {
                max_input_bytes: 3,
                ..Default::default()
            }
        )
        .unwrap()
        .parse_exact(b"\x03\x02aa")
        .unwrap_err()
        .kind,
        ErrorKind::InputLimit
    );
}

#[test]
fn shared_repeated_dag_visits_are_counted_each_time() {
    let rules = [
        Rule::Repeat { rule: 1, count: 32 },
        Rule::Repeat { rule: 2, count: 32 },
        Rule::Empty,
    ];
    let grammar = Grammar::validate(&rules, 0, Limits::default()).unwrap();
    assert_eq!(grammar.parse_exact(b"").unwrap().len(), 1 + 32 * (1 + 32));
    let bounded = Grammar::validate(
        &rules,
        0,
        Limits {
            max_events: 100,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        bounded.parse_exact(b"").unwrap_err().kind,
        ErrorKind::CountLimit
    );
    let bounded = Grammar::validate(
        &rules,
        0,
        Limits {
            max_work: 100,
            max_events: 1000,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        bounded.parse_exact(b"").unwrap_err().kind,
        ErrorKind::WorkLimit | ErrorKind::AllocationLimit
    ));
}

//! Parsing, well-formedness, endpoint projection and mergeability.

use vesper_chorus::check::{ExploreError, ExploreLimits, explore, explore_locals};
use vesper_chorus::parse::ParseErrorKind;
use vesper_chorus::{
    LocalType, ProjectionError, RoleId, WellFormedError, are_dual, parse_choreography, project,
    project_all,
};

const TWO_BUYER: &str = "
roles B1, B2, S;
B1 -> S: Title(String);
S -> B1: Quote(u64);
S -> B2: Quote(u64);
B1 -> B2: Share(u64);
B2 -> S {
    Ok() {
        B2 -> S: Address(String);
        S -> B2: Date(u64);
    }
    Quit() { }
}
";

fn render(src: &str, role: &str) -> String {
    let c = parse_choreography(src).unwrap();
    project(&c, role).unwrap().render(&c)
}

#[test]
fn two_buyer_projects_to_the_textbook_local_types() {
    assert_eq!(
        render(TWO_BUYER, "B1"),
        "S!Title(String); S?Quote(u64); B2!Share(u64); end"
    );
    assert_eq!(
        render(TWO_BUYER, "B2"),
        "S?Quote(u64); B1?Share(u64); select S { Ok(): S!Address(String); S?Date(u64); end, Quit(): end }"
    );
    assert_eq!(
        render(TWO_BUYER, "S"),
        "B1?Title(String); B1!Quote(u64); B2!Quote(u64); offer B2 { Ok(): B2?Address(String); B2!Date(u64); end, Quit(): end }"
    );
    let c = parse_choreography(TWO_BUYER).unwrap();
    let r = explore(&c, ExploreLimits::default()).unwrap();
    assert_eq!(
        r.completed, 1,
        "both branches reach the single terminal state"
    );
    assert!(!r.queue_bound_hit);
}

#[test]
fn unmergeable_third_party_branch_is_rejected_with_a_precise_reason() {
    // C is never told which branch A chose, but must send in one and not the
    // other.
    let src = "
roles A, B, C;
A -> B {
    Left() { C -> B: Ping(); }
    Right() { }
}
";
    let c = parse_choreography(src).unwrap();
    // A and B project fine.
    project(&c, "A").unwrap();
    project(&c, "B").unwrap();
    let err = project(&c, "C").unwrap_err();
    match &err {
        ProjectionError::Unmergeable(u) => {
            assert_eq!(
                (u.role.as_str(), u.decider.as_str(), u.notified.as_str()),
                ("C", "A", "B")
            );
            assert_eq!(
                (u.left_label.as_str(), u.right_label.as_str()),
                ("Left", "Right")
            );
            assert_eq!(u.left, "send Ping to B");
            assert_eq!(u.right, "end");
            assert!(u.path.is_empty());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        err.to_string(),
        "role `C` cannot tell whether `A` chose `Left` or `Right` (only `B` is told): it does `send Ping to B` in one branch and `end` in the other"
    );
    assert!(matches!(
        explore(&c, ExploreLimits::default()),
        Err(ExploreError::Projection(_))
    ));
}

#[test]
fn informed_third_party_merges_into_an_offer() {
    // C learns the choice from B, by label, in both branches.
    let src = "
roles A, B, C;
A -> B {
    Left() { B -> C: GoLeft(); C -> A: Done(); }
    Right() { B -> C: GoRight(u64); C -> A: Done(); }
}
";
    assert_eq!(
        render(src, "C"),
        "offer B { GoLeft(): A!Done(); end, GoRight(u64): A!Done(); end }"
    );
    // A must not merge: it chose. It selects.
    assert!(render(src, "A").starts_with("select B"));
    let c = parse_choreography(src).unwrap();
    assert_eq!(explore(&c, ExploreLimits::default()).unwrap().completed, 1);
}

#[test]
fn identical_third_party_behaviour_needs_no_notification() {
    let src = "
roles A, B, C;
A -> B {
    Left() { C -> B: Log(); }
    Right() { C -> B: Log(); B -> A: Ack(); }
}
";
    assert_eq!(render(src, "C"), "B!Log(); end");
    let c = parse_choreography(src).unwrap();
    explore(&c, ExploreLimits::default()).unwrap();
}

#[test]
fn nested_choice_path_is_reported() {
    let src = "
roles A, B, C, D;
A -> B: Start();
A -> B {
    One() {
        B -> C {
            X() { D -> A: Late(); }
            Y() { }
        }
    }
    Two() { B -> C: Z(); }
}
";
    let c = parse_choreography(src).unwrap();
    let err = project(&c, "D").unwrap_err();
    match err {
        ProjectionError::Unmergeable(u) => {
            assert_eq!(u.decider, "B");
            assert_eq!(u.path, vec!["A->B.One".to_owned()]);
            assert_eq!(u.left_label, "X");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn payload_mismatch_on_a_merged_label_is_rejected() {
    let src = "
roles A, B, C;
A -> B {
    L() { B -> C: Data(u64); }
    R() { B -> C: Data(String); }
}
";
    let c = parse_choreography(src).unwrap();
    assert!(matches!(
        project(&c, "C").unwrap_err(),
        ProjectionError::PayloadMismatch(ref p) if p.label == "Data"
    ));
}

const STREAM: &str = "
roles P, C, L;
rec Loop {
    P -> C {
        More(u64) { C -> L: Seen(u64); continue Loop; }
        Stop() { C -> L: Closed(); }
    }
}
";

#[test]
fn recursion_projects_and_runs() {
    assert_eq!(
        render(STREAM, "P"),
        "rec Loop { select C { More(u64): continue Loop, Stop(): end } }"
    );
    assert_eq!(
        render(STREAM, "L"),
        "rec Loop { offer C { Closed(): end, Seen(u64): continue Loop } }"
    );
    let c = parse_choreography(STREAM).unwrap();
    let r = explore(&c, ExploreLimits::default()).unwrap();
    assert!(r.completed >= 1);
    // The producer can run ahead of the consumer: the explorer bounds the
    // queues and says so.
    assert!(r.queue_bound_hit);
}

#[test]
fn uninformed_role_after_a_loop_is_unmergeable() {
    // L never learns when the loop ends.
    let src = "
roles P, C, L;
rec X {
    P -> C {
        More() { continue X; }
        Stop() { C -> L: Done(); }
    }
}
";
    let c = parse_choreography(src).unwrap();
    let err = project(&c, "L").unwrap_err();
    assert!(
        matches!(&err, ProjectionError::Unmergeable(u) if u.left == "continue X" && u.right == "receive Done from C"),
        "{err:?}"
    );
}

#[test]
fn role_outside_a_loop_projects_through_it() {
    let src = "
roles A, B, C;
A -> C: Hello();
rec X {
    A -> B {
        Again() { continue X; }
        Stop() { }
    }
}
";
    assert_eq!(render(src, "C"), "A?Hello(); end");
    let c = parse_choreography(src).unwrap();
    explore(&c, ExploreLimits::default()).unwrap();
}

#[test]
fn sequencing_after_a_choice_continues_every_branch() {
    let src = "
roles A, B;
A -> B { L() { } R() { B -> A: Extra(); } }
A -> B: After();
";
    assert_eq!(
        render(src, "A"),
        "select B { L(): B!After(); end, R(): B?Extra(); B!After(); end }"
    );
    // `end` stops a branch before the continuation.
    let src = "
roles A, B;
A -> B { L() { end; } R() { } }
A -> B: After();
";
    assert_eq!(
        render(src, "B"),
        "offer A { L(): end, R(): A?After(); end }"
    );
}

#[test]
fn local_actions_project_only_to_their_role() {
    let src = "
roles L, F;
L -> F: Append(Batch);
F: persist;
F -> L: Ack();
";
    assert_eq!(
        render(src, "F"),
        "L?Append(Batch); do persist; L!Ack(); end"
    );
    assert_eq!(render(src, "L"), "F!Append(Batch); F?Ack(); end");
    // A local action in only one branch of an uninformed role is a
    // behavioural difference.
    let src = "
roles A, B, C;
A -> B { L() { C: log; B -> C: X(); } R() { B -> C: Y(); } }
";
    let c = parse_choreography(src).unwrap();
    assert!(matches!(
        project(&c, "C").unwrap_err(),
        ProjectionError::Unmergeable(ref u) if u.left == "local action log"
    ));
}

#[test]
fn binary_projections_are_dual() {
    let src = "
roles C, S;
rec X {
    C -> S {
        Req(u64) { S -> C: Resp(u64); continue X; }
        Bye() { }
    }
}
";
    let c = parse_choreography(src).unwrap();
    let all = project_all(&c).unwrap();
    assert!(are_dual(RoleId(0), &all[0], RoleId(1), &all[1]));
    // Breaking one side breaks duality.
    let broken = LocalType::Send {
        peer: RoleId(1),
        label: "Req".into(),
        payload: "u64".into(),
        next: Box::new(LocalType::End),
    };
    assert!(!are_dual(RoleId(0), &broken, RoleId(1), &all[1]));
}

#[test]
fn hand_written_endpoints_that_skip_projection_can_deadlock() {
    let c = parse_choreography("roles A, B; A -> B: X(); B -> A: Y();").unwrap();
    // Both wait for the other first.
    let a = LocalType::Recv {
        peer: RoleId(1),
        label: "Y".into(),
        payload: String::new(),
        next: Box::new(LocalType::End),
    };
    let b = LocalType::Recv {
        peer: RoleId(0),
        label: "X".into(),
        payload: String::new(),
        next: Box::new(LocalType::End),
    };
    match explore_locals(&c, vec![a, b], ExploreLimits::default()) {
        Err(ExploreError::Deadlock { waiting }) => assert_eq!(waiting.len(), 2),
        other => panic!("{other:?}"),
    }
    // A receiver that does not expect a label sees it at its queue head.
    let a = LocalType::Send {
        peer: RoleId(1),
        label: "Z".into(),
        payload: String::new(),
        next: Box::new(LocalType::End),
    };
    let b = LocalType::Recv {
        peer: RoleId(0),
        label: "X".into(),
        payload: String::new(),
        next: Box::new(LocalType::End),
    };
    assert!(matches!(
        explore_locals(&c, vec![a.clone(), b], ExploreLimits::default()),
        Err(ExploreError::UnexpectedMessage { .. })
    ));
    // A message nobody receives is an orphan.
    assert!(matches!(
        explore_locals(&c, vec![a, LocalType::End], ExploreLimits::default()),
        Err(ExploreError::Orphans { .. })
    ));
}

#[test]
fn well_formedness_errors_are_typed() {
    let wf = |src: &str| match parse_choreography(src) {
        Err(e) => match e.kind {
            ParseErrorKind::Invalid(w) => w,
            other => panic!("{other:?}"),
        },
        Ok(_) => panic!("accepted: {src}"),
    };
    assert_eq!(
        wf("roles A, B; A -> A: X();"),
        WellFormedError::SelfCommunication { role: "A".into() }
    );
    assert!(matches!(
        wf("roles A, B; A -> B { L() {} L() {} }"),
        WellFormedError::DuplicateLabel { .. }
    ));
    assert_eq!(
        wf("roles A, B; A -> B: X(); continue Y;"),
        WellFormedError::UnboundRecursionVariable { var: "Y".into() }
    );
    assert_eq!(
        wf("roles A, B; rec X { A: spin; continue X; }"),
        WellFormedError::UnguardedRecursion { var: "X".into() }
    );
    assert_eq!(
        wf("roles A, B; rec X { A -> B: M(); rec X { continue X; } }"),
        WellFormedError::ShadowedRecursionVariable { var: "X".into() }
    );
    assert_eq!(
        wf("roles A, B, C; A -> B: X();"),
        WellFormedError::UnusedRole { name: "C".into() }
    );
    assert!(matches!(
        wf("roles A, A; A -> A: X();"),
        WellFormedError::DuplicateRole { .. }
    ));
}

#[test]
fn parse_errors_carry_positions() {
    let e = parse_choreography("roles A, B;\nA -> Q: X();").unwrap_err();
    assert_eq!((e.line, e.col), (2, 6));
    assert_eq!(e.kind, ParseErrorKind::UnknownRole("Q".into()));
    let e = parse_choreography("roles A, B; A -> B: X(); end; A -> B: Y();").unwrap_err();
    assert_eq!(e.kind, ParseErrorKind::Unreachable);
    let e = parse_choreography("roles A, B; A -> B X();").unwrap_err();
    assert!(matches!(e.kind, ParseErrorKind::UnexpectedToken { .. }));
    let e = parse_choreography("roles A, B; A -> B: X(").unwrap_err();
    assert!(matches!(e.kind, ParseErrorKind::UnexpectedEof { .. }));
    let e = parse_choreography("roles A; A $").unwrap_err();
    assert_eq!(e.kind, ParseErrorKind::UnexpectedChar('$'));
    // Generic payload types are kept verbatim.
    let c = parse_choreography("roles A, B; A -> B: Batch(Vec[Pair[u64, core::Entry]]);").unwrap();
    assert_eq!(
        project(&c, "B").unwrap().render(&c),
        "A?Batch(Vec[Pair[u64, core::Entry]]); end"
    );
}

#[test]
fn hostile_inputs_are_bounded() {
    // Exponential blow-up through sequencing after nested choices.
    let mut src = String::from("roles A, B;\n");
    for _ in 0..40 {
        src.push_str("A -> B { L() { } R() { } }\n");
    }
    let e = parse_choreography(&src).unwrap_err();
    assert_eq!(e.kind, ParseErrorKind::TooLarge);
    // Deep nesting.
    let mut src = String::from("roles A, B;\n");
    for _ in 0..2000 {
        src.push_str("A -> B { L() {");
    }
    let e = parse_choreography(&src).unwrap_err();
    assert_eq!(e.kind, ParseErrorKind::TooDeep);
    // Oversized source.
    let big = "x".repeat(vesper_chorus::parse::MAX_SOURCE_BYTES + 1);
    assert!(matches!(
        parse_choreography(&big).unwrap_err().kind,
        ParseErrorKind::TooLong { .. }
    ));
    // Garbage never panics.
    let mut seed = 0x1234_5678u64;
    let alphabet = b"AB->:;{}()[], recontinuendroles\n/";
    for _ in 0..2000 {
        let mut s = String::from("roles A, B; ");
        for _ in 0..(seed % 60) {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            s.push(alphabet[(seed >> 33) as usize % alphabet.len()] as char);
        }
        if let Ok(c) = parse_choreography(&s) {
            for r in ["A", "B"] {
                let _ = project(&c, r);
            }
        }
    }
}

#[test]
fn long_sequential_protocols_are_supported() {
    let mut src = String::from("roles A, B;\n");
    for i in 0..250 {
        if i % 2 == 0 {
            src.push_str("A -> B: Ping(u64);\n");
        } else {
            src.push_str("B -> A: Pong(u64);\n");
        }
    }
    let c = parse_choreography(&src).unwrap();
    let all = project_all(&c).unwrap();
    assert!(are_dual(RoleId(0), &all[0], RoleId(1), &all[1]));
    // Beyond the depth limit the parser refuses instead of building a tree
    // that recursive passes could not handle.
    for _ in 0..10 {
        src.push_str("A -> B: Ping(u64);\n");
    }
    assert_eq!(
        parse_choreography(&src).unwrap_err().kind,
        ParseErrorKind::TooDeep
    );
}

#[test]
fn absent_role_in_an_inner_loop_that_exits_to_an_outer_loop_must_merge() {
    // C sits out the inner loop, but the inner loop can jump back to the
    // outer one, where C must act again: C cannot simply end.
    let src = "
roles A, B, C;
rec Outer {
    C -> A: Tick();
    rec Inner {
        A -> B {
            Again() { continue Inner; }
            Back() { continue Outer; }
        }
    }
}
";
    let c = parse_choreography(src).unwrap();
    assert!(matches!(
        project(&c, "C").unwrap_err(),
        ProjectionError::Unmergeable(ref u) if u.left == "continue Inner" && u.right == "continue Outer"
    ));
}

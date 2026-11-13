use vesper_fmt::{Doc, Document, Limits, ProofError, ProofLimits};

#[test]
fn actual_order_unicode_payloads_and_shared_occurrences_are_owned_exactly() {
    let comment = "// 🦀 é\r\n";
    let mut document = Document::new(Limits::default()).unwrap();
    let first = document.push(Doc::Comment(comment)).unwrap();
    let text = document
        .push(Doc::Text("literal /* not a comment tag */"))
        .unwrap();
    let second = document.push(Doc::Comment("/* two */")).unwrap();
    let nested = document.push(Doc::Nest(4, second)).unwrap();
    let root = document
        .push(Doc::Concat(vec![first, text, nested, first]))
        .unwrap();
    let before = document.render(root).unwrap();
    let projected = document.proof_model(root, ProofLimits::default()).unwrap();
    assert_eq!(projected.comments(), &[comment, "/* two */", comment]);
    assert_eq!(projected.node_count(), 6);
    assert_eq!(projected.depth(), 3);
    assert_eq!(
        projected.text_bytes(),
        comment.len() * 2 + "/* two */".len() + "literal /* not a comment tag */".len()
    );
    assert_eq!(before, document.render(root).unwrap());
    drop(document);
    assert_eq!(projected.comments()[0], comment);
}

#[test]
fn foreign_and_abandoned_document_ids_cannot_project_equal_numeric_indices() {
    let mut first = Document::new(Limits::default()).unwrap();
    let mut second = Document::new(Limits::default()).unwrap();
    let own = first.push(Doc::Text("first")).unwrap();
    let foreign = second.push(Doc::Text("second")).unwrap();
    drop(second);
    assert_eq!(
        first
            .proof_model(foreign, ProofLimits::default())
            .unwrap_err(),
        ProofError::InvalidDocument
    );
    assert_eq!(
        first
            .proof_model(own, ProofLimits::default())
            .unwrap()
            .text_bytes(),
        5
    );
}

#[test]
fn expanded_dag_occurrences_are_counted_before_any_owned_tree_is_published() {
    let mut document = Document::new(Limits::default()).unwrap();
    let mut root = document.push(Doc::Comment("/* x */")).unwrap();
    for _ in 0..12 {
        root = document.push(Doc::Concat(vec![root, root])).unwrap();
    }
    assert_eq!(
        document
            .proof_model(root, ProofLimits::default())
            .unwrap_err(),
        ProofError::Resource("nodes")
    );
    let small = ProofLimits {
        max_nodes: 64,
        ..ProofLimits::default()
    };
    assert_eq!(
        document.proof_model(root, small).unwrap_err(),
        ProofError::Resource("nodes")
    );
}

#[test]
fn repeated_aliases_pay_text_limits_even_when_the_arena_has_one_payload() {
    let mut document = Document::new(Limits::default()).unwrap();
    let leaf = document.push(Doc::Text("12345678")).unwrap();
    let root = document.push(Doc::Concat(vec![leaf; 16])).unwrap();
    assert_eq!(
        document
            .proof_model(
                root,
                ProofLimits {
                    max_text_bytes: 64,
                    ..ProofLimits::default()
                }
            )
            .unwrap_err(),
        ProofError::Resource("text")
    );
    let projected = document.proof_model(root, ProofLimits::default()).unwrap();
    assert_eq!(projected.node_count(), 17);
    assert_eq!(projected.text_bytes(), 128);
}

#[test]
fn retained_comment_payloads_have_separate_metadata_storage_cost() {
    let payload = "x".repeat(4096);
    let mut document = Document::new(Limits::default()).unwrap();
    let text = document.push(Doc::Text(&payload)).unwrap();
    let comment = document.push(Doc::Comment(&payload)).unwrap();
    let limits = ProofLimits {
        max_metadata_bytes: 6000,
        max_depth: 1,
        ..ProofLimits::default()
    };
    assert_eq!(
        document.proof_model(text, limits).unwrap().text_bytes(),
        4096
    );
    assert_eq!(
        document.proof_model(comment, limits).unwrap_err(),
        ProofError::Resource("metadata")
    );
}

#[test]
fn zero_and_above_hard_limits_are_rejected_for_every_field() {
    let default = ProofLimits::default();
    let hard = ProofLimits::HARD;
    for limits in [
        ProofLimits {
            max_nodes: 0,
            ..default
        },
        ProofLimits {
            max_text_bytes: 0,
            ..default
        },
        ProofLimits {
            max_depth: 0,
            ..default
        },
        ProofLimits {
            max_metadata_bytes: 0,
            ..default
        },
        ProofLimits {
            max_lean_bytes: 0,
            ..default
        },
        ProofLimits {
            max_work: 0,
            ..default
        },
        ProofLimits {
            max_nodes: hard.max_nodes + 1,
            ..default
        },
        ProofLimits {
            max_text_bytes: hard.max_text_bytes + 1,
            ..default
        },
        ProofLimits {
            max_depth: hard.max_depth + 1,
            ..default
        },
        ProofLimits {
            max_metadata_bytes: hard.max_metadata_bytes + 1,
            ..default
        },
        ProofLimits {
            max_lean_bytes: hard.max_lean_bytes + 1,
            ..default
        },
        ProofLimits {
            max_work: hard.max_work + 1,
            ..default
        },
    ] {
        assert_eq!(limits.validate(), Err(ProofError::InvalidLimits));
    }
    default.validate().unwrap();
    hard.validate().unwrap();
}

#[test]
fn depth_work_and_metadata_failure_leave_the_actual_document_usable() {
    let mut document = Document::new(Limits::default()).unwrap();
    let mut root = document.push(Doc::Comment("/* intact */")).unwrap();
    for _ in 0..10 {
        root = document.push(Doc::Group(root)).unwrap();
    }
    let before = document.render(root).unwrap();
    for (limits, error) in [
        (
            ProofLimits {
                max_depth: 4,
                ..ProofLimits::default()
            },
            ProofError::Resource("depth"),
        ),
        (
            ProofLimits {
                max_work: 1,
                ..ProofLimits::default()
            },
            ProofError::Resource("work"),
        ),
        (
            ProofLimits {
                max_metadata_bytes: 1,
                ..ProofLimits::default()
            },
            ProofError::Resource("metadata"),
        ),
    ] {
        assert_eq!(document.proof_model(root, limits).unwrap_err(), error);
        assert_eq!(document.render(root).unwrap(), before);
    }
    assert_eq!(
        document
            .proof_model(root, ProofLimits::default())
            .unwrap()
            .depth(),
        11
    );
}

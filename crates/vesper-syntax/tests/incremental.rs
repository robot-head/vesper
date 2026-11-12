use std::sync::Arc;
use vesper_syntax::{
    incremental::{SyntaxDocument, UpdateKind},
    parser,
    recovery::{Error, Limits, RecoverySyntax, Resource},
};
fn oracle(document: &SyntaxDocument) {
    let full = parser::parse_recovery(document.source(), Limits::default()).unwrap();
    let actual = document.snapshot();
    assert_eq!(actual.source(), full.source());
    assert_eq!(actual.source_hash(), full.source_hash());
    assert_eq!(actual.syntax().green(), full.syntax().green());
    assert_eq!(actual.diagnostics(), full.diagnostics());
    assert_eq!(actual.declarations(), full.declarations());
    assert_eq!(actual.sites(), full.sites());
}
fn item(snapshot: &RecoverySyntax, index: usize) -> parser::SyntaxNode {
    snapshot
        .syntax()
        .children()
        .filter(|n| n.kind() == parser::ITEM)
        .nth(index)
        .unwrap()
}
fn shared(a: &parser::SyntaxNode, b: &parser::SyntaxNode) -> bool {
    std::ptr::eq(a.green().as_ref(), b.green().as_ref())
}
#[test]
fn body_replacement_reuses_real_untouched_green_siblings_and_matches_full_oracle() {
    let source = "// 🦀 é\r\nfn first()->u64{1}\r\nfn later()->u64{2}\r\n";
    let mut document = SyntaxDocument::open(source, Limits::default()).unwrap();
    let old = Arc::clone(document.snapshot());
    let old_first = item(&old, 0);
    let old_later = item(&old, 1);
    let next = source.replace("{1}", "{12345}");
    assert_eq!(document.update(&next).unwrap(), UpdateKind::BodyReparse);
    oracle(&document);
    assert!(!shared(&old_first, &item(document.snapshot(), 0)));
    assert!(shared(&old_later, &item(document.snapshot(), 1)));
    let old_span = old.declarations()[1].span.clone();
    let new_span = document.snapshot().declarations()[1].span.clone();
    assert_eq!(new_span.start, old_span.start + 4);
    assert_eq!(new_span.end, old_span.end + 4);
    let third = next.replace("12345", "67");
    assert_eq!(document.update(&third).unwrap(), UpdateKind::BodyReparse);
    oracle(&document);
    assert!(shared(&old_later, &item(document.snapshot(), 1)));
}
#[test]
fn later_syntax_diagnostics_sites_receivers_and_owners_shift_with_actual_current_text() {
    let source = "fn good(){1}\r\nfn broken(){tx.}\r\n";
    let mut document = SyntaxDocument::open(source, Limits::default()).unwrap();
    let old = Arc::clone(document.snapshot());
    assert!(!old.sites().is_empty());
    let next = source.replace("{1}", "{123}");
    assert_eq!(document.update(&next).unwrap(), UpdateKind::BodyReparse);
    oracle(&document);
    assert!(shared(&item(&old, 1), &item(document.snapshot(), 1)));
    for (a, b) in old.sites().iter().zip(document.snapshot().sites()) {
        assert_eq!(a.owner, b.owner);
        assert_eq!(b.span.start, a.span.start + 2);
    }
}
#[test]
fn complete_body_can_be_reused_beside_earlier_damaged_syntax_without_stale_ast() {
    let source = "fn broken(){tx.}\nfn good(){1}\nfn later(){2}";
    let mut document = SyntaxDocument::open(source, Limits::default()).unwrap();
    let old = Arc::clone(document.snapshot());
    let next = source.replace("good(){1}", "good(){3}");
    assert_eq!(document.update(&next).unwrap(), UpdateKind::BodyReparse);
    oracle(&document);
    assert!(shared(&item(&old, 0), &item(document.snapshot(), 0)));
    assert!(shared(&item(&old, 2), &item(document.snapshot(), 2)));
}
#[test]
fn header_generic_delimiter_and_lexical_uncertainty_use_real_full_fallback() {
    for (source, next) in [
        ("fn f()->u64{1}", "fn f()->i64{1}"),
        (
            "fn f[T:mod copy](sink x:T)->T{x}",
            "fn f[T:mod copy](sink x:T)->T{y}",
        ),
        ("fn f(){1}", "fn f(){(1)}"),
        ("fn f(){1}", "fn f(){tx.}"),
        ("fn f(){0}\nfn later(){}", "fn f(){/*0}\nfn later(){}"),
        (
            "fn f()->String{\"ok\"}\nfn later(){}",
            "fn f()->String{\"ok}\nfn later(){}",
        ),
    ] {
        let mut document = SyntaxDocument::open(source, Limits::default()).unwrap();
        assert_eq!(
            document.update(next).unwrap(),
            UpdateKind::FullReparse,
            "{source} -> {next}"
        );
        oracle(&document);
    }
}
#[test]
fn utf8_and_crlf_minimal_difference_never_slices_interiors() {
    for (source, next) in [
        ("fn f()->String{\"é\"}", "fn f()->String{\"ê\"}"),
        ("fn f(){\r\n1\r\n}", "fn f(){\n1\r\n}"),
        ("fn f(){1}\r\nfn g(){}", "fn f(){1}\nfn g(){}"),
        ("// 😀\r\nfn f(){1}", "// 😁\r\nfn f(){1}"),
    ] {
        let mut document = SyntaxDocument::open(source, Limits::default()).unwrap();
        document.update(next).unwrap();
        oracle(&document);
    }
}
#[test]
fn unchanged_text_preserves_sealed_arc_and_failed_update_keeps_all_old_state() {
    let limits = Limits {
        parse: parser::ParseLimits {
            max_source_bytes: 64,
            ..Limits::default().parse
        },
        ..Limits::default()
    };
    let source = "fn f(){1}";
    let mut document = SyntaxDocument::open(source, limits).unwrap();
    let old = Arc::clone(document.snapshot());
    assert_eq!(document.update(source).unwrap(), UpdateKind::Unchanged);
    assert!(Arc::ptr_eq(&old, document.snapshot()));
    let kind = document.last_update_kind();
    assert_eq!(
        document.update(&"x".repeat(65)).unwrap_err(),
        Error::Budget(Resource::Source)
    );
    assert_eq!(document.source(), source);
    assert!(Arc::ptr_eq(&old, document.snapshot()));
    assert_eq!(document.last_update_kind(), kind);
}
#[test]
fn repeated_actual_edits_match_full_recovery_including_refusal_and_repair() {
    let mut document = SyntaxDocument::open("fn f(){1}\nfn other(){}", Limits::default()).unwrap();
    for n in 0..24 {
        let source = if n % 6 == 0 {
            format!("fn f(){{tx.}}\nfn other(){{{n}}}")
        } else {
            format!("fn f(){{{n}}}\nfn other(){{}}")
        };
        document.update(&source).unwrap();
        oracle(&document);
    }
}

#[test]
fn real_parameter_context_is_preserved_and_fragment_has_no_extra_depth_privilege() {
    let source = "fn f(in arr:fn(u64)->u64,in idx:u64){arr[idx](1);}\nfn next(){}";
    let mut document = SyntaxDocument::open(source, Limits::default()).unwrap();
    let next = source.replace("(1);", "(2);");
    assert_eq!(document.update(&next).unwrap(), UpdateKind::BodyReparse);
    oracle(&document);
    let source = "fn f(){0}";
    let mut document = SyntaxDocument::open(source, Limits::default()).unwrap();
    let old = Arc::clone(document.snapshot());
    let next = format!("fn f(){{{}0}}", "!".repeat(100));
    assert_eq!(
        parser::parse_recovery(&next, Limits::default()).unwrap_err(),
        Error::Budget(Resource::Depth)
    );
    assert_eq!(
        document.update(&next).unwrap_err(),
        Error::Budget(Resource::Depth)
    );
    assert!(Arc::ptr_eq(&old, document.snapshot()));
}
#[test]
fn failed_fast_attempt_and_fallback_share_one_work_allowance() {
    let old = "fn f(){1}";
    let new = "fn f(){tx.}";
    // Find a bounded real-parser profile that admits each standalone source.
    // The update must still pay for its discarded fast attempt before fallback.
    let mut demonstrated = false;
    for max_work in (128..=8192).step_by(64) {
        let limits = Limits {
            max_work,
            ..Limits::default()
        };
        let Ok(mut document) = SyntaxDocument::open(old, limits) else {
            continue;
        };
        if parser::parse_recovery(new, limits).is_err() {
            continue;
        }
        let before = Arc::clone(document.snapshot());
        if document.update(new) == Err(Error::Budget(Resource::Work)) {
            assert!(Arc::ptr_eq(&before, document.snapshot()));
            assert_eq!(document.source(), old);
            demonstrated = true;
            break;
        }
    }
    assert!(
        demonstrated,
        "the fast/fallback work profile was reset instead of composed"
    );
}

use vesper_syntax::{
    parser,
    recovery::{DeclarationStatus as DS, Limits, SiteKind},
};
#[test]
fn real_source_siblings_and_missing_member_are_preserved() {
    let source = "// 🦀\nfn broken(){let tx=1;tx.}\nfn sibling()->u64{42}\n";
    let recovered = parser::parse_recovery(source, Limits::default()).unwrap();
    assert_eq!(recovered.source(), source);
    assert_eq!(recovered.syntax().text().to_string(), source);
    assert_eq!(recovered.declarations().len(), 2);
    assert_eq!(recovered.declarations()[0].status, DS::Recovered);
    assert_eq!(recovered.declarations()[1].status, DS::Complete);
    assert!(recovered.sites().iter().any(|s| {
        s.kind == SiteKind::MissingMember
            && s.receiver
                .as_ref()
                .is_some_and(|r| &source[r.root.clone()] == "tx")
    }));
    assert!(
        recovered
            .syntax()
            .descendants()
            .any(|n| n.kind() == parser::INCOMPLETE)
    );
    let module = parser::parse_complete_items(&recovered, &[1], Limits::default()).unwrap();
    assert_eq!(module.items.len(), 1);
    let vesper_syntax::ast::Item::Function(f) = &module.items[0] else {
        panic!()
    };
    assert_eq!(f.name, "sibling");
    assert_eq!(&source[f.span.clone()], "fn sibling()->u64{42}");
}
#[test]
fn complete_bodies_have_real_block_ranges_and_original_absolute_ast_spans() {
    let source = "fn first(in x:u64)->u64{x}\nfn second()->u64{42}";
    let recovered = parser::parse_recovery(source, Limits::default()).unwrap();
    assert!(recovered.diagnostics().is_empty());
    for d in recovered.declarations() {
        assert_eq!(d.status, DS::Complete);
        assert_eq!(d.body_green_span, d.body_span);
    }
    let actual = parser::parse_complete_items(&recovered, &[0, 1], Limits::default()).unwrap();
    assert_eq!(actual, parser::parse(source).module);
}
#[test]
fn statement_anchors_continue_after_missing_member_without_exposing_partial_ast() {
    let source = "fn broken(){tx. let after=1; after}\nfn sibling()->u64{42}";
    let r = parser::parse_recovery(source, Limits::default()).unwrap();
    assert_eq!(r.syntax().text().to_string(), source);
    assert_eq!(r.declarations()[0].status, DS::Recovered);
    assert!(
        r.syntax()
            .descendants()
            .any(|n| n.kind() == parser::PATTERN && n.text().to_string().contains("after"))
    );
    assert_eq!(r.declarations()[1].status, DS::Complete);
    assert!(parser::parse_complete_items(&r, &[0], Limits::default()).is_err());
}
#[test]
fn unsupported_pub_function_prefix_stays_an_explicit_opaque_context() {
    let r = parser::parse_recovery("pub fn f(){} fn g(){}", Limits::default()).unwrap();
    assert_eq!(r.declarations()[0].status, DS::Opaque);
    assert_eq!(
        r.declarations()[0].kind,
        vesper_syntax::recovery::DeclarationKind::Other
    );
}
#[test]
fn mode_prefixed_types_and_unambiguous_function_siblings_have_valid_inventory() {
    let source = "// 🦀 é\r\nlinear struct Tx{}\r\nfn good()->u64{1}\r\nfn damaged(sink tx:Tx){tx.}\r\nfn leak(sink other:Tx){}\r\n";
    let r = parser::parse_recovery(source, Limits::default()).unwrap();
    assert_eq!(r.declarations().len(), 4);
    assert_eq!(r.declarations()[0].status, DS::Complete);
    assert_eq!(
        &source[r.declarations()[0].span.clone()],
        "linear struct Tx{}"
    );
    assert_eq!(r.declarations()[2].status, DS::Recovered);
    assert_eq!(r.declarations()[3].status, DS::Complete);
}
#[test]
fn complete_impl_and_model_methods_are_not_top_level_functions_or_headers() {
    for source in [
        "copy struct S{x:u64} impl S{fn get(in self)->u64{self.x}} fn after(){}",
        "copy struct S{x:u64} trait Get{fn get(in self)->u64;} model M:Get for S{fn get(in self)->u64{self.x}} fn after(){}",
    ] {
        let r = parser::parse_recovery(source, Limits::default()).unwrap();
        assert_eq!(r.syntax().text().to_string(), source);
        assert!(r.declarations().iter().all(|d| d.status == DS::Complete));
        assert_eq!(
            r.declarations()
                .iter()
                .filter(|d| d.kind == vesper_syntax::recovery::DeclarationKind::Function)
                .count(),
            1
        );
    }
}
#[test]
fn unclosed_effect_and_impl_do_not_guess_methods_as_global_siblings() {
    for source in [
        "effect E{fn broken(; fn maybe()->u64{42}",
        "impl S{fn broken(){x. fn maybe()->u64{42}",
    ] {
        let r = parser::parse_recovery(source, Limits::default()).unwrap();
        assert_eq!(r.syntax().text().to_string(), source);
        assert_eq!(r.declarations().len(), 1);
        assert_eq!(r.declarations()[0].status, DS::Opaque);
    }
}

#[test]
fn legacy_self_names_and_projections_retain_actual_token_ranges() {
    let source = "fn self(){} fn broken(){tx.self.}";
    let r = parser::parse_recovery(source, Limits::default()).unwrap();
    assert_eq!(r.syntax().text().to_string(), source);
    assert_eq!(r.declarations()[0].status, DS::Complete);
    let site = r
        .sites()
        .iter()
        .find(|s| s.kind == SiteKind::MissingMember)
        .unwrap();
    assert_eq!(site.span.start, site.span.end);
    assert_eq!(site.owner, Some(1));
    let receiver = site.receiver.as_ref().unwrap();
    assert_eq!(&source[receiver.root.clone()], "tx");
    assert_eq!(receiver.projections.len(), 1);
    assert_eq!(&source[receiver.projections[0].clone()], "self");
}

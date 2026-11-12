use std::collections::BTreeSet;
use vesper_forge::typed_quote::*;
use vesper_forge::*;
const PKG: PackageId = PackageId(1);
const MAC: DefId = DefId {
    package: PKG,
    item: 0,
};
const HELP: DefId = DefId {
    package: PKG,
    item: 1,
};
fn host() -> HygieneHost {
    let mut r = DefinitionRegistry::new();
    r.add_package(PackageInfo {
        id: PKG,
        name: "app".into(),
        dependencies: BTreeSet::new(),
    })
    .unwrap();
    for (id, name, kind) in [
        (MAC, "keep", DefKind::Macro),
        (HELP, "helper", DefKind::Function),
    ] {
        r.add_definition(DefInfo {
            id,
            path: name.into(),
            kind,
            visibility: Visibility::Public,
        })
        .unwrap();
    }
    let mut h = HygieneHost::new(SessionId(9), r);
    h.declare_global(PKG, "helper", HELP).unwrap();
    h
}
fn budget() -> TypedBudget {
    TypedBudget::new(TypedLimits::default()).unwrap()
}
fn items(src: &str) -> Vec<ExprQuoteItem> {
    lex_fragment(src)
        .into_iter()
        .map(ExprQuoteItem::Generated)
        .collect()
}
fn span(source: &str, part: &str) -> std::ops::Range<usize> {
    let i = source.find(part).unwrap();
    i..i + part.len()
}
fn local(fragment: &ResolvedFragment, token: usize) -> usize {
    let Resolution::Local(id) = fragment.resolution_at(token).unwrap() else {
        panic!()
    };
    fragment.binder(id).unwrap().token
}
#[test]
fn whole_body_preserves_caller_local_through_generated_binder_and_actual_spans() {
    let mut h = host();
    let mut b = budget();
    let text = "// 🦀\r\nfn main()->u64{let x=42;keep!(x)}";
    let s = h.import_expr_source(PKG, text, &mut b).unwrap();
    let input = h
        .expr_input(
            &s,
            span(text, "x)").start..span(text, "x)").start + 1,
            &mut b,
        )
        .unwrap();
    assert!(
        matches!(&input.provenance()[0],TokenProvenance::Source{span:s} if &text[s.clone()]=="x")
    );
    let e = h
        .begin_expr_expansion(MAC, InvocationSite::Source(PKG), vec![input], &mut b)
        .unwrap();
    let mut q = items("{let x=1;");
    q.push(ExprQuoteItem::InputSlot(0));
    q.extend(items("}"));
    let out = h.finish_expr(e, &q, &mut b).unwrap();
    let body = span(text, "{let x=42;keep!(x)}");
    let r = h
        .resolve_expr_body(&s, body, vec![(span(text, "keep!(x)"), out)], &mut b)
        .unwrap();
    r.validate_host(&h).unwrap();
    let f = r.fragment();
    assert_eq!(f.binders.len(), 2);
    let caller = f
        .references
        .iter()
        .find(|r| matches!(r.origin, Origin::Source { .. }))
        .unwrap();
    assert_eq!(local(f, caller.token), f.binders[0].token);
    assert!(matches!(f.binders[1].origin, Origin::Expansion(_)));
    let mut other = host();
    assert!(r.validate_host(&other).is_err()); // Numeric SessionId intentionally identical.
    let s2 = other.import_expr_source(PKG, "1", &mut budget()).unwrap();
    assert!(h.expr_input(&s2, 0..1, &mut b).is_err());
}
#[test]
fn template_atom_grouping_prevents_callee_boundary_drift_and_preserves_unit() {
    let mut h = host();
    let mut b = budget();
    let text = "{let a=1;a}";
    let s = h.import_expr_source(PKG, text, &mut b).unwrap();
    let input = h.expr_input(&s, 0..text.len(), &mut b).unwrap();
    let e = h
        .begin_expr_expansion(MAC, InvocationSite::Source(PKG), vec![input], &mut b)
        .unwrap();
    let mut q = vec![ExprQuoteItem::InputSlot(0)];
    q.extend(items("(2)"));
    let out = h.finish_expr(e, &q, &mut b).unwrap();
    assert!(
        matches!(out.shape(),ExprShape::Call{callee,..} if matches!(&**callee,ExprShape::Block{..}))
    );
    for (text, tail) in [
        ("{1}", true),
        ("{1;}", false),
        ("{let x=1;}", false),
        ("{}", false),
    ] {
        let s = h.import_expr_source(PKG, text, &mut b).unwrap();
        let r = h
            .resolve_expr_body(&s, 0..text.len(), vec![], &mut b)
            .unwrap();
        assert!(matches!(r.shape(),ExprShape::Block{tail:t,..} if t.is_some()==tail));
    }
}
#[test]
fn source_template_pre_resolves_globals_and_excludes_own_initializer() {
    let mut h = host();
    let mut b = budget();
    let text = "macro keep($value:expr)->expr{quote{{let helper=helper;helper($value)}}} fn main()->i64{let helper=9;keep!(42)}";
    let s = h.import_expr_source(PKG, text, &mut b).unwrap();
    let p = span(text, "value");
    let quote = span(text, "{let helper=helper;helper($value)}");
    let q = h.expr_quotation(&s, MAC, quote, &[p], &mut b).unwrap();
    let claims = q
        .items()
        .iter()
        .filter_map(|i| match i {
            ExprQuoteItem::Generated(g) => g.claimed_def,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(claims, vec![HELP]);
    let input = h.expr_input(&s, span(text, "42"), &mut b).unwrap();
    let e = h
        .begin_expr_expansion(MAC, InvocationSite::Source(PKG), vec![input], &mut b)
        .unwrap();
    let out = h.finish_expr(e, q.items(), &mut b).unwrap();
    let r = h
        .resolve_expr_body(
            &s,
            span(text, "{let helper=9;keep!(42)}"),
            vec![(span(text, "keep!(42)"), out)],
            &mut b,
        )
        .unwrap();
    assert!(
        r.fragment()
            .references
            .iter()
            .any(|r| r.pre_resolved && r.resolution == Resolution::Global(HELP))
    );
}
#[test]
fn rejected_slot_positions_and_generated_token_lies_close_expansions() {
    for q in [
        vec![ExprQuoteItem::InputSlot(1)],
        vec![ExprQuoteItem::Generated(GeneratedToken::ident("x y"))],
        vec![ExprQuoteItem::Generated(GeneratedToken::new(
            TokenKind::Int,
            "99999999999999999999999",
        ))],
        {
            let mut q = items("{let ");
            q.push(ExprQuoteItem::InputSlot(0));
            q.extend(items("=1;1}"));
            q
        },
        {
            let mut q = items("helper::");
            q.push(ExprQuoteItem::InputSlot(0));
            q
        },
    ] {
        let mut h = host();
        let mut b = budget();
        let s = h.import_expr_source(PKG, "1", &mut b).unwrap();
        let input = h.expr_input(&s, 0..1, &mut b).unwrap();
        let e = h
            .begin_expr_expansion(MAC, InvocationSite::Source(PKG), vec![input], &mut b)
            .unwrap();
        let id = e.id();
        assert!(h.finish_expr(e, &q, &mut b).is_err());
        assert!(matches!(
            h.finish_quotation(id, &Quotation::new().text("1")),
            Err(ForgeError::ExpansionClosed(_))
        ));
    }
}
#[test]
fn repetition_is_charged_before_materialization_and_failures_exhaust_shared_work() {
    let mut h = host();
    let mut b = budget();
    let s = h.import_expr_source(PKG, "12345", &mut b).unwrap();
    let input = h.expr_input(&s, 0..5, &mut b).unwrap();
    let e = h
        .begin_expr_expansion(MAC, InvocationSite::Source(PKG), vec![input], &mut b)
        .unwrap();
    let id = e.id();
    let limits = TypedLimits {
        max_output_tokens: 5,
        ..TypedLimits::default()
    };
    let mut tight = TypedBudget::new(limits).unwrap();
    let mut q = items("helper(");
    q.push(ExprQuoteItem::InputSlot(0));
    q.extend(items(","));
    q.push(ExprQuoteItem::InputSlot(0));
    q.extend(items(")"));
    assert!(matches!(
        h.finish_expr(e, &q, &mut tight),
        Err(TypedError::Budget(_))
    ));
    assert!(matches!(h.abandon(id), Err(ForgeError::ExpansionClosed(_))));
    let mut b = budget();
    assert!(b.charge_work(usize::MAX).is_err());
    assert_eq!(b.remaining_work(), 0);
}

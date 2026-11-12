use vesper::{
    ast::*,
    check,
    check::Place,
    ir::{self, PlaceGraph},
    parser,
};
#[test]
fn generated_invalid_text_is_lossless() {
    let alphabet = b"fn let {}()[],;:@|?+-*/abcXYZ019\n\t";
    let mut rng = 123456789u64;
    for _ in 0..1000 {
        let mut s = String::new();
        for _ in 0..80 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            s.push(alphabet[rng as usize % alphabet.len()] as char);
        }
        let p = parser::parse(&s);
        assert_eq!(p.syntax().text().to_string(), s);
    }
}
#[test]
fn generic_vs_index() {
    let p = parser::parse("fn main() { foo[u64](1); arr[idx](2); }");
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let Item::Function(f) = &p.module.items[0] else {
        panic!()
    };
    let ExprKind::Block { statements, .. } = &f.body.as_ref().unwrap().kind else {
        panic!()
    };
    let Statement::Expr(Expr {
        kind: ExprKind::Call { types, .. },
        ..
    }) = &statements[0]
    else {
        panic!()
    };
    assert_eq!(types, &vec![Type::named("u64")]);
    let Statement::Expr(Expr {
        kind: ExprKind::Call { callee, types, .. },
        ..
    }) = &statements[1]
    else {
        panic!()
    };
    assert!(types.is_empty());
    assert!(matches!(callee.kind, ExprKind::Index(..)));
}
#[test]
fn place_graph_ancestry() {
    let mut g = PlaceGraph::default();
    let root = Place {
        root: "broker".into(),
        path: vec![],
    };
    let left = Place {
        root: "broker".into(),
        path: vec!["index".into()],
    };
    let right = Place {
        root: "broker".into(),
        path: vec!["config".into()],
    };
    for p in [&root, &left, &right] {
        g.insert(p);
    }
    assert!(g.overlaps(&root, &left));
    assert!(!g.overlaps(&left, &right));
    assert_eq!(g.roots(), vec!["broker"]);
}
#[test]
fn cfg_and_tokens() {
    let p = parser::parse(
        "fn inc(inout x: i64) { x += 1; } fn main() -> i64 { let x = 0; if true { inc(inout x); } else { x = 2; }; x }",
    );
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let program = ir::lower(&c);
    let f = &program.functions["main"];
    assert!(f.blocks.len() >= 4);
    let tokens = f
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter_map(|i| i.token)
        .collect::<Vec<_>>();
    assert!(tokens.len() >= 2);
    assert!(tokens.iter().any(|(a, b)| a != b));
    assert!(vesper::ir_check::evidence(&program).is_empty());
}

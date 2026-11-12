//! Causal metadata from actual source-checker events, with unchanged primaries.
use vesper::{ast::Diagnostic, check, parser};
fn errors(source: &str) -> Vec<Diagnostic> {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    check::check(&parsed.module).diagnostics
}
fn one<'a>(diagnostics: &'a [Diagnostic], code: &str, message: &str) -> &'a Diagnostic {
    diagnostics
        .iter()
        .find(|d| d.code == code && d.message == message)
        .unwrap_or_else(|| panic!("missing {code}: {message}; {diagnostics:?}"))
}
fn span(source: &str, needle: &str) -> std::ops::Range<usize> {
    let start = source.find(needle).unwrap();
    start..start + needle.len()
}
fn texts<'a>(source: &'a str, d: &Diagnostic) -> Vec<&'a str> {
    d.related.iter().map(|l| &source[l.span.clone()]).collect()
}
fn validate(source: &str, diagnostics: &[Diagnostic]) {
    for d in diagnostics {
        assert!(source.get(d.span.clone()).is_some(), "{d:?}");
        assert!(d.related.len() <= 8);
        for l in &d.related {
            assert!(source.get(l.span.clone()).is_some());
            assert!(!l.message.is_empty());
        }
        if let Some(help) = &d.help {
            assert!(!help.contains(".abort()"));
            assert!(!help.contains(".commit()"));
        }
    }
}
#[test]
fn linear_exhaustion_and_shadowing_label_the_actual_old_declaration() {
    let source = "linear struct Tx{} fn bad(sink tx:Tx){}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let d = one(
        &diagnostics,
        "E0302",
        "linear value 'tx' must be consumed exactly once; destructure it or pass it to a sink parameter",
    );
    assert_eq!(d.span, source.len() - 2..source.len());
    assert_eq!(texts(source, d), ["sink tx:Tx"]);
    assert_eq!(d.related[0].message, "linear binding introduced here");
    assert!(d.help.is_some());
    let source = "linear struct Tx{} fn bad(sink tx:Tx){let tx=Tx{};let Tx{}=tx;}";
    let diagnostics = errors(source);
    let d = one(
        &diagnostics,
        "E0302",
        "shadowing would discard linear value 'tx'",
    );
    assert_eq!(texts(source, d), ["sink tx:Tx"]);
    assert_eq!(&source[d.span.clone()], "tx");
}
#[test]
fn moved_place_labels_unicode_declaration_and_real_earlier_consumption() {
    let source = "// 🦀 é\r\nlinear struct Tx{} fn bad(sink tx:Tx){let Tx{}=tx;let Tx{}=tx;}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let d = one(&diagnostics, "E0303", "use of moved place 'tx'");
    let previous = source.find("=tx;").unwrap() + 1;
    let current = source.rfind("=tx;").unwrap() + 1;
    assert_eq!(d.span, current..current + 2);
    assert_eq!(d.related[0].span, span(source, "sink tx:Tx"));
    assert_eq!(d.related[1].span, previous..previous + 2);
    assert_eq!(
        d.related[1].message,
        "earlier ownership-taking use of this place"
    );
    assert!(d.help.as_ref().unwrap().contains("moved non-copy"));
}
#[test]
fn field_provenance_matches_only_overlapping_places_and_keeps_disjoint_moves_valid() {
    let source =
        "struct Pair{left:String,right:String} fn bad(sink p:Pair){let a=p.left;let b=p.left;}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let d = one(&diagnostics, "E0303", "use of moved place 'p.left'");
    assert_eq!(texts(source, d), ["sink p:Pair", "p.left"]);
    assert_eq!(d.related[1].span, span(source, "p.left"));
    let valid =
        "struct Pair{left:String,right:String} fn good(sink p:Pair){let a=p.left;let b=p.right;}";
    assert!(errors(valid).is_empty());
}
#[test]
fn shadowed_inner_binding_never_inherits_outer_move_evidence() {
    let source =
        "linear struct Tx{} fn bad(sink tx:Tx){let Tx{}=tx;let tx=Tx{};let Tx{}=tx;let Tx{}=tx;}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let d = one(&diagnostics, "E0303", "use of moved place 'tx'");
    let binding = source.find("let tx=Tx{}").unwrap() + 4;
    let consumption = source[binding..].find("let Tx{}=tx;").unwrap() + binding + "let Tx{}=".len();
    assert_eq!(d.related[0].span, binding..binding + 2);
    assert_eq!(d.related[1].span, consumption..consumption + 2);
    assert!(
        !d.related
            .iter()
            .any(|l| l.span == span(source, "sink tx:Tx"))
    );
}
#[test]
fn branch_metadata_records_actual_consumption_without_affecting_join_rules() {
    let source = "linear struct Tx{} fn bad(sink tx:Tx,in yes:bool){if yes{let Tx{}=tx;}else{};let Tx{}=tx;}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let d = one(
        &diagnostics,
        "E0309",
        "linear value 'tx' is consumed on only some control-flow paths",
    );
    assert_eq!(texts(source, d), ["sink tx:Tx", "tx"]);
    assert_eq!(&source[d.span.clone()], "if yes{let Tx{}=tx;}else{}");
    let d = one(&diagnostics, "E0303", "use of moved place 'tx'");
    let moved = source.find("let Tx{}=tx;").unwrap() + "let Tx{}=".len();
    assert_eq!(d.related[1].span, moved..moved + 2);
    assert!(errors("linear struct Tx{} fn good(sink tx:Tx,in yes:bool){if yes{let Tx{}=tx;}else{let Tx{}=tx;};}").is_empty());
}
#[test]
fn alias_and_active_loan_errors_point_to_the_actual_earlier_argument() {
    let source = "copy struct Pair{left:i64,right:i64} fn both(inout a:Pair,in b:Pair){} fn bad(inout p:Pair){both(inout p,in p);}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let d = one(
        &diagnostics,
        "E0301",
        "overlapping arguments 'p' and 'p' violate alias XOR mutability",
    );
    let first = source.find("both(inout p").unwrap() + "both(inout ".len();
    let second = source.find(",in p").unwrap() + ",in ".len();
    assert_eq!(d.span, second..second + 1);
    assert_eq!(d.related[0].span, first..first + 1);
    let d = one(
        &diagnostics,
        "E0301",
        "access to 'p' conflicts with active argument loan 'p'",
    );
    assert_eq!(d.related[0].span, first..first + 1);
    assert!(errors("copy struct Pair{left:i64,right:i64} fn both(inout a.{left}:Pair,in b.{right}:Pair){} fn good(inout p:Pair){both(inout p,in p);}").is_empty());
}
#[test]
fn borrowed_noncopy_move_labels_borrow_declaration_not_a_fabricated_move() {
    let source = "fn bad(in x:String){let y=x;}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let d = one(
        &diagnostics,
        "E0305",
        "cannot move 'x' through a borrowed parameter",
    );
    assert_eq!(texts(source, d), ["in x:String"]);
    assert_eq!(&source[d.span.clone()], "x");
    assert_eq!(d.related[0].message, "borrowed parameter declared here");
}
#[test]
fn abortable_checkpoint_labels_live_linear_and_active_inout_origins() {
    let source = "linear struct Tx{} effect Ask{fn ask();} fn bad(sink tx:Tx) eff[Ask]{Ask::ask();let Tx{}=tx;}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let d = one(
        &diagnostics,
        "E0402",
        "potentially abortable effect 'Ask' may discard live linear value 'tx'",
    );
    assert_eq!(d.span, span(source, "Ask::ask()"));
    assert_eq!(texts(source, d), ["sink tx:Tx"]);
    assert!(errors("linear struct Tx{} linear effect Ask{fn ask();} fn good(sink tx:Tx) eff[Ask]{Ask::ask();let Tx{}=tx;}").is_empty());
    let source = "effect Ask{fn ask()->i64;} fn both(inout x:i64,in y:i64){} fn bad(inout x:i64) eff[Ask]{both(inout x,Ask::ask());}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let d = one(
        &diagnostics,
        "E0402",
        "abortable effect crosses an active inout argument loan",
    );
    let argument = source.find("both(inout x,").unwrap() + "both(inout ".len();
    assert_eq!(d.related[0].span, argument..argument + 1);
    let d = one(
        &diagnostics,
        "E0402",
        "potentially abortable effect 'Ask' crosses active inout parameter 'x'",
    );
    assert_eq!(texts(source, d), ["inout x:i64"]);
}
#[test]
fn abortable_checkpoint_labels_actual_linear_handler_state_initializer() {
    let source = "linear struct Ticket{} linear effect E{fn op()->i64;} fn bad(b:i64)->i64 eff[Fail[RuntimeError]]{handle E state ticket=Ticket{} in {1/b} with {return (x,ticket)=>{let Ticket{}=ticket;x},op(ticket,k)=>k(1,ticket),}}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let d = one(
        &diagnostics,
        "E0402",
        "abortable effect 'Fail' may discard linear handler state Ticket",
    );
    assert_eq!(d.span, span(source, "1/b"));
    assert_eq!(texts(source, d), ["Ticket{}"]);
    assert_eq!(
        d.related[0].message,
        "linear handler state initialized here"
    );
}
#[test]
fn evidence_caps_and_primary_deduplication_do_not_claim_complete_history() {
    let source =
        "struct Item{} fn bad(sink x:Item){let a=x;let b=x;let c=x;let d=x;let e=x;let f=x;}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let last = diagnostics.iter().rfind(|d| d.code == "E0303").unwrap();
    assert_eq!(last.related.len(), 2); // declaration + first valid transfer only
    assert_eq!(
        last.related[1].span,
        span(source, "x;").start..span(source, "x;").start + 1
    );
    let mut keys = std::collections::BTreeSet::new();
    for d in &diagnostics {
        assert!(keys.insert((&d.code, &d.message, d.span.start, d.span.end)));
    }
    let parameters = (0..10)
        .map(|i| format!("inout p{i}:i64"))
        .collect::<Vec<_>>()
        .join(",");
    let args = (0..10)
        .map(|i| format!("inout p{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        "effect Ask{{fn ask()->i64;}} fn all({parameters},in y:i64){{}} fn bad({parameters}) eff[Ask]{{all({args},Ask::ask());}}"
    );
    let diagnostics = errors(&source);
    validate(&source, &diagnostics);
    let d = one(
        &diagnostics,
        "E0402",
        "abortable effect crosses an active inout argument loan",
    );
    assert_eq!(d.related.len(), 8);
    assert!(
        d.related
            .iter()
            .all(|l| source[l.span.clone()].starts_with('p'))
    );
}

#[test]
fn overlapping_projections_of_one_argument_do_not_claim_an_earlier_argument() {
    let source = "copy struct Pair{left:i64,right:i64} fn same(inout x.{left,left}:Pair){} fn bad(inout p:Pair){same(inout p);}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let d = one(
        &diagnostics,
        "E0301",
        "overlapping arguments 'p.left' and 'p.left' violate alias XOR mutability",
    );
    assert_eq!(d.related[0].span, d.span);
    assert_eq!(
        d.related[0].message,
        "overlapping projection of this argument introduced here"
    );
}

#[test]
fn rejected_moves_are_not_successful_transfer_witnesses() {
    for source in [
        "fn bad(in x:String){let a=x;let b=x;}",
        "linear struct L{} fn bad(in x:L,in yes:bool){if yes{let L{}=x;}else{};let L{}=x;}",
        "linear struct L{field:String} fn bad(sink x:L){let a=x.field;let b=x.field;}",
        "struct A{} fn both(in x:A,sink y:A){} fn bad(sink x:A){both(in x,sink x);let z=x;}",
    ] {
        let diagnostics = errors(source);
        validate(source, &diagnostics);
        for d in diagnostics.iter().filter(|d| d.code == "E0303") {
            assert_eq!(
                d.related.len(),
                1,
                "rejected move acquired a transfer witness: {d:?}"
            );
        }
        assert!(diagnostics.iter().any(|d| d.code == "E0303"));
    }
}
#[test]
fn distinct_valid_field_moves_are_capped_and_missing_evidence_stays_absent() {
    let source = "struct Many{a:String,b:String,c:String,d:String,e:String} fn bad(sink x:Many){let a=x.a;let b=x.b;let c=x.c;let d=x.d;let e=x.e;let again=x.e;let twice=x.a;}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let e = one(&diagnostics, "E0303", "use of moved place 'x.e'");
    assert_eq!(e.related.len(), 1); // fifth valid transfer was deliberately not retained
    let a = one(&diagnostics, "E0303", "use of moved place 'x.a'");
    assert_eq!(a.related[1].span, span(source, "x.a"));
}
#[test]
fn nested_block_moves_preserve_origin_and_writes_do_not_reinitialize_recovery_state() {
    let source = "struct A{} fn bad(sink x:A){{let a=x;};let b=x;}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let d = one(&diagnostics, "E0303", "use of moved place 'x'");
    let start = source.find("=x;").unwrap() + 1;
    assert_eq!(d.related[1].span, start..start + 1);
    // The unchanged checker rejects assignment to an already moved place; it
    // does not implement reinitialization. Do not invent a new initialized state
    // or turn that rejected write into an ownership-transfer witness.
    let source = "fn bad(){let x=\"old\";let a=x;x=\"new\";let b=x;}";
    let diagnostics = errors(source);
    validate(source, &diagnostics);
    let ds = diagnostics
        .iter()
        .filter(|d| d.code == "E0303")
        .collect::<Vec<_>>();
    assert_eq!(ds.len(), 2);
    let start = source.find("=x;").unwrap() + 1;
    for d in ds {
        assert_eq!(d.related.len(), 2);
        assert_eq!(d.related[1].span, start..start + 1);
    }
}

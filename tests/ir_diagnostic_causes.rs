use vesper::{
    ast::{Diagnostic, Param, Pass, Span, Type},
    check::{self, Checked},
    ir::{self, Block, Instruction, Op, Program, Terminator, ValueId},
    ir_check, parser,
};
fn lower(source: &str) -> (Checked, Program) {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let program = ir::lower(&checked);
    assert!(ir_check::validate(&checked.world, &program).is_empty());
    (checked, program)
}
fn flow(checked: &Checked, program: &mut Program) -> Vec<Diagnostic> {
    ir::thread_tokens(program);
    assert!(ir_check::evidence(program).is_empty());
    ir_check::validate(&checked.world, program)
        .into_iter()
        .filter(|d| d.code == "E0410")
        .collect()
}
fn labels(source: &str, diagnostics: &[Diagnostic]) {
    for d in diagnostics.iter().filter(|d| d.code == "E0410") {
        assert!(d.related.len() <= 3);
        let mut spans = std::collections::BTreeSet::new();
        for label in &d.related {
            assert!(label.span.start < label.span.end);
            assert!(source.get(label.span.clone()).is_some());
            assert!(spans.insert((label.span.start, label.span.end)));
            assert_ne!(label.span, d.span);
        }
    }
}
#[test]
fn actual_source_linear_temporary_abort_has_real_unicode_definition_cause() {
    let source = "// 🦀 é\nlinear struct Tx {} fn consume(sink tx: Tx, number: i64) { let Tx {} = tx; } fn bad() eff [Fail[i64]] { consume(Tx {}, Fail::raise(1)); }";
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty());
    let checked = check::check(&parsed.module);
    let d = checked
        .diagnostics
        .iter()
        .find(|d| {
            d.code == "E0410"
                && d.message
                    .starts_with("abortable effect may abandon linear temporary %")
        })
        .unwrap();
    assert_eq!(&source[d.span.clone()], "Fail::raise(1)");
    assert!(
        d.related
            .iter()
            .any(|l| l.message == "this IR temporary is defined here"
                && &source[l.span.clone()] == "Tx {}")
    );
    assert!(d.help.as_deref().unwrap().contains("abortable"));
    labels(source, &checked.diagnostics);
    // Actual lowering reproduces the same primary diagnostic and source cause.
    let program = ir::lower(&checked);
    let again = ir_check::validate(&checked.world, &program);
    assert!(again.iter().any(|other| other.code == d.code
        && other.message == d.message
        && other.span == d.span
        && other.related == d.related));
}
const DOUBLE: &str = "// 😀\nlinear struct Tx{} fn drop(sink tx:Tx){let Tx{}=tx;} fn f(sink first:Tx,sink second:Tx){drop(first);drop(second);}";
fn double_consume() -> (Checked, Program, ValueId, Span, Span, Span) {
    let (checked, mut program) = lower(DOUBLE);
    let f = program.functions.get_mut("f").unwrap();
    let calls: Vec<_> = f.blocks[0]
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(n, i)| matches!(&i.op,Op::Call {callee,..} if callee=="drop").then_some(n))
        .collect();
    let first = f.blocks[0].instructions[calls[0]].clone();
    let prior = first.span.clone();
    let Op::Call { args, .. } = first.op else {
        unreachable!()
    };
    let id = args[0].value;
    let definition = f.blocks[0]
        .instructions
        .iter()
        .find(|i| i.dest == id)
        .unwrap()
        .span
        .clone();
    let second = &mut f.blocks[0].instructions[calls[1]];
    let primary = second.span.clone();
    let Op::Call { args, .. } = &mut second.op else {
        unreachable!()
    };
    args[0].value = id;
    (checked, program, id, definition, prior, primary)
}
#[test]
fn duplicate_owned_consumption_points_to_definition_and_valid_same_block_use() {
    let (checked, mut program, id, definition, prior, primary) = double_consume();
    let ds = flow(&checked, &mut program);
    let d = ds
        .iter()
        .find(|d| d.message == format!("IR temporary %{id} is consumed more than once"))
        .unwrap();
    assert_eq!(d.span, primary);
    assert_eq!(
        d.related.iter().map(|l| l.span.clone()).collect::<Vec<_>>(),
        [definition, prior]
    );
    assert_eq!(
        d.related[1].message,
        "an earlier consuming use in this block is here"
    );
    labels(DOUBLE, &ds);
}
#[test]
fn borrowed_noncopy_consume_attempts_never_become_completed_consumption_witnesses() {
    let (checked, mut program, id, definition, _, _) = double_consume();
    let f = program.functions.get_mut("f").unwrap();
    let read = f.blocks[0]
        .instructions
        .iter_mut()
        .find(|i| i.dest == id)
        .unwrap();
    let Op::Read { pass, .. } = &mut read.op else {
        unreachable!()
    };
    *pass = Pass::In;
    let ds = flow(&checked, &mut program);
    assert!(
        ds.iter()
            .any(|d| d.message
                == "a borrowed non-copy temporary cannot be moved into an owned value")
    );
    let d = ds
        .iter()
        .find(|d| d.message == format!("IR temporary %{id} is consumed more than once"))
        .unwrap();
    assert!(d.related.iter().any(|l| l.span == definition));
    assert!(
        ds.iter()
            .flat_map(|d| &d.related)
            .all(|l| !l.message.contains("earlier consuming"))
    );
    labels(DOUBLE, &ds);
}
#[test]
fn mutable_loan_overlap_and_abort_use_the_real_read_and_parameter_span() {
    let source = "copy struct Pair{x:u64} effect Stop{fn act();} fn touch(inout x:Pair){} fn f(inout p:Pair){touch(inout p);p.x=1;} fn g() eff[Stop]{let p=Pair{x:0};touch(inout p);Stop::act();}";
    let (checked, mut program) = lower(source);
    let parameter = program.functions["f"].params[0].span.clone();
    let mut reads = Vec::new();
    for name in ["f", "g"] {
        let f = program.functions.get_mut(name).unwrap();
        let call = f.blocks[0]
            .instructions
            .iter_mut()
            .find(|i| matches!(&i.op,Op::Call {callee,..} if callee=="touch"))
            .unwrap();
        let Op::Call { args, .. } = &mut call.op else {
            unreachable!()
        };
        let id = args[0].value;
        args.clear();
        reads.push(
            f.blocks[0]
                .instructions
                .iter()
                .find(|i| i.dest == id)
                .unwrap()
                .span
                .clone(),
        );
    }
    let read = reads[0].clone();
    let ds = flow(&checked, &mut program);
    let overlap = ds
        .iter()
        .find(|d| d.message.contains("overlaps active loan"))
        .unwrap();
    assert_eq!(&source[overlap.span.clone()], "p.x=1");
    assert!(
        overlap
            .related
            .iter()
            .any(|l| l.span == read && l.message == "this IR read requests the conflicting access")
    );
    assert!(
        overlap
            .related
            .iter()
            .any(|l| l.span == parameter && l.message == "source span for this IR parameter")
    );
    let abort = ds
        .iter()
        .find(|d| d.message == "abortable effect crosses a mutable temporary loan")
        .unwrap();
    assert_eq!(&source[abort.span.clone()], "Stop::act()");
    assert!(abort.related.iter().any(|l| l.span == reads[1]));
    labels(source, &ds);
}
fn erase_read(i: &mut Instruction) {
    i.op = Op::Unit;
    i.ty = Type::unit();
    i.fallible = false;
}
#[test]
fn parameter_and_shadowed_binding_causes_are_declarations_not_guessed_last_moves() {
    let source = "linear struct Tx{} fn f(sink tx:Tx){let Tx{}=tx;} fn shadow(){let x=Tx{};let Tx{}=x;let x=Tx{};let Tx{}=x;}";
    let (checked, mut program) = lower(source);
    let f = program.functions.get_mut("f").unwrap();
    let parameter = f.params[0].span.clone();
    erase_read(
        f.blocks[0]
            .instructions
            .iter_mut()
            .find(|i| matches!(&i.op,Op::Read {root,..} if root=="tx"))
            .unwrap(),
    );
    let shadow = program.functions.get_mut("shadow").unwrap();
    let (root, binding) = shadow.blocks[0]
        .instructions
        .iter()
        .filter_map(|i| match &i.op {
            Op::Bind { pattern, .. } => match &pattern.kind {
                vesper::ast::PatternKind::Bind(name) => Some((name.clone(), pattern.span.clone())),
                _ => None,
            },
            _ => None,
        })
        .next_back()
        .unwrap();
    erase_read(
        shadow.blocks[0]
            .instructions
            .iter_mut()
            .find(|i| matches!(&i.op,Op::Read {root:r,..} if r==&root))
            .unwrap(),
    );
    let ds = flow(&checked, &mut program);
    let param = ds
        .iter()
        .find(|d| d.message == "linear place 'tx' remains live at IR return")
        .unwrap();
    assert!(param.related.iter().any(|l| l.span == parameter));
    let local = ds
        .iter()
        .find(|d| d.message == format!("linear place '{root}' remains live at IR return"))
        .unwrap();
    assert_eq!(
        local
            .related
            .iter()
            .map(|l| l.span.clone())
            .collect::<Vec<_>>(),
        [binding]
    );
    labels(source, &ds);
}
#[test]
fn joins_attach_only_static_declarations_without_cross_branch_consume_guesses() {
    let source =
        "linear struct Tx{} fn f(sink tx:Tx,in flag:bool){if flag{let Tx{}=tx;}else{let Tx{}=tx;}}";
    let (checked, mut program) = lower(source);
    let f = program.functions.get_mut("f").unwrap();
    let parameter = f.params[0].span.clone();
    let no = match f.blocks[0].terminator {
        Terminator::Branch { no, .. } => no,
        _ => unreachable!(),
    };
    erase_read(
        f.blocks[no]
            .instructions
            .iter_mut()
            .find(|i| matches!(&i.op,Op::Read {root,..} if root=="tx"))
            .unwrap(),
    );
    let ds = flow(&checked, &mut program);
    let d = ds
        .iter()
        .find(|d| d.message == "linear place 'tx' has inconsistent ownership at a CFG join")
        .unwrap();
    assert_eq!(
        d.related.iter().map(|l| l.span.clone()).collect::<Vec<_>>(),
        [parameter]
    );
    assert!(
        ds.iter()
            .flat_map(|d| &d.related)
            .all(|l| !l.message.contains("earlier consuming"))
    );
    labels(source, &ds);
}
#[test]
fn loop_revisits_converge_without_propagating_diagnostic_provenance_in_state() {
    let source = "linear struct Tx{} fn drop(sink tx:Tx){let Tx{}=tx;} fn f(sink tx:Tx){drop(tx);}";
    let (checked, mut program) = lower(source);
    let f = program.functions.get_mut("f").unwrap();
    f.blocks[0].terminator = Terminator::Jump {
        block: 0,
        args: vec![],
    };
    let ds = flow(&checked, &mut program);
    assert!(ds.iter().any(|d| d.message == "IR uses moved place 'tx'"));
    assert!(
        ds.iter()
            .all(|d| d.message != "IR ownership dataflow did not converge")
    );
    labels(source, &ds);
}
#[test]
fn undefined_internal_zero_and_duplicate_definitions_do_not_invent_causes() {
    let (checked, mut program) = lower("fn f(){} ");
    program.functions.get_mut("f").unwrap().blocks[0].terminator = Terminator::Return(u32::MAX);
    let ds = flow(&checked, &mut program);
    let d = ds
        .iter()
        .find(|d| d.message.starts_with("IR uses undefined value"))
        .unwrap();
    assert!(d.related.is_empty());
    assert!(d.help.is_none());
    for duplicate in [false, true] {
        let (checked, mut program, id, _, _, _) = double_consume();
        let f = program.functions.get_mut("f").unwrap();
        if duplicate {
            f.blocks.push(Block {
                entry_token: 0,
                incoming_tokens: vec![],
                outgoing_token: 0,
                params: vec![],
                instructions: vec![Instruction {
                    dest: id,
                    op: Op::Unit,
                    ty: Type::unit(),
                    span: 0..0,
                    token: None,
                    fallible: false,
                }],
                terminator: Terminator::Unreachable,
            });
        } else {
            f.blocks[0]
                .instructions
                .iter_mut()
                .find(|i| i.dest == id)
                .unwrap()
                .span = 0..0;
        }
        let ds = flow(&checked, &mut program);
        let d = ds
            .iter()
            .find(|d| d.message == format!("IR temporary %{id} is consumed more than once"))
            .unwrap();
        assert!(d.related.is_empty());
        assert!(d.help.is_none());
    }
}
#[test]
fn provenance_budget_failure_discards_all_optional_origins_and_keeps_primary_error() {
    let (checked, mut program, id, _, _, primary) = double_consume();
    program.functions.get_mut("f").unwrap().params.push(Param {
        name: "x".repeat(1025),
        pass: Pass::In,
        projections: vec![],
        ty: Type::named("u64"),
        span: 0..0,
    });
    let ds = flow(&checked, &mut program);
    let d = ds
        .iter()
        .find(|d| d.message == format!("IR temporary %{id} is consumed more than once"))
        .unwrap();
    assert_eq!(d.span, primary);
    assert!(d.related.is_empty());
    assert!(d.help.is_none());
}

#[test]
fn nested_closure_and_handler_generated_ir_keep_actual_temporary_origins() {
    for source in [
        "// 🦀\nlinear struct Ticket {} fn consume(sink t: Ticket, value: i64) { let Ticket {} = t; } fn bad() { let f = || eff [Fail[i64]] consume(Ticket {}, Fail::raise(0)); }",
        "linear struct Ticket {} effect Stop { fn stop() -> Never; } linear effect Ask { fn ask() -> i64; } fn consume(sink t: Ticket, value: i64) -> i64 { let Ticket {} = t; value } fn main() -> i64 { handle Stop in { handle Ask in { Ask::ask() } with { return x => x, ask(k) => k(consume(Ticket {}, Stop::stop())), } } with { return x => x, stop(k) => 42, } }",
    ] {
        let parsed = parser::parse(source);
        assert!(parsed.diagnostics.is_empty());
        let checked = check::check(&parsed.module);
        assert!(
            checked.diagnostics.iter().any(|d| d.code == "E0410"
                && d.message
                    .starts_with("abortable effect may abandon linear temporary %")
                && d.related
                    .iter()
                    .any(|l| l.message == "this IR temporary is defined here"
                        && &source[l.span.clone()] == "Ticket {}")),
            "{:?}",
            checked.diagnostics
        );
        labels(source, &checked.diagnostics);
    }
}

#[test]
fn invalid_shared_read_recovery_labels_the_request_not_a_successful_loan() {
    let source =
        "copy struct Pair{x:u64} fn observe(in p:Pair){} fn f(in p:Pair)->u64{observe(in p);p.x}";
    let (checked, mut program) = lower(source);
    let f = program.functions.get_mut("f").unwrap();
    let call = f.blocks[0]
        .instructions
        .iter_mut()
        .find(|i| matches!(&i.op,Op::Call {callee,..} if callee=="observe"))
        .unwrap();
    let Op::Call { args, .. } = &mut call.op else {
        unreachable!()
    };
    let id = args[0].value;
    args.clear();
    let read = f.blocks[0]
        .instructions
        .iter_mut()
        .find(|i| i.dest == id)
        .unwrap();
    let request = read.span.clone();
    let Op::Read { pass, .. } = &mut read.op else {
        unreachable!()
    };
    *pass = Pass::Inout;
    let ds = flow(&checked, &mut program);
    assert!(
        ds.iter()
            .any(|d| d.message == "IR mutates a shared parameter")
    );
    let d = ds
        .iter()
        .find(|d| d.message.contains("overlaps active loan"))
        .unwrap();
    assert!(
        d.related
            .iter()
            .any(|l| l.span == request
                && l.message == "this IR read requests the conflicting access")
    );
    assert!(
        ds.iter()
            .flat_map(|d| &d.related)
            .all(|l| !l.message.contains("introduces the active loan"))
    );
    labels(source, &ds);
}

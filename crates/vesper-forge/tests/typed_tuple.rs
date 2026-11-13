use std::{collections::BTreeSet, ops::Range};
use vesper_forge::{typed_quote::*, *};
use vesper_syntax::macro_source::{self, FunctionId, MacroSource};

const PACKAGE: PackageId = PackageId(1);
const MACRO: DefId = DefId {
    package: PACKAGE,
    item: 0,
};
const HELPER: DefId = DefId {
    package: PACKAGE,
    item: 1,
};

fn host() -> HygieneHost {
    let mut registry = DefinitionRegistry::new();
    registry
        .add_package(PackageInfo {
            id: PACKAGE,
            name: "app".into(),
            dependencies: BTreeSet::new(),
        })
        .unwrap();
    for (id, path, kind) in [
        (MACRO, "keep", DefKind::Macro),
        (HELPER, "helper", DefKind::Function),
    ] {
        registry
            .add_definition(DefInfo {
                id,
                path: path.into(),
                kind,
                visibility: Visibility::Public,
            })
            .unwrap();
    }
    let mut host = HygieneHost::new(SessionId(41), registry);
    host.declare_global(PACKAGE, "helper", HELPER).unwrap();
    host
}
fn budget() -> TypedBudget {
    TypedBudget::new(TypedLimits::default()).unwrap()
}
fn items(text: &str) -> Vec<ExprQuoteItem> {
    lex_fragment(text)
        .into_iter()
        .map(ExprQuoteItem::Generated)
        .collect()
}
fn input(host: &mut HygieneHost, text: &str, budget: &mut TypedBudget) -> ExprInput {
    let source = host.import_expr_source(PACKAGE, text, budget).unwrap();
    host.expr_input(&source, 0..text.len(), budget).unwrap()
}
fn identity(host: &mut HygieneHost, text: &str, budget: &mut TypedBudget) -> ExpandedExpr {
    let input = input(host, text, budget);
    let expansion = host
        .begin_expr_expansion(MACRO, InvocationSite::Source(PACKAGE), vec![input], budget)
        .unwrap();
    host.finish_expr(expansion, &[ExprQuoteItem::InputSlot(0)], budget)
        .unwrap()
}

struct Prepared {
    host: HygieneHost,
    syntax: MacroSource,
    source: ExprSource,
    budget: TypedBudget,
    function: FunctionId,
    replacements: Vec<(Range<usize>, ExpandedExpr)>,
}
impl Prepared {
    fn new(text: &str) -> Self {
        let syntax = macro_source::parse(text, macro_source::Limits::default()).unwrap();
        let function = syntax
            .functions()
            .iter()
            .find(|function| function.header.name == "target")
            .unwrap()
            .id;
        let mut host = host();
        let mut budget = budget();
        let source = host.import_expr_source(PACKAGE, text, &mut budget).unwrap();
        let mut replacements = Vec::new();
        for id in &syntax.function(function).unwrap().invocations {
            let invocation = syntax.invocation(*id).unwrap();
            let definition = syntax.definition(invocation.definition).unwrap();
            let names = definition
                .parameters
                .iter()
                .map(|parameter| parameter.name_span.clone())
                .collect::<Vec<_>>();
            let quote = host
                .expr_quotation(
                    &source,
                    MACRO,
                    definition.quote_span.clone(),
                    &names,
                    &mut budget,
                )
                .unwrap();
            let mut inputs = Vec::new();
            for argument in &invocation.arguments {
                inputs.push(
                    host.expr_input(&source, argument.clone(), &mut budget)
                        .unwrap(),
                );
            }
            let expansion = host
                .begin_expr_expansion(MACRO, InvocationSite::Source(PACKAGE), inputs, &mut budget)
                .unwrap();
            let output = host
                .finish_expr(expansion, quote.items(), &mut budget)
                .unwrap();
            replacements.push((invocation.span.clone(), output));
        }
        Self {
            host,
            syntax,
            source,
            budget,
            function,
            replacements,
        }
    }
    fn resolve(&mut self) -> Result<ResolvedBody, TypedError> {
        self.host.resolve_expr_function_body(
            &self.source,
            &self.syntax,
            self.function,
            std::mem::take(&mut self.replacements),
            &mut self.budget,
        )
    }
}

#[test]
fn unit_group_singleton_multiple_and_nested_input_shapes_remain_distinct() {
    for (text, arity) in [
        ("()", Some(0)),
        ("(42)", None),
        ("(42,)", Some(1)),
        ("(1,2)", Some(2)),
        ("(1,2,)", Some(2)),
        ("((),(1,),(2,3))", Some(3)),
    ] {
        let mut host = host();
        let mut budget = budget();
        let input = input(&mut host, text, &mut budget);
        let expansion = host
            .begin_expr_expansion(
                MACRO,
                InvocationSite::Source(PACKAGE),
                vec![input],
                &mut budget,
            )
            .unwrap();
        let output = host
            .finish_expr(expansion, &[ExprQuoteItem::InputSlot(0)], &mut budget)
            .unwrap();
        match arity {
            Some(arity) => assert!(
                matches!(output.shape(), ExprShape::Tuple { values, .. } if values.len() == arity)
            ),
            None => assert!(matches!(output.shape(), ExprShape::Literal { .. })),
        }
    }
    let mut host = host();
    assert!(matches!(
        identity(&mut host, "{}", &mut budget()).shape(),
        ExprShape::Block { .. }
    ));
}

#[test]
fn tuple_slot_is_one_call_argument_and_repeated_occurrences_keep_provenance() {
    let mut host = host();
    let mut budget = budget();
    let tuple = input(&mut host, "(1,2)", &mut budget);
    let expansion = host
        .begin_expr_expansion(
            MACRO,
            InvocationSite::Source(PACKAGE),
            vec![tuple],
            &mut budget,
        )
        .unwrap();
    let mut quote = items("helper(");
    quote.push(ExprQuoteItem::InputSlot(0));
    quote.extend(items(","));
    quote.push(ExprQuoteItem::InputSlot(0));
    quote.extend(items(")"));
    let output = host.finish_expr(expansion, &quote, &mut budget).unwrap();
    let ExprShape::Call { args, .. } = output.shape() else {
        panic!("call expected")
    };
    assert_eq!(args.len(), 2);
    for argument in args {
        assert!(matches!(argument, ExprShape::Tuple { values, .. } if values.len() == 2));
    }
    let repeated = output
        .provenance()
        .iter()
        .enumerate()
        .filter_map(|(index, provenance)| {
            matches!(provenance, TokenProvenance::Source { span } if span == &(1..2))
                .then_some(index)
        })
        .collect::<Vec<_>>();
    assert_eq!(repeated.len(), 2);
    assert_ne!(repeated[0], repeated[1]);
    assert_eq!(host.text(output.tokens()[repeated[0]].text()), "1");
    assert_eq!(host.text(output.tokens()[repeated[1]].text()), "1");
}

#[test]
fn nested_generated_calls_and_caller_slot_calls_keep_separate_actual_origins() {
    let text = "macro keep($x:expr)->expr{quote{(helper(),($x,helper()))}} fn target()->(i64,(i64,i64)){keep!(helper())} fn helper()->i64{42}";
    let mut prepared = Prepared::new(text);
    let resolved = prepared.resolve().unwrap();
    let calls = resolved
        .fragment()
        .operations
        .iter()
        .filter(|operation| matches!(operation.kind, OperationKind::Use { def } if def == HELPER))
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 3);
    assert!(
        calls[0]
            .origins
            .iter()
            .all(|origin| matches!(origin, Origin::Expansion(_)))
    );
    assert_eq!(
        calls[1].origins,
        [Origin::Source { package: PACKAGE }].into_iter().collect()
    );
    assert!(
        calls[2]
            .origins
            .iter()
            .all(|origin| matches!(origin, Origin::Expansion(_)))
    );
    assert!(calls.windows(2).all(|pair| pair[0].token < pair[1].token));
    assert_eq!(
        resolved
            .fragment()
            .references
            .iter()
            .filter(|reference| reference.pre_resolved)
            .count(),
        2
    );
}

#[test]
fn tuple_element_blocks_preserve_parameter_and_generated_binding_isolation() {
    let text = "macro keep($x:expr)->expr{quote{({let value=0;$x},{let value=1;value})}} fn target(in value:i64)->(i64,i64){keep!(value)}";
    let mut prepared = Prepared::new(text);
    let resolved = prepared.resolve().unwrap();
    let parameter = resolved.parameters()[0].binder();
    let fragment = resolved.fragment();
    assert_eq!(fragment.binders.len(), 3);
    assert_eq!(fragment.references.len(), 2);
    assert_eq!(
        fragment.references[0].resolution,
        Resolution::Local(parameter)
    );
    assert_eq!(
        fragment.references[1].resolution,
        Resolution::Local(fragment.binders[2].id)
    );
    let mut plain =
        Prepared::new("fn target(in value:i64)->(i64,i64){({let value=0;value},value)}");
    let resolved = plain.resolve().unwrap();
    assert_eq!(
        resolved.fragment().references[0].resolution,
        Resolution::Local(resolved.fragment().binders[1].id)
    );
    assert_eq!(
        resolved.fragment().references[1].resolution,
        Resolution::Local(resolved.parameters()[0].binder())
    );
}

#[test]
fn tuple_global_pre_resolution_does_not_reuse_sibling_block_locals() {
    let text = "macro keep()->expr{quote{({let helper=0;helper},helper())}} fn target(in helper:i64)->(i64,i64){keep!()} fn helper()->i64{42}";
    let mut prepared = Prepared::new(text);
    let resolved = prepared.resolve().unwrap();
    let fragment = resolved.fragment();
    assert_eq!(
        fragment.references[0].resolution,
        Resolution::Local(fragment.binders[1].id)
    );
    assert_eq!(
        fragment.references[1].resolution,
        Resolution::Global(HELPER)
    );
    assert!(fragment.references[1].pre_resolved);
    let bad =
        "macro keep()->expr{quote{({let hidden=0;hidden},hidden)}} fn target()->(i64,i64){keep!()}";
    let syntax = macro_source::parse(bad, macro_source::Limits::default()).unwrap();
    let mut host = host();
    let mut budget = budget();
    let source = host.import_expr_source(PACKAGE, bad, &mut budget).unwrap();
    assert!(matches!(
        host.expr_quotation(
            &source,
            MACRO,
            syntax.definitions()[0].quote_span.clone(),
            &[],
            &mut budget
        ),
        Err(TypedError::Category)
    ));
}

#[test]
fn unit_has_no_lexical_operations_and_tuple_calls_keep_source_order() {
    let mut unit = Prepared::new("macro keep()->expr{quote{()}} fn target(){keep!()}");
    let resolved = unit.resolve().unwrap();
    assert!(resolved.fragment().references.is_empty());
    assert!(resolved.fragment().operations.is_empty());
    assert_eq!(resolved.fragment().expansions.len(), 1);
    let mut ordered = Prepared::new("fn target()->(i64,i64,i64){(helper(1),helper(2),helper(3))}");
    let resolved = ordered.resolve().unwrap();
    assert_eq!(resolved.fragment().operations.len(), 3);
    assert!(
        resolved
            .fragment()
            .operations
            .windows(2)
            .all(|pair| pair[0].token < pair[1].token)
    );
}

#[test]
fn commas_in_strings_comments_and_large_arity_do_not_change_grammar_categories() {
    let mut host = host();
    let text = "(\"(,🦀)\", /* , ) */ (42,), ())";
    let tuple = identity(&mut host, text, &mut budget());
    let ExprShape::Tuple { values, .. } = tuple.shape() else {
        panic!("tuple expected")
    };
    assert_eq!(values.len(), 3);
    let ExprShape::Literal { token } = values[0] else {
        panic!("string expected")
    };
    assert_eq!(host.text(tuple.tokens()[token].text()), "\"(,🦀)\"");
    let text = format!("({})", vec!["1"; 40].join(","));
    let mut budget = TypedBudget::new(TypedLimits {
        max_slots: 1,
        ..Default::default()
    })
    .unwrap();
    let tuple = identity(&mut host, &text, &mut budget);
    assert!(matches!(tuple.shape(), ExprShape::Tuple { values, .. } if values.len() == 40));
}

#[test]
fn malformed_tuple_and_existing_call_trailing_comma_rules_stay_closed() {
    let mut host = host();
    for text in [
        "(,)",
        "(1,,)",
        "(1 2)",
        "(1,",
        "(1,2]",
        "helper(1,)",
        "helper((1,),)",
    ] {
        let mut budget = budget();
        let source = host.import_expr_source(PACKAGE, text, &mut budget).unwrap();
        assert!(
            host.expr_input(&source, 0..text.len(), &mut budget)
                .is_err(),
            "{text}"
        );
    }
}

#[test]
fn tuple_vectors_depth_and_repeated_slot_work_are_bounded_without_partial_output() {
    let mut host = host();
    let source = host
        .import_expr_source(PACKAGE, "(1,)", &mut budget())
        .unwrap();
    let mut tight = TypedBudget::new(TypedLimits {
        max_metadata_bytes: 1400,
        ..Default::default()
    })
    .unwrap();
    assert!(matches!(
        host.expr_input(&source, 0..4, &mut tight),
        Err(TypedError::Budget("metadata"))
    ));
    let nested = "((((1,),),),)";
    let source = host
        .import_expr_source(PACKAGE, nested, &mut budget())
        .unwrap();
    let mut tight = TypedBudget::new(TypedLimits {
        max_depth: 3,
        ..Default::default()
    })
    .unwrap();
    assert!(matches!(
        host.expr_input(&source, 0..nested.len(), &mut tight),
        Err(TypedError::Budget("depth"))
    ));
    let mut shared = budget();
    let tuple = input(&mut host, "(1,2)", &mut shared);
    let expansion = host
        .begin_expr_expansion(
            MACRO,
            InvocationSite::Source(PACKAGE),
            vec![tuple],
            &mut shared,
        )
        .unwrap();
    let id = expansion.id();
    let mut quote = items("(");
    quote.push(ExprQuoteItem::InputSlot(0));
    quote.extend(items(","));
    quote.push(ExprQuoteItem::InputSlot(0));
    quote.extend(items(")"));
    let mut tight = TypedBudget::new(TypedLimits {
        max_slot_occurrences: 1,
        ..Default::default()
    })
    .unwrap();
    assert!(matches!(
        host.finish_expr(expansion, &quote, &mut tight),
        Err(TypedError::Budget("slot occurrences"))
    ));
    assert!(matches!(
        host.finish_quotation(id, &Quotation::new().text("1")),
        Err(ForgeError::ExpansionClosed(_))
    ));
}

#[test]
fn tuple_inputs_keep_real_host_identity_and_flat_lexical_parser_compatibility() {
    let mut owner = host();
    let tuple = input(&mut owner, "(1,2)", &mut budget());
    assert!(matches!(
        host().begin_expr_expansion(
            MACRO,
            InvocationSite::Source(PACKAGE),
            vec![tuple],
            &mut budget()
        ),
        Err(TypedError::Foreign)
    ));
    let mut budget = budget();
    let tuple = input(&mut owner, "(1,)", &mut budget);
    let expansion = owner
        .begin_expr_expansion(
            MACRO,
            InvocationSite::Source(PACKAGE),
            vec![tuple],
            &mut budget,
        )
        .unwrap();
    let output = owner
        .finish_expr(expansion, &[ExprQuoteItem::InputSlot(0)], &mut budget)
        .unwrap();
    assert!(matches!(output.shape(), ExprShape::Tuple { values, .. } if values.len() == 1));
    assert!(owner.resolve(output.tokens().to_vec()).is_err());
}

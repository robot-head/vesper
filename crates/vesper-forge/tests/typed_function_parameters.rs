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
        let mut budget = TypedBudget::new(TypedLimits::default()).unwrap();
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
fn actual_header_tokens_are_suffix_bindings_and_plain_passes_are_preserved() {
    let text = "// 🦀\r\nfn target(in first:i64,inout second:i64,sink self:i64)->i64{first}";
    let mut prepared = Prepared::new(text);
    let resolved = prepared.resolve().unwrap();
    resolved.validate_host(&prepared.host).unwrap();
    assert_eq!(resolved.function_id(), Some(prepared.function));
    assert_eq!(resolved.original_source(), text);
    assert_eq!(resolved.parameters().len(), 3);
    let fragment = resolved.fragment();
    let ExprShape::Block { close, .. } = resolved.shape() else {
        panic!("whole body expected")
    };
    for (ordinal, parameter) in resolved.parameters().iter().enumerate() {
        assert_eq!(parameter.ordinal(), ordinal);
        assert_eq!(
            parameter.name_span(),
            &prepared
                .syntax
                .parameter_name_spans(prepared.function)
                .unwrap()[ordinal]
        );
        assert_eq!(parameter.token(), fragment.tokens.len() - 3 + ordinal);
        assert!(*close < parameter.token());
        let binder = fragment.binder(parameter.binder()).unwrap();
        assert_eq!(binder.token, parameter.token());
        assert_eq!(binder.name, &text[parameter.name_span().clone()]);
        assert!(matches!(binder.origin, Origin::Source { package } if package == PACKAGE));
        assert_eq!(
            resolved.provenance()[parameter.token()],
            TokenProvenance::Source {
                span: parameter.name_span().clone()
            }
        );
    }
    assert_eq!(
        fragment.references[0].resolution,
        Resolution::Local(resolved.parameters()[0].binder())
    );
    assert_eq!(
        prepared
            .syntax
            .function(prepared.function)
            .unwrap()
            .header
            .params
            .iter()
            .map(|p| p.pass)
            .collect::<Vec<_>>(),
        vec![
            vesper_syntax::ast::Pass::In,
            vesper_syntax::ast::Pass::Inout,
            vesper_syntax::ast::Pass::Sink
        ]
    );
}

#[test]
fn caller_parameter_and_generated_binding_cannot_capture_each_other() {
    let text = "macro keep($x:expr)->expr{quote{{let value=0;$x;value}}} fn target(in value:i64)->i64{keep!(value)}";
    let mut prepared = Prepared::new(text);
    let resolved = prepared.resolve().unwrap();
    let fragment = resolved.fragment();
    let parameter = resolved.parameters()[0].binder();
    let generated = fragment
        .binders
        .iter()
        .find(|binder| matches!(binder.origin, Origin::Expansion(_)))
        .unwrap();
    assert_ne!(parameter, generated.id);
    let caller = fragment
        .references
        .iter()
        .find(|reference| matches!(reference.origin, Origin::Source { .. }))
        .unwrap();
    assert_eq!(caller.resolution, Resolution::Local(parameter));
    let template = fragment
        .references
        .iter()
        .find(|reference| matches!(reference.origin, Origin::Expansion(_)))
        .unwrap();
    assert_eq!(template.resolution, Resolution::Local(generated.id));
}

#[test]
fn definition_site_global_claim_survives_same_named_parameter() {
    let text = "macro keep()->expr{quote{helper()}} fn target(in helper:i64)->i64{keep!()} fn helper()->i64{42}";
    let mut prepared = Prepared::new(text);
    let resolved = prepared.resolve().unwrap();
    assert_eq!(resolved.parameters().len(), 1);
    assert_eq!(resolved.fragment().references.len(), 1);
    let reference = &resolved.fragment().references[0];
    assert!(reference.pre_resolved);
    assert_eq!(reference.resolution, Resolution::Global(HELPER));
    assert!(
        resolved
            .fragment()
            .operations
            .iter()
            .any(|operation| matches!(operation.kind, OperationKind::Use { def } if def == HELPER))
    );
}

#[test]
fn initializer_sees_parameter_and_following_reference_sees_new_local() {
    let text = "macro keep($x:expr)->expr{quote{$x}} fn target(in value:i64)->i64{let value=keep!(value);value}";
    let mut prepared = Prepared::new(text);
    let resolved = prepared.resolve().unwrap();
    let fragment = resolved.fragment();
    assert_eq!(fragment.binders.len(), 2);
    assert_eq!(fragment.references.len(), 2);
    assert_eq!(
        fragment.references[0].resolution,
        Resolution::Local(resolved.parameters()[0].binder())
    );
    assert_eq!(
        fragment.references[1].resolution,
        Resolution::Local(fragment.binders[1].id)
    );
}

#[test]
fn foreign_host_exact_source_and_function_identity_refuse_before_resolution() {
    let text = "fn target(in value:i64)->i64{value}";
    let mut prepared = Prepared::new(text);
    let mut foreign = host(); // Same public numeric session, different real owner.
    let foreign_source = foreign
        .import_expr_source(
            PACKAGE,
            text,
            &mut TypedBudget::new(TypedLimits::default()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        prepared.host.resolve_expr_function_body(
            &foreign_source,
            &prepared.syntax,
            prepared.function,
            vec![],
            &mut prepared.budget
        ),
        Err(TypedError::Foreign)
    ));
    let changed = macro_source::parse(
        "fn target(in other:i64)->i64{other}",
        macro_source::Limits::default(),
    )
    .unwrap();
    assert_eq!(changed.source().len(), text.len());
    assert!(matches!(
        prepared.host.resolve_expr_function_body(
            &prepared.source,
            &changed,
            changed.functions()[0].id,
            vec![],
            &mut prepared.budget
        ),
        Err(TypedError::SourceMismatch)
    ));
    assert!(matches!(
        prepared.host.resolve_expr_function_body(
            &prepared.source,
            &prepared.syntax,
            changed.functions()[0].id,
            vec![],
            &mut prepared.budget
        ),
        Err(TypedError::SourceMismatch)
    ));
    let mut copied = prepared.syntax.function(prepared.function).unwrap().clone();
    copied.header.params[0].name = "forged".into();
    assert_eq!(
        prepared.resolve().unwrap().fragment().binders[0].name,
        "value"
    );
}

#[test]
fn legacy_and_zero_parameter_function_results_keep_distinct_association() {
    let mut prepared = Prepared::new("fn target()->i64{42}");
    let body = prepared
        .syntax
        .function(prepared.function)
        .unwrap()
        .body_span
        .clone();
    let legacy = prepared
        .host
        .resolve_expr_body(&prepared.source, body, vec![], &mut prepared.budget)
        .unwrap();
    assert_eq!(legacy.function_id(), None);
    assert!(legacy.parameters().is_empty());
    let function = prepared.resolve().unwrap();
    assert_eq!(function.function_id(), Some(prepared.function));
    assert!(function.parameters().is_empty());
    assert_eq!(
        legacy.fragment().tokens.len(),
        function.fragment().tokens.len()
    );
    assert_eq!(legacy.shape(), function.shape());
}

#[test]
fn additional_parameter_scopes_are_refused_before_binder_minting() {
    let text = "fn target(in a:i64,in b:i64,in c:i64,in d:i64,in e:i64,in f:i64,in g:i64,in h:i64)->i64{a}";
    let mut prepared = Prepared::new(text);
    let parameters = prepared
        .syntax
        .function(prepared.function)
        .unwrap()
        .header
        .params
        .len();
    // Body is `{a}`: 3 tokens plus real appended parameter identifiers.
    let tokens = parameters + 3;
    let legacy_envelope = tokens * (TypedLimits::default().max_depth + 2) + tokens;
    prepared.budget = TypedBudget::new(TypedLimits {
        max_scope_entries: legacy_envelope,
        ..Default::default()
    })
    .unwrap();
    let before = prepared.host.fresh_scope().unwrap().index();
    assert!(matches!(
        prepared.resolve(),
        Err(TypedError::Budget("scope entries"))
    ));
    assert_eq!(prepared.host.fresh_scope().unwrap().index(), before + 1);
    for limits in [
        TypedLimits {
            max_work: 1,
            ..Default::default()
        },
        TypedLimits {
            max_metadata_bytes: 1,
            ..Default::default()
        },
        TypedLimits {
            max_slots: 1,
            ..Default::default()
        },
    ] {
        prepared.budget = TypedBudget::new(limits).unwrap();
        assert!(matches!(prepared.resolve(), Err(TypedError::Budget(_))));
    }
}

#[test]
fn direct_function_api_rejects_unsupported_headers_and_indirect_parameter_calls() {
    for text in [
        "fn target[T](in value:T)->T{value}",
        "copy struct Pair{x:i64,y:i64} fn target(in value.{x}:Pair)->i64{42}",
        "fn target(in callback:fn(in i64)->i64)->i64{callback(42)}",
    ] {
        let mut prepared = Prepared::new(text);
        assert!(
            matches!(prepared.resolve(), Err(TypedError::Unsupported)),
            "{text}"
        );
    }
    let mut duplicate = Prepared::new("fn target(in value:i64,in value:i64)->i64{value}");
    assert!(matches!(duplicate.resolve(), Err(TypedError::Structural)));
}

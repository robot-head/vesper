//! Actual parameter-aware source compilation, not caller-provided header approval.
use vesper::{
    ast::{Item, Pass},
    forge_frontend::{
        self as forge, CompileConfig, CompileLimits, ExpandedChecked, Stage,
        authority::{self, AuthorityLimits},
    },
};
use vesper_syntax::macro_source::{self, MacroSource};

fn compile(source: &str) -> ExpandedChecked {
    forge::expand_source(source, &CompileConfig::default(), CompileLimits::default())
        .unwrap_or_else(|failure| panic!("{failure:?}"))
}
fn parameter<'a>(
    artifact: &'a ExpandedChecked,
    function: &str,
    index: usize,
) -> &'a vesper::ast::Param {
    artifact
        .checked()
        .parsed()
        .module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(f) if f.name == function => f.params.get(index),
            _ => None,
        })
        .unwrap()
}
fn syntax(source: &str) -> MacroSource {
    macro_source::parse(source, macro_source::Limits::default()).unwrap()
}

#[test]
fn a_parameter_cannot_hijack_a_definition_site_global_after_serialization() {
    let source = "macro call()->expr{quote{helper()}} fn helper()->i64{42} fn target(in helper:i64)->i64{call!()} fn main()->i64{target(0)}";
    let artifact = compile(source);
    let renamed = parameter(&artifact, "target", 0);
    assert_ne!(renamed.name, "helper");
    assert!(artifact.expanded_source().contains("helper()"));
    assert_eq!(renamed.pass, Pass::In);
    let parsed = syntax(source);
    let function = parsed
        .functions()
        .iter()
        .find(|f| f.header.name == "target")
        .unwrap();
    let span = &parsed.parameter_name_spans(function.id).unwrap()[0];
    assert_eq!(&source[span.clone()], "helper");
    let anchor = artifact
        .source_map()
        .iter()
        .find(|entry| {
            entry.original.start == span.start
                && entry.original.end == span.end
                && artifact.expanded_source()[entry.expanded.start..entry.expanded.end]
                    == renamed.name
        })
        .unwrap();
    assert!(matches!(anchor.kind, forge::MapKind::Source));
    assert_eq!(artifact.checked().source(), artifact.expanded_source());
}

#[test]
fn parameter_input_survives_generated_shadowing_and_initializer_scopes() {
    for source in [
        "macro keep($x:expr)->expr{quote{{let n=0;$x}}} fn target(in n:i64)->i64{keep!(n)} fn main()->i64{target(42)}",
        "macro keep($x:expr)->expr{quote{$x}} fn target(in n:i64)->i64{let n=keep!(n);n} fn main()->i64{target(42)}",
        "macro local()->expr{quote{{let n=42;n}}} fn target(in n:i64)->i64{local!()} fn main()->i64{target(0)}",
    ] {
        let artifact = compile(source);
        assert_ne!(parameter(&artifact, "target", 0).name, "n");
        assert_eq!(artifact.audits().len(), 1);
    }
}

#[test]
fn plain_pass_modes_self_and_header_comments_retain_actual_semantics() {
    for (spelling, pass) in [
        ("in", Pass::In),
        ("inout", Pass::Inout),
        ("sink", Pass::Sink),
    ] {
        let source = format!(
            "macro keep($x:expr)->expr{{quote{{$x}}}} fn target({spelling} /*before*/ value /*after*/:i64)->i64{{keep!(value)}}"
        );
        let artifact = compile(&source);
        let actual = parameter(&artifact, "target", 0);
        assert_eq!(actual.pass, pass);
        assert_ne!(actual.name, "value");
        assert!(artifact.expanded_source().contains("/*before*/"));
        assert!(artifact.expanded_source().contains("/*after*/"));
    }
    let artifact =
        compile("macro keep($x:expr)->expr{quote{$x}} fn target(in self:i64)->i64{keep!(self)}");
    assert_ne!(parameter(&artifact, "target", 0).name, "self");
    let unused = compile("macro fixed()->expr{quote{42}} fn target(in unused:i64)->i64{fixed!()}");
    assert_ne!(parameter(&unused, "target", 0).name, "unused");
}

#[test]
fn linear_parameter_consumption_is_checked_and_duplication_or_leak_is_rejected() {
    let valid = "linear struct Ticket{} macro keep($x:expr)->expr{quote{$x}} fn transfer(sink ticket:Ticket)->Ticket{keep!(ticket)} fn finish(sink ticket:Ticket)->i64{let Ticket{}=ticket;42} fn main()->i64{finish(transfer(Ticket{}))}";
    let artifact = compile(valid);
    assert_eq!(parameter(&artifact, "transfer", 0).pass, Pass::Sink);
    for (source, code) in [
        (
            "linear struct Ticket{} macro twice($x:expr)->expr{quote{{finish($x);finish($x)}}} fn finish(sink ticket:Ticket)->i64{let Ticket{}=ticket;42} fn target(sink ticket:Ticket)->i64{twice!(ticket)}",
            "E0303",
        ),
        (
            "linear struct Ticket{} macro discard($x:expr)->expr{quote{42}} fn target(sink ticket:Ticket)->i64{discard!(ticket)}",
            "E0302",
        ),
    ] {
        let failure =
            forge::expand_source(source, &CompileConfig::default(), CompileLimits::default())
                .unwrap_err();
        assert_eq!(failure.stage(), Stage::Check, "{failure:?}");
        assert!(
            failure.diagnostics().iter().any(|d| d.code == code),
            "{failure:?}"
        );
        assert!(failure.expanded_source().is_some());
    }
}

#[test]
fn duplicate_original_names_are_not_repaired_and_unsupported_headers_stay_rejected() {
    for source in [
        "macro keep($x:expr)->expr{quote{$x}} fn target(in n:i64,in n:i64)->i64{keep!(n)}",
        "macro keep($x:expr)->expr{quote{$x}} fn target[T](in n:T)->T{keep!(n)}",
        "copy struct Pair{x:i64} macro keep($x:expr)->expr{quote{$x}} fn target(in n.{x}:Pair)->i64{keep!(42)}",
    ] {
        assert!(
            forge::expand_source(source, &CompileConfig::default(), CompileLimits::default())
                .is_err()
        );
    }
}

#[test]
fn same_header_helper_body_changes_rerun_actual_authority_with_parameter_bindings() {
    let source = "macro call($x:expr)->expr{quote{helper($x)}} fn helper(in x:i64)->i64 eff[Fail[ArithOverflow]]{wrapping_add(x,1)} fn target(in n:i64)->i64 eff[Fail[ArithOverflow]]{call!(n)}";
    assert_eq!(compile(source).audits().len(), 1);
    let changed = source.replace("wrapping_add(x,1)", "x+1");
    let refusal = forge::expand_source(
        &changed,
        &CompileConfig::default(),
        CompileLimits::default(),
    )
    .unwrap_err();
    assert_eq!(refusal.stage(), Stage::Authority, "{refusal:?}");
}

#[test]
fn public_authority_derivation_still_requires_original_parameter_names() {
    let source = "macro keep($x:expr)->expr{quote{$x}} fn target(in n:i64)->i64{keep!(n)}";
    let artifact = compile(source);
    let bindings =
        authority::bind_source(&syntax(source), "@local/source", AuthorityLimits::default())
            .unwrap();
    let refusal =
        authority::derive(artifact.checked(), &bindings, AuthorityLimits::default()).unwrap_err();
    assert!(matches!(
        refusal,
        authority::AuthorityFailure::Mapping("checked_function_header")
    ));
}

#[test]
fn parameter_header_and_body_growth_are_bounded_without_partial_artifacts() {
    let source = "macro keep($x:expr)->expr{quote{$x}} fn target(in n:i64)->i64{keep!(n)}";
    let artifact = compile(source);
    let exact = artifact.expanded_source().len();
    assert!(
        forge::expand_source(
            source,
            &CompileConfig::default(),
            CompileLimits {
                max_expanded_bytes: exact,
                ..CompileLimits::default()
            }
        )
        .is_ok()
    );
    for limits in [
        CompileLimits {
            max_source_bytes: source.len() - 1,
            ..CompileLimits::default()
        },
        CompileLimits {
            max_expanded_bytes: exact - 1,
            ..CompileLimits::default()
        },
        CompileLimits {
            max_map_entries: 1,
            ..CompileLimits::default()
        },
        CompileLimits {
            max_work: 1,
            ..CompileLimits::default()
        },
    ] {
        assert!(forge::expand_source(source, &CompileConfig::default(), limits).is_err());
    }
}

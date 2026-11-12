use vesper_syntax::{
    ast::{Item, Type},
    lexer::TokenKind as K,
    macro_source::{self, ErrorKind, Limits, Resource},
    parser,
};
const KEEP: &str = "macro keep($value: expr) -> expr { quote { $value } }\nfn main() -> u64 { let n = 42; keep!(n) }\n";
fn parse(source: &str) -> macro_source::MacroSource {
    macro_source::parse(source, Limits::default()).unwrap_or_else(|e| panic!("{e:?}: {source}"))
}
#[test]
fn actual_source_tokens_headers_definitions_and_invocations_are_exact() {
    let source = parse(KEEP);
    assert_eq!(source.source(), KEEP);
    assert_eq!(
        source.source_hash(),
        *blake3::hash(KEEP.as_bytes()).as_bytes()
    );
    let rebuilt = source
        .tokens()
        .iter()
        .filter(|t| t.kind != K::Eof)
        .map(|t| &KEEP[t.span.clone()])
        .collect::<String>();
    assert_eq!(rebuilt, KEEP);
    let d = &source.definitions()[0];
    assert_eq!(&KEEP[d.name_span.clone()], "keep");
    assert_eq!(&KEEP[d.quote_span.clone()], " $value ");
    assert_eq!(&KEEP[d.parameters[0].span.clone()], "$value: expr");
    let f = &source.functions()[0];
    assert_eq!(f.header.name, "main");
    assert_eq!(&KEEP[f.header.name_span.clone()], "main");
    assert_eq!(&KEEP[f.header.span.clone()], "fn main() -> u64 ");
    assert_eq!(&KEEP[f.body_span.clone()], "{ let n = 42; keep!(n) }");
    assert_eq!(f.header.result, Type::named("u64"));
    assert!(f.header.params.is_empty());
    assert!(source.support_items().items.is_empty());
    let call = source.invocation(f.invocations[0]).unwrap();
    assert_eq!(&KEEP[call.span.clone()], "keep!(n)");
    assert_eq!(&KEEP[call.arguments[0].clone()], "n");
    assert_eq!(source.definition(call.definition).unwrap(), d);
    assert_eq!(source.function(call.function).unwrap(), f);
}
#[test]
fn ordinary_support_items_are_actual_complete_ast_with_real_absolute_spans() {
    let input = "macro keep($x:expr)->expr{quote{$x}}\ncopy struct Pair{left:u64,right:u64}\neffect Log{fn event(in n:u64);}\nfn helper(in n:u64)->u64{n}\nfn main()->u64{keep!(helper(42))}";
    let s = parse(input);
    assert_eq!(s.functions().len(), 2);
    assert!(s.functions()[0].invocations.is_empty());
    assert_eq!(s.functions()[0].header.params.len(), 1);
    assert_eq!(s.support_items().items.len(), 3);
    let Item::Function(function) = &s.support_items().items[2] else {
        panic!("actual helper missing")
    };
    assert_eq!(&input[function.span.clone()], "fn helper(in n:u64)->u64{n}");
    assert_eq!(&input[function.body.as_ref().unwrap().span.clone()], "{n}");
    assert_eq!(function.params[0].name, "n");
    assert!(
        s.support_items()
            .items
            .iter()
            .all(|item| !matches!(item,Item::Function(f) if f.name=="main"))
    );
}
#[test]
fn explicit_nested_blocks_keep_trailing_semicolon_and_slot_atom_boundaries() {
    let input = "macro scope($x:expr)->expr{quote{{let n=1;$x;}}} fn main(){scope!({let n=42;n})}";
    let s = parse(input);
    assert_eq!(
        &input[s.definitions()[0].quote_span.clone()],
        "{let n=1;$x;}"
    );
    assert_eq!(
        &input[s.invocations()[0].arguments[0].clone()],
        "{let n=42;n}"
    );
    let input = "macro call($x:expr)->expr{quote{$x(2)}} fn main(){call!(helper)} fn helper(in n:u64)->u64{n}";
    assert_eq!(
        &input[parse(input).definitions()[0].quote_span.clone()],
        "$x(2)"
    ); // grouping/drift is enforced by the typed host, not faked here
}
#[test]
fn comments_strings_unicode_and_crlf_never_create_anchors() {
    let input = "// 🦀 macro forged($x:expr)->expr{quote{$x}}\r\nmacro keep($x:expr)->expr{/* quote { fake } */quote{ $x /* 🦀 */ }}\r\nfn text()->String{\"keep!(forged) macro fake\"}\r\nfn main()->u64{/* fake!(0) */keep!(/* 🦀 */42)}\r\n";
    let s = parse(input);
    assert_eq!(s.definitions().len(), 1);
    assert_eq!(s.invocations().len(), 1);
    assert_eq!(&input[s.invocations()[0].arguments[0].clone()], "42");
    assert_eq!(
        &input[s.definitions()[0].quote_span.clone()],
        " $x /* 🦀 */ "
    );
}
#[test]
fn unknown_arity_recursive_and_nested_invocations_fail_closed() {
    for (source, kind) in [
        ("fn main(){missing!(1)}", ErrorKind::UnknownMacro),
        (
            "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!()}",
            ErrorKind::Arity,
        ),
        (
            "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!(1,2)}",
            ErrorKind::Arity,
        ),
        (
            "macro keep($x:expr)->expr{quote{keep!($x)}} fn main(){keep!(1)}",
            ErrorKind::Unsupported,
        ),
        (
            "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!(keep!(1))}",
            ErrorKind::Unsupported,
        ),
    ] {
        assert_eq!(
            macro_source::parse(source, Limits::default())
                .unwrap_err()
                .kind,
            kind,
            "{source}"
        );
    }
}
#[test]
fn malformed_categories_headers_delimiters_and_contexts_are_explicit_errors() {
    for source in [
        "macro keep($x:pat)->expr{quote{$x}}",
        "macro keep($x:expr)->item{quote{$x}}",
        "macro keep($x:expr)->expr{quote{}}",
        "macro keep($x:expr)->expr{quote{$missing}}",
        "macro keep($x:expr)->expr{quote{let n=1;$x}}",
        "macro keep($x:expr)->expr{quote{{let $x=1;0}}}",
        "macro keep($x:expr)->expr{quote{$x,$x}}",
        "macro keep($x:expr)->expr{quote{$x}} fn main(in x:u64){keep!(x)}",
        "macro keep($x:expr)->expr{quote{$x}} fn main[T](){keep!(1)}",
        "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!(handle E in {0} with {})}",
        "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!(1) 2}",
        "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!([1])}",
        "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!((1])}",
        "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!(1)",
        "macro keep($x:expr)->expr{quote{$x}} fn main()->{keep!(1)}",
        "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!(1)} trailing",
        "macro keep($x:expr)->expr{quote{$x}} fn main(){keep!(1)} /*",
    ] {
        assert!(
            macro_source::parse(source, Limits::default()).is_err(),
            "{source}"
        );
    }
}
#[test]
fn duplicate_macro_and_parameter_names_fail_before_publication() {
    for source in [
        "macro x()->expr{quote{1}} macro x()->expr{quote{2}}",
        "macro x($a:expr,$a:expr)->expr{quote{$a}}",
    ] {
        assert_eq!(
            macro_source::parse(source, Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::DuplicateName
        );
    }
}
#[test]
fn source_ids_reject_other_actual_sources_even_with_identical_headers() {
    let a = parse(KEEP);
    let b = parse(&format!("// current revision\n{KEEP}"));
    assert_eq!(
        a.definition(b.definitions()[0].id).unwrap_err().kind,
        ErrorKind::ForeignId
    );
    assert_eq!(
        a.function(b.functions()[0].id).unwrap_err().kind,
        ErrorKind::ForeignId
    );
    assert_eq!(
        a.invocation(b.invocations()[0].id).unwrap_err().kind,
        ErrorKind::ForeignId
    );
    assert_eq!(a.definitions()[0].id.index(), 0);
    assert_eq!(a.functions()[0].id.source_hash(), a.source_hash());
}
#[test]
fn direct_caps_and_allocation_stage_reservations_are_checked() {
    let d = Limits::default();
    assert!(Limits::HARD.validate().is_ok());
    for limits in [
        Limits {
            max_source_bytes: usize::MAX,
            ..d
        },
        Limits { max_depth: 0, ..d },
        Limits { max_tokens: 0, ..d },
    ] {
        assert_eq!(
            macro_source::parse("", limits).unwrap_err().kind,
            ErrorKind::InvalidLimits
        );
    }
    for (limits, resource) in [
        (
            Limits {
                max_source_bytes: KEEP.len() - 1,
                ..d
            },
            Resource::Source,
        ),
        (Limits { max_tokens: 1, ..d }, Resource::Tokens),
        (Limits { max_macros: 0, ..d }, Resource::Macros),
        (
            Limits {
                max_functions: 0,
                ..d
            },
            Resource::Functions,
        ),
        (
            Limits {
                max_invocations: 0,
                ..d
            },
            Resource::Invocations,
        ),
        (
            Limits {
                max_parameters: 0,
                ..d
            },
            Resource::Parameters,
        ),
        (
            Limits {
                max_metadata_bytes: 1,
                ..d
            },
            Resource::Metadata,
        ),
        (Limits { max_work: 1, ..d }, Resource::Work),
        (Limits { max_depth: 1, ..d }, Resource::Depth),
    ] {
        assert_eq!(
            macro_source::parse(KEEP, limits).unwrap_err().kind,
            ErrorKind::Budget(resource),
            "{resource:?}"
        );
    }
    assert!(
        macro_source::parse(
            KEEP,
            Limits {
                max_source_bytes: KEEP.len(),
                ..d
            }
        )
        .is_ok()
    );
}
#[test]
fn ordinary_batch_and_recovery_remain_real_and_dollar_is_appended_only() {
    assert_eq!(K::Dollar as u16, K::Eof as u16 + 1);
    assert!(!parser::parse(KEEP).diagnostics.is_empty());
    for source in [
        "fn identity[T: mod linear](sink x:T)->T{x} fn main()->u64{42}",
        "effect Carry[T]{fn send(in x:T);} fn f() eff [slot:Carry[fn(in i64)->i64 eff [Fail[u8]]]]{}",
        "fn shadow[r:eff](in r:u64)->u64{r}",
    ] {
        let ordinary = parser::parse(source);
        assert!(
            ordinary.diagnostics.is_empty(),
            "{:?}",
            ordinary.diagnostics
        );
        assert_eq!(ordinary.syntax().to_string(), source);
        let recovery =
            parser::parse_recovery(source, vesper_syntax::recovery::Limits::default()).unwrap();
        assert_eq!(recovery.source(), source);
        assert!(recovery.diagnostics().is_empty());
        let macros = parse(source);
        assert_eq!(macros.support_items(), &ordinary.module);
    }
}
#[test]
fn forward_macros_and_multiple_independent_invocations_resolve_actual_ids() {
    let input = "fn main()->u64{let n=keep!(40);keep!(n)} macro keep($x:expr)->expr{quote{$x}}";
    let s = parse(input);
    assert_eq!(s.invocations().len(), 2);
    assert_eq!(s.functions()[0].invocations.len(), 2);
    assert_eq!(s.invocations()[0].definition, s.invocations()[1].definition);
    assert!(s.invocations()[0].span.end <= s.invocations()[1].span.start);
}

#[test]
fn every_actual_utf8_prefix_is_bounded_and_never_fabricates_original_ranges() {
    let input = "// 🦀\r\nmacro keep($x:expr)->expr{quote{{let n=\"} fake!(0)\";$x}}}\r\nfn helper(in n:u64)->u64{n}\r\nfn main()->u64{keep!(helper(42))}";
    for end in input
        .char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(input.len()))
    {
        let prefix = &input[..end];
        if let Ok(parsed) = macro_source::parse(prefix, Limits::default()) {
            assert_eq!(parsed.source(), prefix);
            for definition in parsed.definitions() {
                assert!(prefix.get(definition.span.clone()).is_some());
                assert!(prefix.get(definition.quote_span.clone()).is_some());
            }
            for function in parsed.functions() {
                assert!(prefix.get(function.span.clone()).is_some());
                assert!(prefix.get(function.body_span.clone()).is_some());
            }
        }
    }
    assert_eq!(
        macro_source::parse("copy fn main(){}", Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
}

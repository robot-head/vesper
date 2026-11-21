use vesper_syntax::{
    parser,
    sugar_expand::{expand_sugar, expand_sugar_mapped, expand_sugar_with_imports},
};

#[test]
fn lookup_and_expansion_preserve_caller_locations() {
    let src =
        "sugar echo:expr { rule (echo $x:expr $b:block) => { $x } } fn main()->i64{ echo 41 {} }";
    let out = expand_sugar_mapped(src).unwrap().unwrap();
    assert!(
        parser::parse(&out.source).diagnostics.is_empty(),
        "{}",
        out.source
    );
    let value = out.source.find("41").unwrap();
    let orig = src.find("41").unwrap();
    assert_eq!(out.original_span(value..value + 2), orig..orig + 2);
    assert_eq!(out.regions.len(), 1);
    let rule = src.find("rule").unwrap();
    assert_eq!(out.regions[0].rule.start, rule);
    let call = src.rfind("echo").unwrap();
    assert_eq!(out.regions[0].call, call..call + "echo 41 {}".len());
}

#[test]
fn nested_rules_and_aliases_expand_before_core_parsing() {
    let imported = "pub sugar inner:expr {rule (inner $x:expr $b:block) => { $x }}";
    let src = "sugar outer:expr {rule (outer $x:expr $b:block) => { renamed $x {} }} fn main()->i64{outer 12 {} }";
    let out = expand_sugar_with_imports(src, &[("renamed", imported)])
        .unwrap()
        .unwrap();
    assert!(
        parser::parse(&out.source).diagnostics.is_empty(),
        "{}",
        out.source
    );
    assert_eq!(out.regions.len(), 2);
    assert!(out.import_origins.contains(&Some(0)));
    assert!(out.source.contains("12"));
}

#[test]
fn unresolved_wrong_slot_and_recursive_rules_are_diagnostics() {
    assert_eq!(
        expand_sugar("fn main(){unknown {}}").unwrap_err().code,
        "E0142"
    );
    assert_eq!(
        expand_sugar("sugar foo:tag {rule(foo $a:expr)=>{$a}} fn main(){foo {}}")
            .unwrap_err()
            .code,
        "E0141"
    );
    let e =
        expand_sugar("sugar yes:expr {rule(yes $b:block)=>{yes {}}} fn f(){yes {} }").unwrap_err();
    assert!(!e.related.is_empty() || e.code == "E0143", "{e:?}");
    assert!(expand_sugar("fn main(){ 1 }").unwrap().is_none());
}

#[test]
fn tail_capture_stops_at_the_enclosing_block() {
    let src = "sugar discard:stmt {rule(discard $b:block $..rest:stmts)=>{{ 7 }}} fn main(){ {discard {} 999}; 42 }";
    let out = expand_sugar(src).unwrap().unwrap();
    assert!(!out.contains("999"), "{out}");
    assert!(out.contains("42"), "{out}");
    assert!(parser::parse(&out).diagnostics.is_empty(), "{out}");
}

#[test]
fn nested_expansions_only_exempt_copied_caller_operands() {
    let src = "sugar inner:expr {rule(inner $x:expr $b:block)=>{privileged($x)}} sugar outer:expr {rule(outer $x:expr $b:block)=>{inner $x {}}} fn run(){outer 71 {} }";
    let out = expand_sugar_mapped(src).unwrap().unwrap();
    let outer = out
        .regions
        .iter()
        .find(|r| src[r.rule.clone()].starts_with("rule(outer"))
        .unwrap();
    let op = out.source.find("privileged").unwrap();
    assert!(!outer.holes.iter().any(|r| r.contains(&op)), "{outer:?}");
    let input = out.source.find("71").unwrap();
    assert!(outer.holes.iter().any(|r| r.contains(&input)), "{outer:?}");
}

#[test]
fn template_helper_and_caller_hole_resolve_in_their_own_lexical_scopes() {
    let src = "fn helper(x:i64)->i64{x} sugar run:expr {rule(run $x:expr $b:block)=>{helper($x)}} fn main()->i64{let helper=33;run helper {} }";
    let out = expand_sugar_mapped(src).unwrap().unwrap();
    let f = out.source.split("fn main").nth(1).unwrap();
    assert!(
        f.contains(&format!("let {}caller_0=33", out.fresh_prefix)),
        "{f}"
    );
    assert!(
        f.contains(&format!("helper(({}caller_0))", out.fresh_prefix))
            || f.contains(&format!("helper({}caller_0)", out.fresh_prefix)),
        "{f}"
    );
    assert!(
        parser::parse(&out.source).diagnostics.is_empty(),
        "{}",
        out.source
    );
}

#[test]
fn imported_templates_resolve_nested_sugar_in_the_defining_module() {
    use vesper_syntax::sugar_expand::expand_sugar_with_scopes;
    let outer = "pub sugar outer:expr {rule(outer $x:expr $b:block)=>{hidden $x {}}}";
    let private = "sugar hidden:expr {rule(hidden $x:expr $b:block)=>{10 + $x}}";
    let src = "sugar hidden:expr {rule(hidden $x:expr $b:block)=>{33}} fn main(){outer 2 {} }";
    let bindings = vec![("hidden".to_owned(), 1)];
    let out = expand_sugar_with_scopes(
        src,
        &[("outer", outer), ("hidden", private)],
        &[&bindings, &bindings],
        &[0],
    )
    .unwrap()
    .unwrap();
    assert!(out.source.contains("10 +"), "{}", out.source);
    assert!(!out.source.contains("33"), "{}", out.source);
}

#[test]
fn declaration_ceiling_cannot_introduce_an_unbound_row_variable() {
    let e = expand_sugar("sugar unused:stmt emits eff [e] {rule(unused {})=>{1}}").unwrap_err();
    assert!(e.message.contains("closed effect row"), "{e:?}");
}

#[test]
fn a_braced_scope_expands_its_nested_lexical_sugar_before_exit_lowering() {
    let src = "sugar holding:expr {rule(holding $body:scope)=>{{let result=$body;result.resume()}}} sugar inc:expr {rule(inc $x:expr {})=>{wrapping_add($x,1)}} fn main()->i64{ holding { inc 41 {} } }";
    let out = expand_sugar_mapped(src).unwrap().unwrap();
    assert!(
        parser::parse(&out.source).diagnostics.is_empty(),
        "{}",
        out.source
    );
    assert!(out.source.contains("wrapping_add"), "{}", out.source);
    assert_eq!(out.regions.len(), 2);
}

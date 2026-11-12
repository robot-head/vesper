use vesper::{check, interface, parser};
use vesper_semver::{self as api, Classification, Declaration, Mode, Type};

fn export(source: &str) -> api::Interface {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    interface::export(&checked.world, "@example/library").unwrap()
}
fn function(interface: &api::Interface, name: &str) -> api::Function {
    match &interface.exports[&format!("@example/library::fn::{name}")] {
        Declaration::Function { signature } => signature.clone(),
        d => panic!("expected function: {d:?}"),
    }
}
fn assert_unknown(interface: &api::Interface) {
    let report = api::compare(interface, interface);
    assert_eq!(report.classification, Classification::Unknown, "{report:?}");
    assert!(!report.is_compatible());
}

#[test]
fn bodies_locations_and_parameter_names_do_not_enter_contracts() {
    let before = export("fn inc(x: i64) -> i64 { let temp = x + 1; temp }");
    let after =
        export("// shifted source\nfn inc(input: i64) -> i64 { let other = input + 2; other }");
    assert_eq!(before, after);
    assert!(api::compare(&before, &after).is_compatible());
    let json = serde_json::to_string(&before).unwrap();
    assert!(!json.contains("$cap"));
    assert!(!json.contains("span"));
    assert_ne!(before, export("fn inc(x: u32) -> u32 { x + 1 }"));
}

#[test]
fn type_and_row_binders_are_positional() {
    let before = export("fn id[T](sink x: T) -> T { x } fn run[r: eff]() eff [r] {} ");
    let after = export("fn id[A](sink renamed: A) -> A { renamed } fn run[s: eff]() eff [s] {} ");
    assert_eq!(before, after);
    assert!(api::compare(&before, &after).is_compatible());
    assert_eq!(function(&before, "id").result, Type::Parameter { index: 0 });
    assert_eq!(function(&before, "id").result_mode, Mode::Linear);
}

#[test]
fn source_effects_are_stable_sorted_and_include_operation_contracts() {
    let before = export(
        "linear effect Ask[T] { fn ask() -> T; } effect Log { fn log(x: i64); } fn get() -> i64 eff [Ask[i64], Log] { Log::log(1); Ask::ask() }",
    );
    let after = export(
        "effect Log { fn log(value: i64); } linear effect Ask[A] { fn ask() -> A; } fn get() -> i64 eff [Log, Ask[i64]] { Ask::ask() }",
    );
    assert_eq!(before, after);
    assert!(api::compare(&before, &after).is_compatible());
    let Declaration::Effect {
        sealed, operations, ..
    } = &before.exports["@example/library::effect::Ask"]
    else {
        panic!()
    };
    assert!(!sealed);
    assert_eq!(operations["ask"].result, Type::Parameter { index: 0 });
    let changed = export(
        "linear effect Ask[T] { fn ask(x: i64) -> T; } effect Log { fn log(x: i64); } fn get() -> i64 eff [Ask[i64], Log] { Ask::ask(0) }",
    );
    assert!(!api::compare(&before, &changed).is_compatible());
}

#[test]
fn projections_and_passing_modes_are_preserved() {
    let interface = export(
        "copy struct Pair { left: i64, right: i64 } fn update(inout p.{left}: Pair) { p.left += 1; }",
    );
    let p = &function(&interface, "update").parameters[0];
    assert_eq!(p.pass, api::Pass::Inout);
    assert_eq!(
        p.projection,
        api::Projection::Paths {
            paths: [vec!["left".into()]].into()
        }
    );
    let changed = export(
        "copy struct Pair { left: i64, right: i64 } fn update(inout p.{right}: Pair) { p.right += 1; }",
    );
    assert!(!api::compare(&interface, &changed).is_compatible());
}

#[test]
fn nominal_modes_keep_fixed_payloads_and_crossing_changes() {
    let before = export(
        "copy struct Item {} fn make() -> Item { Item {} } fn option() -> Option[u8] { Option::Some(1) }",
    );
    let after = export(
        "linear struct Item {} fn make() -> Item { Item {} } fn option() -> Option[u8] { Option::Some(1) }",
    );
    assert_eq!(function(&before, "make").result_mode, Mode::Copy);
    assert_eq!(function(&after, "make").result_mode, Mode::Linear);
    assert_eq!(function(&before, "option").result_mode, Mode::Copy);
    assert!(!api::compare(&before, &after).is_compatible());
    let fixed =
        export("copy struct Text { value: String } fn make() -> Text { Text { value: \"hi\" } }");
    assert_eq!(function(&fixed, "make").result_mode, Mode::Affine);
    assert!(api::compare(&fixed, &fixed).is_compatible());
}

#[test]
fn methods_and_builtin_definitions_have_qualified_identities() {
    let interface = export(
        "copy struct Box[T: mod copy] { value: T } impl[T: mod copy] Box[T] { fn get(in self) -> T { self.value } } fn ratio(x: i64) -> i64 eff [Fail[RuntimeError]] { 4 / x }",
    );
    assert!(
        interface
            .exports
            .contains_key("@example/library::fn::Box::get")
    );
    assert!(interface.exports.contains_key("std/core::type::Option"));
    assert!(interface.exports.contains_key("std/core::effect::Fail"));
    assert!(api::compare(&interface, &interface).is_compatible());
}

#[test]
fn named_enum_payloads_fail_closed() {
    let interface = export("copy enum Event { Data { value: i64 }, Empty }");
    assert_unknown(&interface);
    let tuple = export("copy enum Event { Data(i64), Empty }");
    assert!(api::compare(&tuple, &tuple).is_compatible());
}

#[test]
fn named_and_latent_capabilities_fail_closed_even_after_source_shifts() {
    for source in [
        "linear effect Ask { fn ask() -> i64; } fn get() -> i64 eff [local: Ask] { local.ask() }",
        "linear effect Ask { fn ask() -> i64; } fn make() -> fn() -> i64 eff [Ask] { || eff [Ask] Ask::ask() }",
        "fn apply[T](sink f: fn(sink T) -> T, sink x: T) -> T { f(sink x) }",
    ] {
        let before = export(source);
        let after = export(&format!("// different positions\n{source}"));
        assert_eq!(before, after);
        assert_unknown(&before);
    }
}

#[test]
fn ordinary_closed_pure_arrows_are_supported() {
    let interface = export("fn apply(sink f: fn(i64) -> i64, x: i64) -> i64 { f(x) }");
    assert!(api::compare(&interface, &interface).is_compatible());
}

#[test]
fn malformed_package_names_are_rejected() {
    let world = check::check(&parser::parse("fn main() {}").module).world;
    for name in [
        "",
        "library",
        "example/library",
        "@Example/library",
        "@org/../lib",
        "@org/lib::evil",
        "@org/lib/extra",
    ] {
        assert!(interface::export(&world, name).is_err(), "{name}");
    }
    assert!(interface::export(&world, "std/test").is_ok());
}

#[test]
fn captured_arrow_generic_cannot_resolve_to_a_same_named_nominal() {
    let interface =
        export("copy struct T {} fn apply[T: mod copy](sink f: fn(T) -> T, x: T) -> T { f(x) }");
    assert_unknown(&interface);
}

#[test]
fn phantom_argument_modes_are_conservative_without_invalid_metadata() {
    let interface = export(
        "copy struct Phantom[T] {} linear struct Ticket {} fn make(sink x: Phantom[Ticket]) -> Phantom[Ticket] { x }",
    );
    assert_eq!(function(&interface, "make").result_mode, Mode::Linear);
    assert!(api::compare(&interface, &interface).is_compatible());
}

#[test]
fn nominal_row_arguments_fail_closed() {
    let interface =
        export("copy struct Container[r: eff] {} fn inspect[r: eff](x: Container[r]) {}");
    assert_unknown(&interface);
}

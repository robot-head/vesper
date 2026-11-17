//! Export ABI v2 (`vesper.native.export-abi/2`, #898): sequences, slices,
//! handles, boxes, generic instances and `.vsys` imports. The pilot's
//! `kit` library (crates/vesper-port-pilot) runs every form end to end;
//! these tests pin the interface, the v1 compatibility and every refusal.
use vesper::native::export::{
    self, AbiType, BindingOptions, ExportError, LibraryOptions, compile_source,
    compile_source_with, directives::parse_instance, rust_binding,
};

const PILOT: &str = include_str!("../crates/vesper-port-pilot/vesper/pilot.ves");
const KIT: &str = include_str!("../crates/vesper-port-pilot/vesper/kit.ves");

fn options(library: &'static str, target: Option<&'static str>) -> LibraryOptions<'static> {
    LibraryOptions {
        library,
        target,
        optimize: true,
    }
}

fn refusal(source: &str) -> String {
    match compile_source("t.ves", source, &options("t", None)) {
        Ok(_) => panic!("exported:\n{source}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn v1_libraries_keep_the_v1_interface_and_binding() {
    let pilot = compile_source("pilot.ves", PILOT, &options("pilot", None)).unwrap();
    assert_eq!(pilot.interface.abi, export::EXPORT_ABI);
    assert!(!pilot.interface.is_v2());
    let json = export::interface_json(&pilot.interface).unwrap();
    for v2_key in ["runtime", "\"pass\"", "handle", "foreign", "instance_of"] {
        assert!(!json.contains(v2_key), "v1 interface has {v2_key}:\n{json}");
    }
    // The committed v1 binding is still exactly what the compiler writes.
    let binding = rust_binding(
        &pilot.interface,
        &BindingOptions {
            types_path: Some("crate".into()),
        },
    );
    let committed = include_str!("../crates/vesper-port-pilot/port/binding.rs");
    assert_eq!(binding, committed.replace("\r\n", "\n"));
    // A v1 object carries no v2 runtime entry points.
    assert!(!pilot.object.windows(15).any(|w| w == b"vesper_pilot_se"));
}

#[test]
fn kit_interface_lists_every_form() {
    let kit = compile_source("kit.ves", KIT, &options("kit", None)).unwrap();
    let i = &kit.interface;
    assert_eq!(i.abi, export::EXPORT_ABI_V2);
    let runtime = i.runtime.as_ref().unwrap();
    assert_eq!(runtime.seq_new, "vesper_kit_seq_new");
    assert_eq!(runtime.seq_free, "vesper_kit_seq_free");
    assert_eq!(runtime.live_allocations, "vesper_kit_live_allocations");
    let f = |name: &str| i.functions.iter().find(|f| f.name == name).unwrap();
    assert!(matches!(&f("squares").result, AbiType::Seq { .. }));
    assert!(matches!(&f("sum").params[0].ty, AbiType::Slice { .. }));
    assert_eq!(f("scale").params[0].pass(), "inout");
    assert_eq!(f("ticket_close").params[0].pass(), "sink");
    // Handles: drop symbols, except for the linear Ticket.
    assert!(i.types["Counter"].handle && i.types["Counter"].owns_sequence);
    assert_eq!(
        i.types["Counter"].drop_symbol.as_deref(),
        Some("vesper_kit_Counter_drop")
    );
    assert!(i.types["Ticket"].linear && i.types["Ticket"].drop_symbol.is_none());
    assert!(i.types["Tree"].handle);
    // Instances keep stable symbols and name their generic.
    let largest = f("largest_u64");
    assert_eq!(largest.symbol, "vesper_kit_largest_u64");
    assert_eq!(largest.instance_of.as_deref(), Some("largest[u64]"));
    assert!(i.functions.iter().all(|f| f.name != "largest"));
    // The .vsys import.
    assert_eq!(f("stamp").foreign, ["Clock"]);
    assert_eq!(i.foreign[0].name, "Clock");
    assert_eq!(i.foreign[0].symbols, ["pilot_clock_ticks"]);
    assert_eq!(i.foreign[0].rust_path.as_deref(), Some("crate::kit::shim"));
    // The interface round-trips.
    let json = export::interface_json(i).unwrap();
    let back: export::LibraryInterface = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, i);
}

#[test]
fn kit_objects_build_for_every_hosted_target() {
    // x86_64-apple-darwin has no .vsys target (vesper-ffi), so the kit,
    // which imports one, is refused there by name.
    let refused = compile_source("kit.ves", KIT, &options("kit", Some("x86_64-apple-darwin")));
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains(".vsys libraries have no target for x86_64-apple-darwin")
    );
    for (target, symbol) in [
        ("x86_64-unknown-linux-gnu", "vesper_kit_Tree_drop"),
        ("aarch64-unknown-linux-gnu", "vesper_kit_Tree_drop"),
        ("aarch64-apple-darwin", "_vesper_kit_Tree_drop"),
        ("x86_64-pc-windows-msvc", "vesper_kit_Tree_drop"),
    ] {
        let a = compile_source("kit.ves", KIT, &options("kit", Some(target)))
            .unwrap_or_else(|e| panic!("{target}: {e}"));
        let b = compile_source("kit.ves", KIT, &options("kit", Some(target))).unwrap();
        assert_eq!(a.object, b.object, "{target}: objects are deterministic");
        assert!(
            a.object
                .windows(symbol.len())
                .any(|w| w == symbol.as_bytes()),
            "{target}: {symbol} missing"
        );
    }
}

#[test]
fn instances_from_flags_get_stable_names() {
    let source = "pub fn first[T: mod copy](in xs: Slice[T], none: T) -> T {\n    match xs.try_get(0) {\n        Option::Some(v) => v,\n        Option::None => none,\n    }\n}\n";
    let instances = [
        parse_instance("first[u64]").unwrap(),
        parse_instance("first_byte = first[u8]").unwrap(),
    ];
    let lib = compile_source_with("t.ves", source, &options("t", None), &instances).unwrap();
    let names: Vec<_> = lib.interface.functions.iter().map(|f| &f.symbol).collect();
    assert_eq!(names, ["vesper_t_first__u64", "vesper_t_first_byte"]);
    // Recompiling gives the same names and the same object.
    let again = compile_source_with("t.ves", source, &options("t", None), &instances).unwrap();
    assert_eq!(lib.object, again.object);
}

/// Every form v2 still refuses, with the function it refuses.
#[test]
fn still_refused_forms_name_the_function() {
    let cases = [
        (
            "pub fn id[T](sink x: T) -> T { x }",
            "cannot export generic function 'id'",
        ),
        (
            "pub fn f(sink xs: ChunkSeq[u64]) -> u64 { 0 }",
            "cannot export 'f': type ChunkSeq[u64] is a sequence taken by value",
        ),
        (
            "struct S { xs: ChunkSeq[u64] }\npub fn f(sink s: S) -> u64 { 0 }",
            "cannot export 'f': type S is a handle that owns a sequence, taken by value",
        ),
        (
            "pub fn f(in b: Box[i64]) -> i64 { 0 }",
            "cannot export 'f': type Box[i64] owns memory",
        ),
        (
            "pub fn s() -> String { \"hi\" }",
            "cannot export 's': type String has no fixed word layout",
        ),
        (
            "pub fn f(in xs: Slice[String]) -> u64 { 0 }",
            "cannot export 'f': type String",
        ),
        (
            "effect Tick { fn tick() -> i64; }\npub fn f() -> i64 eff [Tick] { Tick::tick() }",
            "cannot export 'f': effect Tick is not exportable",
        ),
        (
            "copy struct Pair[T] { a: T, b: T }\npub fn p(x: Pair[i64]) -> i64 { x.a }",
            "cannot export 'p'",
        ),
        (
            "// @export instance missing[u64]\npub fn one() -> i64 { 1 }",
            "cannot export instance 'missing[u64]'",
        ),
        (
            "// @export instance id[u64, u8]\npub fn id[T](sink x: T) -> T { x }",
            "cannot export instance 'id[u64, u8]'",
        ),
        (
            "// @export everything\npub fn one() -> i64 { 1 }",
            "@export",
        ),
    ];
    for (source, expected) in cases {
        let message = refusal(source);
        assert!(
            message.contains(expected),
            "{source}\nexpected '{expected}', got: {message}"
        );
    }
}

#[test]
fn instance_spellings_are_checked() {
    for bad in [
        "first",
        "first[]",
        "= first[u64]",
        "first[u64",
        "Bad = first[u64]",
    ] {
        assert!(
            matches!(
                parse_instance(bad),
                Err(ExportError::Instance { .. } | ExportError::Directive(_))
            ),
            "{bad} parsed"
        );
    }
}

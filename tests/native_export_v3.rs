//! Export ABI v3 (`vesper.native.export-abi/3`, #934): copy structs cross
//! in place through `#[repr(C)]` mirrors. The pilot's `gauge` library
//! (crates/vesper-port-pilot) runs every form end to end; these tests pin
//! the interface, the generated layout asserts, the directive, and that v1
//! and v2 libraries are unchanged byte for byte.
use vesper::native::export::{
    self, AbiType, BindingOptions, LibraryOptions, Mirror, compile_source, rust_binding,
};

const GAUGE: &str = include_str!("../crates/vesper-port-pilot/vesper/gauge.ves");
const KIT: &str = include_str!("../crates/vesper-port-pilot/vesper/kit.ves");

fn options(library: &'static str) -> LibraryOptions<'static> {
    LibraryOptions {
        library,
        target: None,
        optimize: true,
    }
}

fn binding(interface: &export::LibraryInterface, path: &str) -> String {
    rust_binding(
        interface,
        &BindingOptions {
            types_path: Some(path.into()),
        },
    )
}

#[test]
fn gauge_interface_marks_mirrors_and_in_place_parameters() {
    let gauge = compile_source("gauge.ves", GAUGE, &options("gauge")).unwrap();
    let i = &gauge.interface;
    assert_eq!(i.abi, export::EXPORT_ABI_V3);
    assert!(i.is_v3() && i.is_v2());
    assert!(i.runtime.is_some());
    for ty in ["Gauge", "Sample", "Span"] {
        assert_eq!(i.types[ty].mirror, Some(Mirror::ReprC), "{ty}");
    }
    assert!(matches!(
        &i.types["Phase"].mirror,
        Some(Mirror::Converted { reason }) if reason.contains("enum")
    ));
    let f = |name: &str| i.functions.iter().find(|f| f.name == name).unwrap();
    let record = f("gauge_record");
    assert!(record.params.iter().all(|p| p.in_place));
    assert_eq!(record.params[0].pass(), "inout");
    let scan = f("gauge_scan");
    assert!(matches!(&scan.params[1].ty, AbiType::Slice { .. }));
    assert!(scan.params[1].in_place);
    assert!(f("shift_samples").params[0].in_place);
    assert!(f("sample_score").params[0].in_place);
    // A consumed mirror is copied, and a converted enum result is not in place.
    assert!(!f("span_width").params[0].in_place);
    assert!(f("gauge_new").result_in_place);
    assert!(!f("gauge_phase").result_in_place);
    let json = export::interface_json(i).unwrap();
    assert!(json.contains("\"kind\": \"repr_c\""), "{json}");
    assert!(json.contains("\"in_place\": true"));
}

#[test]
fn gauge_binding_asserts_every_offset() {
    let gauge = compile_source("gauge.ves", GAUGE, &options("gauge")).unwrap();
    let text = binding(&gauge.interface, "crate::gauge");
    // The committed binding is what the compiler writes.
    let committed = include_str!("../crates/vesper-port-pilot/port/gauge.rs");
    assert_eq!(text, committed.replace("\r\n", "\n"));
    let decl = &gauge.interface.types["Gauge"];
    assert!(text.contains(&format!(
        "assert!(core::mem::size_of::<repr::Gauge>() == {} * 8);",
        decl.words
    )));
    for field in &decl.variants[0].fields {
        let line = format!(
            "assert!(core::mem::offset_of!(repr::Gauge, {}) == {} * 8);",
            field.name, field.offset
        );
        assert!(text.contains(&line), "missing {line}");
    }
    // Every cell kind appears.
    for cell in [
        "pub count: u64,",
        "pub last: vesper_port_abi::Word<i32>,",
        "pub seen: vesper_port_abi::Word<bool>,",
        "pub phase: vesper_port_abi::Word<crate::gauge::Phase>,",
        "pub peak: vesper_port_abi::Words<Option<u64>, 2>,",
        "pub span: Span,",
        "pub ratio: f64,",
    ] {
        assert!(text.contains(cell), "missing cell {cell}");
    }
    // In place: no word buffer for the state or the slice.
    assert!(text.contains("pub fn gauge_record(g: &mut repr::Gauge, s: &repr::Sample) -> ()"));
    assert!(
        text.contains("pub fn gauge_scan(g: &mut repr::Gauge, samples: &[repr::Sample]) -> u64")
    );
    assert!(text.contains("vesper_port_abi::with_mirror_slice_mut(samples"));
    assert!(!text.contains("arg_g"));
}

#[test]
fn a_moved_field_moves_its_assert() {
    // The generator derives every assert from the interface, so a layout
    // that changes on the Vesper side changes the asserts the committed
    // mirror must pass (and vesper-port-abi's compile_fail doctests show
    // a wrong offset does not compile).
    let gauge = compile_source("gauge.ves", GAUGE, &options("gauge")).unwrap();
    let mut moved = gauge.interface.clone();
    let fields = &mut moved.types.get_mut("Sample").unwrap().variants[0].fields;
    fields.swap(0, 1);
    let (a, b) = (fields[0].offset, fields[1].offset);
    fields[0].offset = b;
    fields[1].offset = a;
    let text = binding(&moved, "crate::gauge");
    assert!(text.contains("assert!(core::mem::offset_of!(repr::Sample, level) == 0 * 8);"));
    assert_ne!(
        text,
        binding(&gauge.interface, "crate::gauge"),
        "a moved field must change the binding"
    );
}

#[test]
fn v2_libraries_are_unchanged() {
    let kit = compile_source("kit.ves", KIT, &options("kit")).unwrap();
    assert_eq!(kit.interface.abi, export::EXPORT_ABI_V2);
    let json = export::interface_json(&kit.interface).unwrap();
    for v3_key in ["mirror", "in_place", "result_in_place"] {
        assert!(!json.contains(v3_key), "v2 interface has {v3_key}");
    }
    let committed = include_str!("../crates/vesper-port-pilot/port/kit.rs");
    assert_eq!(
        binding(&kit.interface, "crate::kit"),
        committed.replace("\r\n", "\n")
    );
}

#[test]
fn v3_objects_carry_the_v2_runtime() {
    // v3 changes only what the Rust side passes; the object is the v2
    // object, with its runtime entry points.
    let v3 = compile_source("gauge.ves", GAUGE, &options("gauge")).unwrap();
    let has = |name: &[u8]| v3.object.windows(name.len()).any(|w| w == name);
    assert!(has(b"vesper_gauge_seq_new"));
    assert!(has(b"vesper_gauge_live_allocations"));
    // Without the directive the same source (which takes `inout`
    // parameters) is a v2 library with the same object.
    let plain = GAUGE.replace("// @export abi 3", "//");
    let v2 = compile_source("gauge.ves", &plain, &options("gauge")).unwrap();
    assert_eq!(v2.interface.abi, export::EXPORT_ABI_V2);
    assert_eq!(v2.object, v3.object);
}

#[test]
fn the_abi_directive_is_checked() {
    for (directive, needle) in [
        ("// @export abi 2", "expected 3"),
        ("// @export abi three", "expected 3"),
        ("// @export abi 3\n// @export abi 3", "named twice"),
    ] {
        let source = format!("{directive}\npub fn f(x: u64) -> u64 {{\n    x\n}}\n");
        match compile_source("t.ves", &source, &options("t")) {
            Ok(_) => panic!("accepted {directive}"),
            Err(e) => assert!(e.to_string().contains(needle), "{directive}: {e}"),
        }
    }
    let ok = "// @export abi 3\npub fn f(x: u64) -> u64 {\n    x\n}\n";
    let lib = compile_source("t.ves", ok, &options("t")).unwrap();
    assert_eq!(lib.interface.abi, export::EXPORT_ABI_V3);
}

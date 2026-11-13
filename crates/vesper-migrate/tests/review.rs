//! Regression tests for review findings on PR #140.

use vesper_migrate::{Limits, Migration, RefusalReason, c, lift::render_param, rust, synth};

fn rs(src: &str) -> Migration {
    rust::analyze_source("t", src, Limits::default()).unwrap()
}

fn reason(m: &Migration, item: &str) -> RefusalReason {
    m.all_refusals()
        .into_iter()
        .find(|r| r.item == item)
        .unwrap_or_else(|| panic!("no refusal for {item}: {:#?}", m.all_refusals()))
        .reason
}

fn opts() -> synth::SysSpecOptions {
    synth::SysSpecOptions {
        library: "T".into(),
        target: vesper_ffi::sys::Target::LinuxX86_64,
        link: vesper_ffi::sys::LinkMode::Static,
        link_name: "t".into(),
        artifact_blake3: [0; 32],
        header: None,
        pkg_config: None,
    }
}

#[test]
fn conditional_items_are_refused_not_exported() {
    let m = rs(r#"
        #[cfg(unix)] pub fn os_name() -> u32 { 1 }
        #[cfg(windows)] pub fn os_name() -> u32 { 2 }
        #[cfg(feature = "x")] #[no_mangle] pub extern "C" fn gated(x: u32) -> u32 { x }
        #[cfg_attr(unix, no_mangle)] pub extern "C" fn maybe(x: u32) -> u32 { x }
        #[cfg(unix)] pub mod unix_only { pub fn f() {} }
        pub struct S { pub a: u32 }
        impl S { #[cfg(test)] pub fn probe(&self) -> u32 { self.a } }
    "#);
    assert!(m.functions.is_empty(), "{:#?}", m.functions);
    for item in [
        "fn os_name",
        "fn gated",
        "fn maybe",
        "mod unix_only",
        "fn S::probe",
    ] {
        assert_eq!(reason(&m, item), RefusalReason::UnsupportedItem, "{item}");
    }
    let e = synth::sys_spec(&m, &opts()).unwrap_err();
    assert_eq!(e.kind, vesper_migrate::ErrorKind::Synthesis);
    assert!(
        e.message.contains("no function has a C-ABI signature"),
        "{e}"
    );
}

#[test]
fn index_expressions_inside_projected_places_are_scanned() {
    let m = rs(r#"
        pub struct S { pub items: [u32; 4], pub cursor: usize, pub other: u32 }
        pub fn step(s: &mut S) { s.items[s.cursor] += 1; }
        pub fn assign(s: &mut S) { s.items[s.cursor] = s.other; }
        pub fn whole_in_index(s: &mut S) { s.items[pick(s)] = 0; }
        pub fn read(s: &S) -> u32 { s.items[s.cursor] }
        pub fn borrow(s: &mut S) { let r = &mut s.items[s.cursor]; *r = 0; }
        fn pick(_s: &S) -> usize { 0 }
    "#);
    let p = |n: &str| render_param(&m.function(n).unwrap().params[0]);
    assert_eq!(p("step"), "inout s.{cursor, items}: S");
    assert_eq!(p("assign"), "inout s.{cursor, items, other}: S");
    assert_eq!(p("whole_in_index"), "inout s: S");
    assert_eq!(p("read"), "in s.{cursor, items}: S");
    assert_eq!(p("borrow"), "inout s.{cursor, items}: S");
}

#[test]
fn structs_storing_references_or_raw_pointers_are_refused() {
    let m = rs(r#"
        pub struct R { pub p: &'static mut u32 }
        pub struct P { pub p: *mut u32 }
        pub struct V { pub v: Vec<&'static u8> }
        pub fn bump(s: &mut R) { *s.p += 1; }
        pub fn raw(s: &P) {}
    "#);
    assert_eq!(reason(&m, "struct R"), RefusalReason::BorrowingType);
    assert_eq!(reason(&m, "struct P"), RefusalReason::BorrowingType);
    assert_eq!(reason(&m, "struct V"), RefusalReason::BorrowingType);
    assert_eq!(reason(&m, "fn bump"), RefusalReason::BorrowingType);
    assert_eq!(reason(&m, "fn raw"), RefusalReason::BorrowingType);
}

#[test]
fn repr_modifiers_block_wire_layouts() {
    let m = rs(r#"
        #[repr(C, align(16))] pub struct A { pub x: u32 }
        #[repr(C, packed)] pub struct B { pub a: u8, pub x: u32 }
        #[repr(C)] #[repr(packed(2))] pub struct D { pub x: u32 }
        #[repr(transparent)] pub struct T { pub x: u32 }
        #[repr(C)] pub struct Ok1 { pub x: u32 }
    "#);
    for s in ["struct A", "struct B", "struct D", "struct T"] {
        assert_eq!(reason(&m, s), RefusalReason::WireLayout, "{s}");
    }
    assert_eq!(synth::wire_sizes(&m), vec![("Ok1".to_string(), 4, 4)]);
}

#[test]
fn aliases_defined_in_two_modules_are_refused() {
    let m = rs(r#"
        pub mod a { pub type Word = u32; #[no_mangle] pub extern "C" fn fa(x: Word) -> Word { x } }
        pub mod b { pub type Word = u64; #[no_mangle] pub extern "C" fn fb(x: Word) -> Word { x } }
        pub type Single = u16;
        #[no_mangle] pub extern "C" fn fs(x: Single) -> Single { x }
    "#);
    assert_eq!(reason(&m, "fn fa"), RefusalReason::UnsupportedType);
    assert_eq!(reason(&m, "fn fb"), RefusalReason::UnsupportedType);
    let spec = synth::sys_spec(&m, &opts()).unwrap();
    assert!(
        spec.text.contains("fn fs(x: u16) -> ForeignVal[u16];"),
        "{}",
        spec.text
    );
}

#[test]
fn c_constants_outside_int_semantics_are_refused() {
    let h = |src: &str| c::analyze_header("t.h", src, Limits::default()).unwrap();
    for src in [
        "enum e { A = 0xffffffffu + 1u, B };",
        "enum e { A = 2147483647 + 1 };",
        "enum e { A = 1 << 31 };",
        "enum e { A = 0x80000000 };",
        "enum e { A = 5L };",
        "enum e { A = -1 << 2 };",
    ] {
        let m = h(src);
        assert_eq!(
            reason(&m, "enum e"),
            RefusalReason::UnsupportedType,
            "{src}"
        );
        assert!(
            m.enumeration("e").unwrap().variants.is_empty(),
            "{src}: no values are guessed"
        );
    }
    let m = h("enum e { A = -2147483647 - 1, B = 0x7fffffff };");
    assert_eq!(
        m.enumeration("e").unwrap().variants,
        [
            ("A".to_string(), -2147483648),
            ("B".to_string(), 2147483647)
        ]
    );
    let m = h("struct s { uint8_t b[1u]; };");
    assert_eq!(reason(&m, "struct s"), RefusalReason::UnsupportedType);
}

#[test]
fn renamed_parameters_never_collide() {
    let m = c::analyze_header(
        "t.h",
        "uint32_t f(uint32_t in, uint32_t in_, uint32_t in__);",
        Limits::default(),
    )
    .unwrap();
    let names: Vec<_> = m
        .function("f")
        .unwrap()
        .params
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(names, ["in___", "in_", "in__"]);
    synth::sys_spec(&m, &opts()).unwrap();
    let m = rs("#[no_mangle] pub extern \"C\" fn g(sink: u32, sink_: u32) -> u32 { sink + sink_ }");
    let names: Vec<_> = m
        .function("g")
        .unwrap()
        .params
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(names, ["sink__", "sink_"]);
    synth::sys_spec(&m, &opts()).unwrap();
}

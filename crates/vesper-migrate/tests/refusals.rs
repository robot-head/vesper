//! Refusal and hostile-input tests for both front ends: unsupported forms are
//! typed refusals or typed errors, never success and never panics.

use vesper_migrate::{ErrorKind, Limits, Migration, RefusalReason, c, rust, synth};

fn header(src: &str) -> Migration {
    c::analyze_header("t.h", src, Limits::default()).unwrap()
}

fn header_err(src: &str) -> ErrorKind {
    c::analyze_header("t.h", src, Limits::default())
        .unwrap_err()
        .kind
}

fn crate_src(src: &str) -> Migration {
    rust::analyze_source("t", src, Limits::default()).unwrap()
}

fn reason(m: &Migration, item: &str) -> RefusalReason {
    m.all_refusals()
        .into_iter()
        .find(|r| r.item == item)
        .unwrap_or_else(|| panic!("no refusal for {item}: {:#?}", m.all_refusals()))
        .reason
}

#[test]
fn c_preprocessor_subset_is_enforced() {
    assert_eq!(header_err("#define N 4\n"), ErrorKind::Preprocessor);
    assert_eq!(
        header_err("#include \"other.h\"\n"),
        ErrorKind::Preprocessor
    );
    assert_eq!(header_err("#if 1\n#endif\n"), ErrorKind::Preprocessor);
    assert_eq!(
        header_err("#ifndef G\n#define G\n"),
        ErrorKind::Preprocessor,
        "unterminated guard"
    );
    assert_eq!(header_err("#endif\n"), ErrorKind::Preprocessor);
    assert_eq!(header_err("int f(void); \\\n"), ErrorKind::Preprocessor);
    assert_eq!(header_err("/* never closed"), ErrorKind::Syntax);
    assert_eq!(header_err("int f(void"), ErrorKind::Syntax);
    assert_eq!(header_err("typedef int uint32_t;"), ErrorKind::Duplicate);
    assert_eq!(
        header_err("int f(void); int f(void);"),
        ErrorKind::Duplicate
    );
    assert_eq!(
        header_err("struct s { int a; }; struct s { int b; };"),
        ErrorKind::Duplicate
    );
    // The accepted subset parses, and `__cplusplus` blocks are skipped.
    let m = header(
        "#pragma once\n#include <stdbool.h>\n#ifdef __cplusplus\nextern \"C\" {\n#endif\nint32_t f(void);\n#ifdef __cplusplus\n}\n#endif\n",
    );
    assert_eq!(m.functions.len(), 1);
}

#[test]
fn c_line_numbers_point_into_the_header() {
    let e = c::analyze_header(
        "t.h",
        "int32_t ok(void);\n\nint32_t broken(;\n",
        Limits::default(),
    )
    .unwrap_err();
    assert_eq!((e.kind, e.line), (ErrorKind::Syntax, Some(3)));
    let e = c::analyze_header("t.h", "\n#define X 1\n", Limits::default()).unwrap_err();
    assert_eq!(e.line, Some(2));
}

#[test]
fn c_type_refusals() {
    let m = header(
        "char f1(void);
         void f2(bool b);
         void f3(uint32_t *p);
         void f4(void *p, size_t n);
         void f5(struct s *p, size_t n);
         void f6(int32_t a[2][3]);
         uint32_t f7(uint32_t x) ;
         struct bits { uint32_t a : 3; };
         struct anon_member { struct { int32_t x; } ; };
         struct holds_ptr { uint32_t *p; };
         struct holds_words { uint32_t w[2]; };
         struct outer { struct padded inner; };
         struct padded { uint8_t a; uint64_t b; };
         _Atomic(uint32_t) f8(void);",
    );
    assert_eq!(reason(&m, "fn f1"), RefusalReason::PlatformDependentWidth);
    assert_eq!(
        reason(&m, "fn f2"),
        RefusalReason::ImplementationDefinedCarrier
    );
    assert_eq!(reason(&m, "fn f3"), RefusalReason::PointerParameter);
    assert_eq!(reason(&m, "fn f4"), RefusalReason::OpaquePointer);
    assert_eq!(reason(&m, "fn f5"), RefusalReason::Aggregate);
    assert_eq!(reason(&m, "fn f6"), RefusalReason::UnsupportedType);
    assert_eq!(reason(&m, "fn f8"), RefusalReason::InteriorMutability);
    assert!(m.function("f7").is_some());
    assert_eq!(reason(&m, "struct bits"), RefusalReason::Bitfield);
    assert_eq!(reason(&m, "struct holds_ptr"), RefusalReason::Aggregate);
    assert_eq!(reason(&m, "struct holds_words"), RefusalReason::WireLayout);
    assert_eq!(reason(&m, "struct padded"), RefusalReason::WireLayout);
    // A container of a refused struct is refused at the fixpoint.
    assert_eq!(reason(&m, "struct outer"), RefusalReason::WireLayout);
}

#[test]
fn c_enum_constant_expressions() {
    let m = header("enum e { A = 1 << 4, B = A + 1, C = -3, D, E = 0x10 | 0x01, F = 017 };");
    let e = m.enumeration("e").unwrap();
    assert_eq!(
        e.variants,
        [
            ("A".into(), 16),
            ("B".into(), 17),
            ("C".into(), -3),
            ("D".into(), -2),
            ("E".into(), 17),
            ("F".into(), 15)
        ]
    );
    let m = header("enum e { A = sizeof(int) };");
    assert_eq!(reason(&m, "enum e"), RefusalReason::UnsupportedType);
}

#[test]
fn reserved_parameter_names_are_renamed_safely() {
    let m = header("uint32_t f(uint32_t in, const uint8_t *fn, size_t n);");
    let spec = synth::sys_spec(&m, &opts()).unwrap();
    assert!(
        spec.text
            .contains("fn f(in_: u32, nocapture in fn_: CSlice[u8]) -> ForeignVal[u32];"),
        "{}",
        spec.text
    );
}

fn opts() -> synth::SysSpecOptions {
    synth::SysSpecOptions {
        library: "T".into(),
        target: vesper_ffi::sys::Target::LinuxAarch64,
        link: vesper_ffi::sys::LinkMode::Dynamic,
        link_name: "t".into(),
        artifact_blake3: [0; 32],
        header: None,
        pkg_config: None,
    }
}

#[test]
fn bad_sys_options_are_a_typed_synthesis_error() {
    let m = header("uint32_t f(void);");
    let mut o = opts();
    o.link_name = "-lt --evil".into();
    assert_eq!(
        synth::sys_spec(&m, &o).unwrap_err().kind,
        ErrorKind::Synthesis
    );
    let mut o = opts();
    o.header = Some("../escape.h".into());
    assert_eq!(
        synth::sys_spec(&m, &o).unwrap_err().kind,
        ErrorKind::Synthesis
    );
}

#[test]
fn rust_refusals_have_reasons() {
    let m = crate_src(
        r#"
        pub struct Holder { pub cell: std::cell::RefCell<u32> }
        pub struct Counter { pub n: std::sync::atomic::AtomicU64 }
        pub struct Inner { pub h: Holder }
        pub struct View<'a> { pub b: &'a [u8] }
        pub struct Pair<T> { pub a: T }
        pub struct Tup(pub u32);
        pub enum Data { A(u32), B }
        pub enum Plain { A, B }
        #[repr(i32)] pub enum Signed { A = -1 }
        #[repr(u8)] pub enum Wide { A = 300 }
        pub fn f1(h: &Holder) {}
        pub fn f2(c: &Counter) {}
        pub fn f3(i: &mut Inner) {}
        pub fn f4<'a>(v: &'a mut View<'a>) {}
        pub fn f5(v: &View<'_>) {}
        pub fn f6(x: &u32) -> &u32 { x }
        pub fn f7(x: &[u8]) -> Option<&u8> { x.first() }
        pub fn f8(p: *mut u32) {}
        pub fn f9(x: impl Fn()) {}
        pub fn f10(x: &dyn std::fmt::Debug) {}
        pub fn f11(x: Box<dyn Fn(u32)>) {}
        pub async fn f12() {}
        pub fn f13((a, b): (u32, u32)) {}
        pub fn f14(x: Pair<u32>) {}
        pub fn f15(x: Data) {}
        pub fn f16(x: Unknown) {}
        pub fn f17(x: &&u32) {}
        pub fn f18(x: Vec<&u32>) {}
        pub fn f19() -> *const u8 { std::ptr::null() }
        /// vesper-migrate: p sideways
        pub fn f20(p: *const u8) {}
        /// vesper-migrate: p inout
        pub fn f21(p: *const u8) {}
        /// vesper-migrate: p in len n
        pub fn f22(p: *const u8, m: usize) {}
        /// vesper-migrate: q in
        pub fn f23(p: u32) {}
        pub fn f24(x: Tup) {}
        pub fn f25(x: Plain) -> u32 { 0 }
        pub trait T {}
        pub static S: u32 = 0;
        "#,
    );
    let r = |n: &str| reason(&m, n);
    assert_eq!(r("fn f1"), RefusalReason::InteriorMutability);
    assert_eq!(r("fn f2"), RefusalReason::InteriorMutability);
    assert_eq!(
        r("struct Inner"),
        RefusalReason::InteriorMutability,
        "transitive"
    );
    assert_eq!(r("fn f3"), RefusalReason::InteriorMutability);
    assert_eq!(r("fn f4"), RefusalReason::SelfReferentialLifetime);
    assert_eq!(r("fn f5"), RefusalReason::BorrowingType);
    assert_eq!(r("fn f6"), RefusalReason::ReturnedBorrow);
    assert_eq!(r("fn f7"), RefusalReason::BorrowingType);
    assert_eq!(r("fn f8"), RefusalReason::RawPointerWithoutAnnotation);
    assert_eq!(r("fn f9"), RefusalReason::Generic);
    assert_eq!(r("fn f10"), RefusalReason::Generic);
    assert_eq!(r("fn f11"), RefusalReason::Generic);
    assert_eq!(r("fn f12"), RefusalReason::UnsupportedItem);
    assert_eq!(r("fn f13"), RefusalReason::UnsupportedItem);
    assert_eq!(r("fn f14"), RefusalReason::Generic);
    assert_eq!(r("fn f15"), RefusalReason::UnsupportedItem);
    assert_eq!(r("fn f16"), RefusalReason::UnsupportedType);
    assert_eq!(r("fn f17"), RefusalReason::BorrowingType);
    assert_eq!(r("fn f18"), RefusalReason::BorrowingType);
    assert_eq!(r("fn f19"), RefusalReason::RawPointerWithoutAnnotation);
    assert_eq!(r("fn f20"), RefusalReason::BadAnnotation);
    assert_eq!(r("fn f21"), RefusalReason::BadAnnotation);
    assert_eq!(r("fn f22"), RefusalReason::BadAnnotation);
    assert_eq!(r("fn f23"), RefusalReason::BadAnnotation);
    assert_eq!(r("fn f24"), RefusalReason::UnsupportedItem);
    assert_eq!(r("fn f25: vsys"), RefusalReason::NotCAbi);
    assert_eq!(r("enum Plain"), RefusalReason::ImplementationDefinedCarrier);
    assert_eq!(
        r("enum Signed"),
        RefusalReason::ImplementationDefinedCarrier
    );
    assert_eq!(r("enum Wide"), RefusalReason::UnsupportedType);
    assert_eq!(r("trait T"), RefusalReason::UnsupportedItem);
    assert_eq!(r("static S"), RefusalReason::UnsupportedItem);
}

#[test]
fn rust_c_abi_requires_extern_c_and_no_mangle() {
    let m = crate_src(
        r#"
        pub extern "C" fn mangled(x: u32) -> u32 { x }
        #[no_mangle] pub extern "system" fn sys_abi(x: u32) -> u32 { x }
        #[no_mangle] pub extern "C" fn borrowed(x: &u32) -> u32 { *x }
        #[no_mangle] pub extern "C" fn sliced(x: &[u8]) -> u32 { 0 }
        #[no_mangle] pub extern "C" fn wide(x: isize) -> u32 { 0 }
        #[no_mangle] pub extern "C" fn float(x: f64) -> u32 { 0 }
        #[no_mangle] pub extern "C" fn ok(x: u32, y: bool) -> i64 { 0 }
        "#,
    );
    assert_eq!(reason(&m, "fn mangled: vsys"), RefusalReason::NotCAbi);
    assert_eq!(reason(&m, "fn sys_abi: vsys"), RefusalReason::NotCAbi);
    assert_eq!(
        reason(&m, "fn borrowed: vsys"),
        RefusalReason::PointerParameter
    );
    assert_eq!(reason(&m, "fn sliced: vsys"), RefusalReason::NotCAbi);
    assert_eq!(
        reason(&m, "fn wide: vsys"),
        RefusalReason::PlatformDependentWidth
    );
    assert_eq!(reason(&m, "fn float: vsys"), RefusalReason::Floating);
    let spec = synth::sys_spec(&m, &opts()).unwrap();
    assert_eq!(spec.descriptor.functions().len(), 1);
    assert!(
        spec.text
            .contains("fn ok(x: u32, y: bool) -> ForeignVal[i64];")
    );
    assert_eq!(spec.omitted.len(), 6);
}

#[test]
fn rust_body_scan_is_conservative() {
    let m = crate_src(
        r#"
        pub struct S { pub a: u32, pub b: Vec<u8>, pub c: [u8; 4], pub d: u64 }
        pub fn compound(s: &mut S) { s.a += 1; }
        pub fn index_write(s: &mut S, i: usize) { s.c[i] = 0; }
        pub fn method_on_field(s: &mut S) { s.b.push(1); }
        pub fn shared_method(s: &S) -> usize { s.b.len() }
        pub fn mut_ref(s: &mut S) { let r = &mut s.a; *r = 1; }
        pub fn passes_on(s: &mut S) { helper(s); }
        pub fn shadowed(s: &mut S) { let s = 1u32; }
        pub fn in_macro(s: &S) { println!("{}", s.a); }
        pub fn closure(s: &mut S) { let mut f = || s.d = 2; f(); }
        pub fn read_only_unique(s: &mut S) -> u32 { s.a }
        pub fn unused(s: &mut S) {}
        pub fn deref_field(s: &mut S) { (*s).a = 3; }
        fn helper(_s: &mut S) {}
        "#,
    );
    let p = |n: &str| vesper_migrate::lift::render_param(&m.function(n).unwrap().params[0]);
    assert_eq!(p("compound"), "inout s.{a}: S");
    assert_eq!(p("index_write"), "inout s.{c}: S");
    assert_eq!(p("method_on_field"), "inout s.{b}: S");
    assert_eq!(p("shared_method"), "in s.{b}: S");
    assert_eq!(p("mut_ref"), "inout s.{a}: S");
    assert_eq!(p("passes_on"), "inout s: S");
    assert_eq!(p("shadowed"), "inout s: S");
    assert_eq!(p("in_macro"), "in s: S");
    assert_eq!(p("closure"), "inout s.{d}: S");
    assert_eq!(p("read_only_unique"), "in s.{a}: S");
    assert_eq!(p("unused"), "in s: S");
    assert_eq!(p("deref_field"), "inout s.{a}: S");
    assert!(
        m.function("helper").is_none(),
        "private items are not lifted"
    );
    assert_eq!(
        reason(&m, "struct S"),
        RefusalReason::WireLayout,
        "no #[repr(C)]"
    );
}

#[test]
fn rust_errors_are_typed() {
    let e = rust::analyze_source("t", "pub fn f( {", Limits::default()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Syntax);
    let e = rust::analyze_source(
        "t",
        "pub struct A; pub mod m { pub struct A; }",
        Limits::default(),
    )
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Duplicate);
    let e = rust::analyze_crate_dir(
        std::path::Path::new("/definitely/not/a/crate"),
        Limits::default(),
    )
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Io);
}

#[test]
fn budgets_and_nesting_are_enforced() {
    let small = Limits {
        source_bytes: 16,
        ..Limits::default()
    };
    assert_eq!(
        c::analyze_header("t.h", "uint32_t f(void); uint32_t g(void);", small)
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
    assert_eq!(
        rust::analyze_source("t", "pub fn f() {} pub fn g() {}", small)
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
    let deep = format!("pub fn f() {{ {} }}", "(".repeat(5000) + &")".repeat(5000));
    assert_eq!(
        rust::analyze_source("t", &deep, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
    let deep_c = format!(
        "int32_t f(void) {{ return {}0{}; }}",
        "(".repeat(5000),
        ")".repeat(5000)
    );
    assert_eq!(
        c::analyze_header("t.h", &deep_c, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
    let generic = format!(
        "pub fn f(x: {}u8{}) {{}}",
        "Vec<".repeat(5000),
        ">".repeat(5000)
    );
    assert_eq!(
        rust::analyze_source("t", &generic, Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
    let few = Limits {
        items: 2,
        ..Limits::default()
    };
    assert_eq!(
        c::analyze_header(
            "t.h",
            "uint32_t a(void); uint32_t b(void); uint32_t c(void);",
            few
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
    assert_eq!(
        rust::analyze_source("t", "pub fn a() {} pub fn b() {} pub fn c() {}", few)
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
    let params = Limits {
        parameters: 1,
        ..Limits::default()
    };
    assert_eq!(
        c::analyze_header("t.h", "uint32_t a(uint32_t x, uint32_t y);", params)
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
    // Long shift chains are not mistaken for generic nesting.
    let shifts = format!("pub fn f() -> u32 {{ 1 {} }}", "<< 0 ".repeat(1000));
    assert!(rust::analyze_source("t", &shifts, Limits::default()).is_ok());
}

/// Every prefix and a byte-flipped copy of the fixtures is either a typed
/// result or a typed error, never a panic.
#[test]
fn truncated_and_corrupted_fixtures_never_panic() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let h = std::fs::read_to_string(root.join("legacy_codec.h")).unwrap();
    let r = std::fs::read_to_string(root.join("legacy_codec/src/lib.rs")).unwrap();
    let cut = |s: &str, n: usize| -> String {
        s.char_indices()
            .take_while(|(i, _)| *i < n)
            .map(|(_, c)| c)
            .collect()
    };
    for n in (0..h.len()).step_by(7) {
        let _ = c::analyze_header("t.h", &cut(&h, n), Limits::default());
    }
    for n in (0..r.len()).step_by(11) {
        let _ = rust::analyze_source("t", &cut(&r, n), Limits::default());
    }
    let mut x = 0x1234_5678_u64;
    for _ in 0..200 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let mut hb = h.clone().into_bytes();
        let i = (x as usize) % hb.len();
        hb[i] = b"(){}[]*;,#\"'/\\<>"[(x >> 32) as usize % 16];
        if let Ok(s) = String::from_utf8(hb)
            && let Ok(m) = c::analyze_header("t.h", &s, Limits::default())
        {
            let _ = synth::vesper_declarations(&m);
            let _ = synth::report(&m);
        }
        let mut rb = r.clone().into_bytes();
        let i = (x as usize) % rb.len();
        rb[i] = b"(){}[]*;,#\"'/\\<>&"[(x >> 40) as usize % 17];
        if let Ok(s) = String::from_utf8(rb)
            && let Ok(m) = rust::analyze_source("t", &s, Limits::default())
        {
            let _ = synth::vesper_declarations(&m);
        }
    }
}

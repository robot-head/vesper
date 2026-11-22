//! `.vsys` callback handles at the export boundary (#1502, graphics gap
//! 10 in docs/parallel-results/rust-exit-graphics.md). A callback type is a
//! copy handle, a C function pointer the library resolved, so a copy value
//! holding one may cross as a parameter or result: Rust sees the opaque
//! word `vesper_port_abi::FnPtr`, which it can hold and hand back but not
//! build or call. Linear and affine extern-type handles keep the v4 rule.
//! The pilot's `fnptrs` library (crates/vesper-port-pilot) runs every form
//! end to end; these tests pin the interface and the committed binding.
use vesper::native::export::{
    self, AbiType, BindingOptions, LibraryOptions, compile_source, rust_binding,
};

const FNPTRS: &str = include_str!("../crates/vesper-port-pilot/vesper/fnptrs.ves");

fn options(library: &'static str, target: Option<&'static str>) -> LibraryOptions<'static> {
    LibraryOptions {
        library,
        target,
        optimize: true,
    }
}

fn library() -> export::LibraryInterface {
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ] {
        let lib = compile_source("fnptrs.ves", FNPTRS, &options("fnptrs", Some(target)))
            .unwrap_or_else(|e| panic!("{target}: {e}"));
        assert!(!lib.object.is_empty());
    }
    compile_source("fnptrs.ves", FNPTRS, &options("fnptrs", None))
        .unwrap()
        .interface
}

fn callback() -> AbiType {
    AbiType::Callback {
        name: "Binary".into(),
    }
}

#[test]
fn callback_handles_cross_as_one_word() {
    let i = library();
    let f = |name: &str| i.functions.iter().find(|f| f.name == name).unwrap();
    assert_eq!(f("call").params[0].ty, callback());
    assert_eq!(f("pick").result, callback());
    assert_eq!(i.types["Table"].words, 3);
    assert!(!i.types["Table"].handle);
    let fields: Vec<_> = i.types["Table"].variants[0]
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.ty.clone()))
        .collect();
    assert_eq!(fields[0], ("add", callback()));
    assert_eq!(fields[1], ("mul", callback()));
    let json = serde_json::to_string(&f("call").params[0].ty).unwrap();
    assert_eq!(json, r#"{"kind":"callback","name":"Binary"}"#);
}

#[test]
fn committed_binding_is_what_the_compiler_writes() {
    let text = rust_binding(&library(), &BindingOptions::default());
    let committed = include_str!("../crates/vesper-port-pilot/port/fnptrs.rs");
    assert_eq!(text, committed.replace("\r\n", "\n"));
    assert!(text.contains("pub add: vesper_port_abi::FnPtr<"));
    // Each callback type is its own Rust type.
    let binary = "vesper_port_abi::FnPtr<0xd6f13188220f3d1c>";
    let unary = "vesper_port_abi::FnPtr<0x4fa372b53368d170>";
    assert!(text.contains(&format!("pub unsafe fn call_unary(f: {unary}")));
    assert!(text.contains(&format!("pub unsafe fn call(f: {binary}")));
    // A wrapper taking a callback, directly or in a field, is `unsafe`;
    // one only returning it is not.
    assert!(text.contains("pub unsafe fn combine(t: &repr::Table"));
    assert!(text.contains(&format!("pub fn missing() -> Option<{binary}>")));
}

/// A linear extern-type handle in a copy struct is still refused: Rust
/// would copy a value whose release the library owns.
#[test]
fn extern_type_handles_keep_the_lent_rule() {
    let source = "// @export abi 4\n\
        // @export vsys direct rust=crate::shim\n\
        // | sys library Files (target = \"@TARGET@\", abi = \"@ABI@\", link = \"static\", name = \"files\",\n\
        // |     artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {\n\
        // |     linear extern type File;\n\
        // |     fn size(in f: File) -> ForeignVal[u64];\n\
        // |     fn close(sink f: File) -> ForeignVal[Unit];\n\
        // | }\n\
        pub fn size_of(in f: File) -> u64 eff [Ffi[Files]] { Files::size(in f) }\n\
        pub fn give(sink f: File) -> File { f }\n";
    let e = match compile_source("t.ves", source, &options("t", None)) {
        Ok(_) => panic!("a by-value extern handle crossed"),
        Err(e) => e.to_string(),
    };
    assert!(e.contains("opaque .vsys extern-type handle"), "{e}");
}

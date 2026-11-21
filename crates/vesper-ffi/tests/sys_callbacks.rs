//! Callback types and `#[link_name]` in `.vsys` (§16.3, SH4 lane 13,
//! #1287): `callback Name(..) -> ForeignVal[R];` declares a C function
//! pointer type, which is a copyable handle and whose call is the library
//! function `Name` taking the pointer first; `#[link_name = "sym"]` lets
//! several typed declarations import one C symbol, as loaders such as
//! `vkGetInstanceProcAddr` and `dlsym` need.
use vesper_ffi::sys::*;

fn library(link: &str, body: &str) -> Result<LibraryDescriptor, Error> {
    parse(
        &format!(
            r#"sys library Fixture (
            target = "x86_64-unknown-linux-gnu", abi = "sysv64",
            link = "dynamic", name = "{link}",
            artifact = "blake3:{}"
        ) {{ {body} }}"#,
            "0".repeat(64)
        ),
        Limits::default(),
    )
}
fn linux(body: &str) -> Result<LibraryDescriptor, Error> {
    library("c", body)
}

const LOADER: &str = r#"
    linear extern type Instance;
    fn destroy(sink i: Instance) -> ForeignVal[Unit];
    callback Add(a: u64, b: u64) -> ForeignVal[u64];
    callback Notify(in i: Instance, code: i32) -> ForeignVal[Unit];
    #[link_name = "lookup"]
    fn lookup_add(nocapture in name: CStr) -> ForeignVal[Option[Add]];
    #[link_name = "lookup"]
    fn lookup_notify(nocapture in name: CStr) -> ForeignVal[NonNull[Notify]];
"#;

#[test]
fn a_callback_type_is_a_copyable_handle_with_a_call() {
    let lib = linux(LOADER).unwrap();
    let add = lib.extern_type("Add").unwrap();
    assert_eq!(add.linearity(), Linearity::Callback);
    assert_eq!(add.drop_import(), None);
    let call = lib.function("Add").unwrap();
    assert!(call.is_indirect());
    assert_eq!(call.parameters().len(), 3);
    assert_eq!(call.parameters()[0].name(), CALLEE);
    assert!(matches!(
        call.parameters()[0].ty(),
        ParameterType::Handle { handle, access: HandleAccess::In } if handle.name() == "Add"
    ));
    // The pointer is the callee, not an argument word.
    let abi = call.abi();
    assert_eq!(
        abi.parameters,
        vec![AbiType::Integer(Scalar::U64), AbiType::Integer(Scalar::U64)]
    );
    assert_eq!(abi.result, Some(AbiType::Integer(Scalar::U64)));
    let notify = lib.function("Notify").unwrap();
    assert!(matches!(
        notify.parameters()[1].ty(),
        ParameterType::Handle { handle, access: HandleAccess::In } if handle.name() == "Instance"
    ));
}

#[test]
fn link_names_let_typed_declarations_share_a_symbol() {
    let lib = linux(LOADER).unwrap();
    let a = lib.function("lookup_add").unwrap();
    let n = lib.function("lookup_notify").unwrap();
    assert_eq!(a.link_symbol(), "lookup");
    assert_eq!(n.link_symbol(), "lookup");
    assert_eq!(a.link_name(), Some("lookup"));
    assert!(!a.is_indirect());
    assert!(matches!(
        a.result(),
        ValueType::Handle { handle, nullable: true } if handle.name() == "Add"
    ));
    assert!(matches!(
        n.result(),
        ValueType::NonNullHandle { handle } if handle.name() == "Notify"
    ));
    assert_eq!(lib.function("destroy").unwrap().link_symbol(), "destroy");
    assert_eq!(lib.function("destroy").unwrap().link_name(), None);
}

#[test]
fn callbacks_and_link_names_have_their_own_identity() {
    let plain = linux("fn lookup(nocapture in name: CStr) -> ForeignVal[u64];").unwrap();
    let renamed =
        linux(r#"#[link_name = "lookup"] fn find(nocapture in name: CStr) -> ForeignVal[u64];"#)
            .unwrap();
    let with_callback = linux(
        "fn lookup(nocapture in name: CStr) -> ForeignVal[u64]; callback Tick() -> ForeignVal[u64];",
    )
    .unwrap();
    assert_ne!(plain.identity(), renamed.identity());
    assert_ne!(plain.identity(), with_callback.identity());
    // The declarations are spelled in the canonical text.
    let text = String::from_utf8_lossy(with_callback.canonical_bytes()).into_owned();
    assert!(text.contains("callbacks"), "{text}");
    assert!(!text.contains("extern-types"), "{text}");
    let text = String::from_utf8_lossy(renamed.canonical_bytes()).into_owned();
    assert!(text.contains("link-name"), "{text}");
    // A library with neither keeps its bytes.
    let text = String::from_utf8_lossy(plain.canonical_bytes()).into_owned();
    assert!(!text.contains("callbacks") && !text.contains("link-name"));
}

#[test]
fn callback_parameters_are_words() {
    let e = linux("callback Sum(nocapture in xs: CSlice[u64]) -> ForeignVal[u64];").unwrap_err();
    assert!(e.to_string().contains("two words"), "{e}");
    // Pointers, C strings, handles and other callbacks are words.
    linux(
        "linear extern type H; fn close(sink h: H) -> ForeignVal[Unit];
         callback Hook(nocapture in s: CStr, nocapture inout n: u64, in h: H) -> ForeignVal[i32];
         callback Outer(in inner: Hook) -> ForeignVal[Unit];
         fn install(in hook: Outer) -> ForeignVal[i32];",
    )
    .unwrap();
}

#[test]
fn malformed_callbacks_and_link_names_are_refused() {
    for body in [
        // Not variadic.
        "callback F(a: i32, ..., b: i64) -> ForeignVal[i32]; fn g() -> ForeignVal[u64];",
        // A function pointer is lent `in` only.
        "callback F() -> ForeignVal[u64]; fn g(sink f: F) -> ForeignVal[Unit];",
        "callback F() -> ForeignVal[u64]; fn g(inout f: F) -> ForeignVal[Unit];",
        // A mode is still required, as for every handle.
        "callback F() -> ForeignVal[u64]; fn g(f: F) -> ForeignVal[Unit];",
        // Names are unique across functions and types.
        "callback g() -> ForeignVal[u64]; fn g() -> ForeignVal[u64];",
        "linear extern type F; fn close(sink f: F) -> ForeignVal[Unit]; callback F() -> ForeignVal[u64];",
        "callback u64() -> ForeignVal[u64];",
        "callback F(callee: u64) -> ForeignVal[u64];",
        // link_name names a C identifier and precedes a `fn`.
        r#"#[link_name = "a-b"] fn g() -> ForeignVal[u64];"#,
        r#"#[link_name = "lookup"] callback F() -> ForeignVal[u64];"#,
        r#"#[link = "lookup"] fn g() -> ForeignVal[u64];"#,
    ] {
        assert!(linux(body).is_err(), "{body}");
    }
}

#[test]
fn versioned_link_names_are_bare_names() {
    let body = "fn g_free(x: u64) -> ForeignVal[Unit];";
    assert_eq!(library("glib-2.0", body).unwrap().link().name(), "glib-2.0");
    for bad in ["../glib", "glib..2", ".glib", "a/b", "-lglib"] {
        assert!(library(bad, body).is_err(), "{bad}");
    }
}

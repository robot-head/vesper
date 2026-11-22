//! `nocapture in argv: CStrList` (SH6 lane 13, #1408, gap G2): a
//! NULL-terminated `char *const argv[]` built from one byte slice.
use vesper_ffi::sys::*;

fn parse_body(body: &str) -> Result<LibraryDescriptor, Error> {
    parse(
        &format!(
            r#"sys library Fixture (
            target = "x86_64-unknown-linux-gnu", abi = "sysv64",
            link = "dynamic", name = "c",
            artifact = "blake3:{}"
        ) {{ {body} }}"#,
            "0".repeat(64)
        ),
        Limits::default(),
    )
}

#[test]
fn a_cstr_list_is_one_pointer() {
    let lib = parse_body(
        "fn execv(nocapture in path: CStr, nocapture in argv: CStrList) -> ForeignVal[i32] errno;",
    )
    .unwrap();
    let f = lib.function("execv").unwrap();
    assert_eq!(f.parameters()[1].ty(), &ParameterType::CStrList);
    assert_eq!(f.abi().parameters, vec![AbiType::Pointer, AbiType::Pointer]);
    // Only a `CStr` can refuse a call.
    assert!(f.takes_cstr());
    let g = parse_body("fn g(nocapture in argv: CStrList) -> ForeignVal[i32];").unwrap();
    assert!(!g.function("g").unwrap().takes_cstr());
}

#[test]
fn a_cstr_list_is_only_read_and_never_null() {
    for body in [
        "fn f(nocapture inout argv: CStrList) -> ForeignVal[i32];",
        "fn f(nocapture in argv: Option[CStrList]) -> ForeignVal[i32];",
    ] {
        let e = parse_body(body).unwrap_err();
        assert!(e.to_string().contains("CStrList"), "{body}: {e}");
    }
}

#[test]
fn a_cstr_list_has_its_own_identity() {
    let list = parse_body("fn f(nocapture in argv: CStrList) -> ForeignVal[i32];").unwrap();
    let string = parse_body("fn f(nocapture in argv: CStr) -> ForeignVal[i32];").unwrap();
    assert_ne!(list.identity(), string.identity());
}

#[test]
fn the_cstr_list_name_is_reserved() {
    for body in [
        "affine extern type CStrList; fn f() -> ForeignVal[Unit];",
        "enum CStrList: u8 { X = 1 } fn f() -> ForeignVal[Unit];",
        "wire struct CStrList { x: u8 } fn f() -> ForeignVal[Unit];",
    ] {
        let e = parse_body(body).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Type, "{body}: {e}");
        assert!(e.to_string().contains("reserved"), "{body}: {e}");
    }
}

//! Variadic C functions in `.vsys` (`fn f(fixed.., ..., variadic..)`,
//! SH6 lane 13, #1408): the parameters after `...` are the variadic
//! arguments every call passes, each word-sized. Apple arm64 passes them on
//! the stack, so the signature pads the eight integer argument registers.
use vesper_ffi::sys::*;

fn source(triple: &str, abi: &str, body: &str) -> String {
    format!(
        r#"sys library Fixture (
        target = "{triple}", abi = "{abi}",
        link = "dynamic", name = "c",
        artifact = "blake3:{}"
    ) {{ {body} }}"#,
        "0".repeat(64)
    )
}
fn linux(body: &str) -> Result<LibraryDescriptor, Error> {
    parse(
        &source("x86_64-unknown-linux-gnu", "sysv64", body),
        Limits::default(),
    )
}

const FCNTL: &str = "fn fcntl(fd: i32, cmd: i32, ..., arg: i64) -> ForeignVal[i32] errno;";

#[test]
fn variadic_parameters_follow_the_ellipsis() {
    let lib = linux(FCNTL).unwrap();
    let f = lib.function("fcntl").unwrap();
    assert_eq!(f.variadic_from(), Some(2));
    assert_eq!(f.parameters().len(), 3);
    let abi = f.abi();
    assert_eq!(abi.parameters.len(), 3);
    assert_eq!(
        abi.variadic,
        Some(Variadic {
            first: 2,
            padding: 0
        })
    );
}

#[test]
fn every_target_but_apple_passes_variadic_words_like_fixed_ones() {
    for (triple, abi) in [
        ("x86_64-unknown-linux-gnu", "sysv64"),
        ("aarch64-unknown-linux-gnu", "aapcs64"),
        ("x86_64-pc-windows-msvc", "win64"),
    ] {
        let lib = parse(&source(triple, abi, FCNTL), Limits::default()).unwrap();
        let sig = lib.function("fcntl").unwrap().abi();
        assert_eq!(sig.parameters.len(), 3, "{triple}");
        assert_eq!(sig.variadic.unwrap().padding, 0, "{triple}");
    }
}

#[test]
fn apple_arm64_pads_the_argument_registers() {
    let body = "fn open(nocapture in path: CStr, flags: i32, ..., mode: u64) -> ForeignVal[i32] errno;\n\
                fn ioctl(fd: i32, request: u64, ..., nocapture inout arg: CSlice[u8]) -> ForeignVal[i32] errno;";
    let lib = parse(
        &source("aarch64-apple-darwin", "darwin64", body),
        Limits::default(),
    )
    .unwrap();
    let open = lib.function("open").unwrap().abi();
    // path, flags, six zero words in x2-x7, then mode at [sp].
    assert_eq!(open.parameters.len(), 9);
    assert_eq!(
        open.variadic,
        Some(Variadic {
            first: 8,
            padding: 6
        })
    );
    assert!(
        open.parameters[2..8]
            .iter()
            .all(|t| *t == AbiType::Integer(Scalar::U64))
    );
    assert_eq!(open.parameters[8], AbiType::Integer(Scalar::U64));
    // A slice's pointer and length are both variadic words.
    let ioctl = lib.function("ioctl").unwrap().abi();
    assert_eq!(ioctl.parameters.len(), 10);
    assert_eq!(ioctl.variadic.unwrap().first, 8);
}

#[test]
fn variadic_arguments_are_word_sized() {
    let e = linux("fn fcntl(fd: i32, cmd: i32, ..., arg: i32) -> ForeignVal[i32];").unwrap_err();
    assert!(e.to_string().contains("must be word-sized"), "{e}");
    let e = linux("fn f(a: i32, ..., b: bool) -> ForeignVal[i32];").unwrap_err();
    assert!(e.to_string().contains("must be word-sized"), "{e}");
    // Pointers and C strings are words.
    linux("fn f(a: i32, ..., nocapture in s: CStr, nocapture inout n: u32) -> ForeignVal[i32];")
        .unwrap();
}

#[test]
fn the_ellipsis_needs_fixed_and_variadic_parameters() {
    for body in [
        "fn f(..., a: i64) -> ForeignVal[i32];",
        "fn f(a: i32, ...) -> ForeignVal[i32];",
        "fn f(a: i32, ..., ) -> ForeignVal[i32];",
        "fn f(a: i32, ..., b: i64, ..., c: i64) -> ForeignVal[i32];",
        "fn f(a: i32 ...) -> ForeignVal[i32];",
    ] {
        assert!(linux(body).is_err(), "{body}");
    }
}

#[test]
fn variadic_functions_have_their_own_identity() {
    let fixed = linux("fn fcntl(fd: i32, cmd: i32, arg: i64) -> ForeignVal[i32] errno;").unwrap();
    let variadic = linux(FCNTL).unwrap();
    assert_ne!(fixed.identity(), variadic.identity());
    assert_eq!(fixed.function("fcntl").unwrap().variadic_from(), None);
    assert_eq!(fixed.function("fcntl").unwrap().abi().variadic, None);
}

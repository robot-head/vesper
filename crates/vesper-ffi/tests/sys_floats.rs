//! `f32`/`f64` by value in `.vsys` (§16.3, graphics gap 1): float
//! parameters and results are floating-point register carriers, part of a
//! descriptor's identity, and never variadic.
use vesper_ffi::sys::*;

fn source(triple: &str, abi: &str, body: &str) -> String {
    format!(
        r#"sys library Fixture (
        target = "{triple}", abi = "{abi}",
        link = "dynamic", name = "m",
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

#[test]
fn floats_are_float_register_carriers() {
    let lib = linux(
        "fn ldexp(x: f64, exp: i32) -> ForeignVal[f64];\n\
         fn sqrtf(x: f32) -> ForeignVal[f32];\n\
         callback Curve(x: f64) -> ForeignVal[f64];",
    )
    .unwrap();
    let ldexp = lib.function("ldexp").unwrap();
    assert_eq!(
        ldexp.parameters()[0].ty(),
        &ParameterType::Value(ValueType::Float(Float::F64))
    );
    assert_eq!(ldexp.result(), &ValueType::Float(Float::F64));
    let abi = ldexp.abi();
    assert_eq!(
        abi.parameters,
        [AbiType::Float(Float::F64), AbiType::Integer(Scalar::I32)]
    );
    assert_eq!(abi.result, Some(AbiType::Float(Float::F64)));
    assert_eq!(
        lib.function("sqrtf").unwrap().abi().result,
        Some(AbiType::Float(Float::F32))
    );
    assert_eq!(
        lib.function("Curve").unwrap().abi().parameters,
        [AbiType::Float(Float::F64)]
    );
}

#[test]
fn float_widths_are_part_of_the_identity() {
    let a = linux("fn f(x: f64) -> ForeignVal[f64];").unwrap();
    let b = linux("fn f(x: f32) -> ForeignVal[f64];").unwrap();
    let c = linux("fn f(x: u64) -> ForeignVal[u64];").unwrap();
    assert_ne!(a.identity(), b.identity());
    assert_ne!(a.identity(), c.identity());
}

#[test]
fn floats_are_never_variadic() {
    // C promotes a variadic float to double and some targets pass it in a
    // float register, some in an integer one: refused, not guessed.
    let e =
        linux("fn printf(nocapture in format: CStr, ..., x: f64) -> ForeignVal[i32];").unwrap_err();
    assert!(e.to_string().contains("word-sized"), "{e}");
}

#[test]
fn apple_padding_counts_only_integer_registers() {
    // `x` takes `v0`, so the fixed words use one integer register (`fd`)
    // and seven zero words fill `x1`-`x7`.
    let lib = parse(
        &source(
            "aarch64-apple-darwin",
            "darwin64",
            "fn f(x: f64, fd: i32, ..., arg: i64) -> ForeignVal[i32];",
        ),
        Limits::default(),
    )
    .unwrap();
    let abi = lib.function("f").unwrap().abi();
    assert_eq!(
        abi.variadic,
        Some(Variadic {
            first: 9,
            padding: 7
        })
    );
}

#[test]
fn float_names_are_reserved() {
    for body in ["affine extern type f64;", "enum f32: u8 { A = 0 }"] {
        assert!(linux(body).is_err(), "{body}");
    }
}

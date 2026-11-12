use vesper_ffi::sys::*;

fn source(body: &str) -> String {
    format!(
        r#"sys library Fixture (
        target = "x86_64-unknown-linux-gnu", abi = "sysv64",
        link = "static", name = "bridge_fixture",
        artifact = "blake3:{}", header = "bridge/fixture.h",
        pkg_config = "bridge-fixture =1.2.3"
    ) {{ {body} }}"#,
        "0".repeat(64)
    )
}
fn library(body: &str) -> LibraryDescriptor {
    parse(&source(body), Limits::default()).unwrap()
}
fn rejects(body: &str) -> Error {
    parse(&source(body), Limits::default()).unwrap_err()
}
#[test]
fn explicit_targets_and_c_abi_slice_expansion() {
    let body = "fn copy(nocapture in src: CSlice[u8], nocapture inout dst: CSlice[u32], count: usize) -> ForeignVal[u32];";
    for (triple, abi, target) in [
        ("x86_64-unknown-linux-gnu", "sysv64", Target::LinuxX86_64),
        ("aarch64-unknown-linux-gnu", "aapcs64", Target::LinuxAarch64),
        ("aarch64-apple-darwin", "darwin64", Target::DarwinAarch64),
        ("x86_64-pc-windows-msvc", "win64", Target::WindowsX86_64),
    ] {
        let s = source(body)
            .replace("x86_64-unknown-linux-gnu", triple)
            .replace("sysv64", abi);
        let lib = parse(&s, Limits::default()).unwrap();
        assert_eq!(lib.target(), target);
        assert_eq!(target.pointer_bits(), 64);
        assert!(target.little_endian());
        let f = lib.function("copy").unwrap();
        assert_eq!(
            f.abi(),
            AbiSignature {
                convention: target.convention(),
                parameters: vec![
                    AbiType::Pointer,
                    AbiType::Integer(Scalar::Usize),
                    AbiType::Pointer,
                    AbiType::Integer(Scalar::Usize),
                    AbiType::Integer(Scalar::Usize)
                ],
                result: Some(AbiType::Integer(Scalar::U32))
            }
        );
        assert_eq!(lib.link().mode(), LinkMode::Static);
        assert_eq!(lib.link().header(), Some("bridge/fixture.h"));
    }
}
#[test]
fn declaration_order_comments_and_option_order_have_one_identity() {
    let a = source(
        "enum State: u32 { Busy = 9, Ok = 1 } fn b(x: State) -> ForeignVal[State]; fn a() -> ForeignVal[bool];",
    );
    let b=source("// comment\nfn a() -> ForeignVal[bool]; fn b(x: State) -> ForeignVal[State]; enum State: u32 { Ok = 1, Busy = 9 }")
        .replace("target = \"x86_64-unknown-linux-gnu\", abi = \"sysv64\"", "abi = \"sysv64\", target = \"x86_64-unknown-linux-gnu\"");
    let a = parse(&a, Limits::default()).unwrap();
    let b = parse(&b, Limits::default()).unwrap();
    assert_eq!(a.identity(), b.identity());
    assert_eq!(a.canonical_bytes(), b.canonical_bytes());
    assert!(
        a.identity()
            .to_string()
            .starts_with("vesper.vsys/1:blake3:")
    );
}
#[test]
fn identity_covers_every_target_library_and_signature_input() {
    let original = source(
        "enum State: u32 { Ok = 1 } fn value(nocapture in bytes: CSlice[u8]) -> ForeignVal[State];",
    );
    let id = parse(&original, Limits::default()).unwrap().identity();
    for changed in [
        original.replace("Fixture", "Fixture2"),
        original
            .replace("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu")
            .replace("sysv64", "aapcs64"),
        original.replace("\"static\"", "\"dynamic\""),
        original.replace("name = \"bridge_fixture\"", "name = \"other\""),
        original.replace(&"0".repeat(64), &"1".repeat(64)),
        original.replace("fixture.h", "other.h"),
        original.replace("=1.2.3", "^1.2.3"),
        original.replace("Ok = 1", "Ok = 2"),
        original.replace("enum State: u32", "enum State: u64"),
        original.replace("fn value", "fn other"),
        original.replace("in bytes", "in input"),
        original.replace("nocapture in", "nocapture inout"),
        original.replace("CSlice[u8]", "CSlice[u16]"),
        original.replace("ForeignVal[State]", "ForeignVal[u32]"),
    ] {
        assert_ne!(
            id,
            parse(&changed, Limits::default()).unwrap().identity(),
            "{changed}"
        );
    }
}
#[test]
fn unsupported_or_ambiguous_abis_fail_closed() {
    for body in [
        "fn f(x: u32, ...) -> ForeignVal[u32];",
        "fn f(x: c_long) -> ForeignVal[u32];",
        "fn f(x: f64) -> ForeignVal[u32];",
        "fn f(x: CSlice[u8]) -> ForeignVal[u32];",
        "fn f(nocapture sink x: CSlice[u8]) -> ForeignVal[u32];",
        "fn f(nocapture in x: CSlice[bool]) -> ForeignVal[u32];",
        "fn f(x: Ptr[u8]) -> ForeignVal[u32];",
        "fn f(x: Unit) -> ForeignVal[u32];",
        "fn f() -> u32;",
        "fn f() -> ForeignVal[Unknown];",
        "fn f() -> ForeignVal[Option[u32]];",
        "linear extern type Handle; fn f() -> ForeignVal[usize];",
        "struct State { value: u32 } fn f() -> ForeignVal[State];",
        "fn f() -> ForeignVal[u32] { 1 }",
        "enum State: i32 { A = 0 } fn f() -> ForeignVal[State];",
        "enum State: usize { A = 0 } fn f() -> ForeignVal[State];",
        "enum State: u8 { A = 256 } fn f() -> ForeignVal[State];",
        "enum State: u32 {} fn f() -> ForeignVal[State];",
        "fn f() -> ForeignVal[Ptr[Unknown]];",
        "fn f() -> ForeignVal[u64be];",
    ] {
        assert!(!rejects(body).message.is_empty(), "{body}");
    }
}
#[test]
fn duplicate_definitions_and_invalid_link_inputs_are_rejected() {
    for body in [
        "fn f() -> ForeignVal[u8]; fn f() -> ForeignVal[u8];",
        "fn f(x: u8, x: u8) -> ForeignVal[u8];",
        "enum E: u8 { A = 0, A = 1 } fn f() -> ForeignVal[E];",
        "enum E: u8 { A = 0, B = 0 } fn f() -> ForeignVal[E];",
        "enum E: u8 { A = 0 } enum E: u8 { B = 1 } fn f() -> ForeignVal[E];",
    ] {
        assert_eq!(rejects(body).kind, ErrorKind::Duplicate, "{body}");
    }
    let original = source("fn f() -> ForeignVal[u8];");
    for (from, to) in [
        ("abi = \"sysv64\"", "abi = \"win64\""),
        ("x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"),
        ("\"static\"", "\"framework\""),
        ("name = \"bridge_fixture\"", "name = \"-lbad;sh\""),
        ("name = \"bridge_fixture\"", "name = \"-lbad\""),
        ("bridge/fixture.h", "../private.h"),
        ("bridge/fixture.h", "/usr/local/private.h"),
        ("bridge/fixture.h", "bridge//fixture.h"),
        ("bridge-fixture =1.2.3", "bridge-fixture >=1.2.3;evil"),
        ("blake3:", "sha256:"),
        ("header =", "unknown ="),
        ("target =", "abi ="),
    ] {
        assert!(
            parse(&original.replace(from, to), Limits::default()).is_err(),
            "{to}"
        );
    }
    assert!(parse(&(original + " sys library Other {}"), Limits::default()).is_err());
}
#[test]
fn limits_are_checked_before_unbounded_retention() {
    let s = source("fn f(a: u8, b: u8) -> ForeignVal[u8]; fn g() -> ForeignVal[u8];");
    for limits in [
        Limits {
            source_bytes: s.len() - 1,
            ..Limits::default()
        },
        Limits {
            tokens: 5,
            ..Limits::default()
        },
        Limits {
            functions: 1,
            ..Limits::default()
        },
        Limits {
            parameters: 1,
            ..Limits::default()
        },
        Limits {
            string_bytes: 4,
            ..Limits::default()
        },
        Limits {
            canonical_bytes: 8,
            ..Limits::default()
        },
        Limits {
            source_bytes: usize::MAX,
            ..Limits::default()
        },
        Limits {
            tokens: 0,
            ..Limits::default()
        },
    ] {
        assert_eq!(parse(&s, limits).unwrap_err().kind, ErrorKind::Limit);
    }
    assert_eq!(
        parse(
            &source("enum E: u8 { A = 0, B = 1 } fn f() -> ForeignVal[E];"),
            Limits {
                variants: 1,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
    assert_eq!(
        parse(
            &source("enum E: u8 { A = 0 } enum F: u8 { A = 0 } fn f() -> ForeignVal[E];"),
            Limits {
                enums: 1,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
    for text in [
        "\"unterminated",
        "/* unsupported */",
        "fn f() -> ForeignVal[u128];",
        "fn f() -> ForeignVal[Ptr[Ptr[u8]]];",
    ] {
        assert!(parse(text, Limits::default()).is_err());
    }
}
#[test]
fn scalar_returns_never_truncate_and_signed_values_are_explicit_bits() {
    let lib = library(
        "fn i() -> ForeignVal[i8]; fn u() -> ForeignVal[u16]; fn wide() -> ForeignVal[i64]; fn unit() -> ForeignVal[Unit];",
    );
    assert_eq!(
        lib.function("i")
            .unwrap()
            .capture_return(255, ())
            .validate()
            .unwrap()
            .value(),
        &ValidatedValue::Signed(-1)
    );
    assert_eq!(
        lib.function("wide")
            .unwrap()
            .capture_return(1u64 << 63, ())
            .validate()
            .unwrap()
            .value(),
        &ValidatedValue::Signed(i64::MIN)
    );
    assert!(matches!(
        lib.function("i")
            .unwrap()
            .capture_return(256, ())
            .validate()
            .unwrap_err()
            .error(),
        ValidationError::Width { bits: 8 }
    ));
    assert_eq!(
        lib.function("u")
            .unwrap()
            .capture_return(65535, ())
            .validate()
            .unwrap()
            .value(),
        &ValidatedValue::Unsigned(65535)
    );
    assert!(
        lib.function("u")
            .unwrap()
            .capture_return(65536, ())
            .validate()
            .is_err()
    );
    assert!(
        lib.function("unit")
            .unwrap()
            .capture_return(0, ())
            .validate()
            .is_ok()
    );
    assert_eq!(lib.function("unit").unwrap().abi().result, None);
    assert_eq!(
        lib.function("unit")
            .unwrap()
            .capture_return(1, ())
            .validate()
            .unwrap_err()
            .error(),
        &ValidationError::NonzeroVoid
    );
}
#[test]
fn bool_sparse_enum_and_pointer_validity_are_refinements_not_references() {
    let lib = library(
        "enum State: u32 { Ok = 0, Busy = 7 } fn b() -> ForeignVal[bool]; fn e() -> ForeignVal[State]; fn p() -> ForeignVal[Ptr[u32]]; fn n() -> ForeignVal[NullablePtr[u64]];",
    );
    let b = lib.function("b").unwrap();
    assert_eq!(b.abi().result, Some(AbiType::Integer(Scalar::U8)));
    assert_eq!(
        b.capture_return(1, ()).validate().unwrap().value(),
        &ValidatedValue::Bool(true)
    );
    assert_eq!(
        b.capture_return(2, ()).validate().unwrap_err().error(),
        &ValidationError::InvalidBool
    );
    let e = lib.function("e").unwrap();
    assert!(e.capture_return(7, ()).validate().is_ok());
    assert!(e.capture_return(1, ()).validate().is_err());
    let p = lib.function("p").unwrap();
    assert_eq!(
        p.capture_return(0, ()).validate().unwrap_err().error(),
        &ValidationError::NullAddress
    );
    assert_eq!(
        p.capture_return(3, ()).validate().unwrap_err().error(),
        &ValidationError::MisalignedAddress { alignment: 4 }
    );
    match p.capture_return(4096, ()).validate().unwrap().value() {
        ValidatedValue::Address(a) => assert_eq!(a.address(), 4096),
        _ => panic!("address expected"),
    }
    assert!(
        lib.function("n")
            .unwrap()
            .capture_return(0, ())
            .validate()
            .is_ok()
    );
}
#[test]
fn failures_preserve_nonclone_owners_and_raw_return_for_recovery() {
    #[derive(Debug)]
    struct Owner(String);
    let lib = library("fn b() -> ForeignVal[bool];");
    let raw = lib
        .function("b")
        .unwrap()
        .capture_return(3, Owner("owned memory".into()));
    let (error, raw) = raw.validate().unwrap_err().into_parts();
    assert_eq!(error, ValidationError::InvalidBool);
    assert_eq!(raw.raw_bits(), 3);
    let (bits, owner) = raw.into_parts();
    assert_eq!(bits, 3);
    assert_eq!(owner.0, "owned memory");
}
#[test]
fn library_error_sentinels_keep_owners() {
    let lib = library("fn f() -> ForeignVal[u32];");
    let f = lib.function("f").unwrap();
    let failure = f
        .capture_return(9, vec![1, 2])
        .validate_non_error(&[7, 9])
        .unwrap_err();
    let (error, raw) = failure.into_parts();
    assert_eq!(error, ValidationError::ForeignErrorCode(9));
    assert_eq!(raw.into_parts(), (9, vec![1, 2]));
    assert!(f.capture_return(3, ()).validate_non_error(&[7, 9]).is_ok());
    assert_eq!(
        f.capture_return(3, ())
            .validate_non_error(&[0; 257])
            .unwrap_err()
            .error(),
        &ValidationError::ErrorCodeBudget
    );
}
#[test]
fn call_preparation_checks_exact_abi_arguments_bounds_aliases_and_owners() {
    let lib = library(
        "fn copy(nocapture in src: CSlice[u32], nocapture inout dst: CSlice[u32], flag: bool) -> ForeignVal[usize];",
    );
    let f = lib.function("copy").unwrap();
    let args = vec![
        RawArgument::Slice {
            address: 4096,
            length: 4,
        },
        RawArgument::Slice {
            address: 8192,
            length: 4,
        },
        RawArgument::Bits(1),
    ];
    let call = f.prepare_call(args, vec![0xaa]).unwrap();
    assert_eq!(
        call.arguments(),
        &[
            (AbiType::Pointer, 4096),
            (AbiType::Integer(Scalar::Usize), 4),
            (AbiType::Pointer, 8192),
            (AbiType::Integer(Scalar::Usize), 4),
            (AbiType::Integer(Scalar::U8), 1)
        ]
    );
    assert_eq!(call.into_parts().1, vec![0xaa]);
    for (src, dst, flag, error) in [
        (
            RawArgument::Slice {
                address: 4096,
                length: 4,
            },
            RawArgument::Slice {
                address: 4100,
                length: 2,
            },
            1,
            ValidationError::OverlappingSlices { other: 0 },
        ),
        (
            RawArgument::Slice {
                address: 0,
                length: 4,
            },
            RawArgument::Slice {
                address: 8192,
                length: 4,
            },
            1,
            ValidationError::NullAddress,
        ),
        (
            RawArgument::Slice {
                address: 4097,
                length: 4,
            },
            RawArgument::Slice {
                address: 8192,
                length: 4,
            },
            1,
            ValidationError::MisalignedAddress { alignment: 4 },
        ),
        (
            RawArgument::Slice {
                address: 4096,
                length: u64::MAX,
            },
            RawArgument::Slice {
                address: 8192,
                length: 4,
            },
            1,
            ValidationError::SliceRangeOverflow,
        ),
        (
            RawArgument::Slice {
                address: u64::MAX - 3,
                length: 2,
            },
            RawArgument::Slice {
                address: 8192,
                length: 4,
            },
            1,
            ValidationError::SliceRangeOverflow,
        ),
        (
            RawArgument::Slice {
                address: 4096,
                length: (i64::MAX as u64) / 4 + 1,
            },
            RawArgument::Slice {
                address: 8192,
                length: 4,
            },
            1,
            ValidationError::SliceTooLarge,
        ),
        (
            RawArgument::Slice {
                address: 4096,
                length: 4,
            },
            RawArgument::Slice {
                address: 8192,
                length: 4,
            },
            2,
            ValidationError::InvalidBool,
        ),
        (
            RawArgument::Bits(1),
            RawArgument::Slice {
                address: 8192,
                length: 4,
            },
            1,
            ValidationError::ArgumentKind,
        ),
    ] {
        let args = vec![src, dst, RawArgument::Bits(flag)];
        let failure = f
            .prepare_call(args.clone(), String::from("buffer owner"))
            .unwrap_err();
        let (got, _, retained, owner) = failure.into_parts();
        assert_eq!(got, error);
        assert_eq!(retained, args);
        assert_eq!(owner, "buffer owner");
    }
    assert_eq!(
        f.prepare_call(vec![], ()).unwrap_err().error(),
        &ValidationError::ArgumentCount
    );
    assert!(
        f.prepare_call(
            vec![
                RawArgument::Slice {
                    address: 0,
                    length: 0
                },
                RawArgument::Slice {
                    address: 0,
                    length: 0
                },
                RawArgument::Bits(0)
            ],
            ()
        )
        .is_ok()
    );
}
#[test]
fn shared_overlap_is_allowed_and_adjacent_unique_ranges_are_disjoint() {
    let lib = library(
        "fn shared(nocapture in a: CSlice[u8], nocapture in b: CSlice[u8]) -> ForeignVal[Unit]; fn unique(nocapture inout a: CSlice[u8], nocapture inout b: CSlice[u8]) -> ForeignVal[Unit];",
    );
    let same = vec![
        RawArgument::Slice {
            address: 1024,
            length: 8,
        },
        RawArgument::Slice {
            address: 1024,
            length: 8,
        },
    ];
    assert!(
        lib.function("shared")
            .unwrap()
            .prepare_call(same.clone(), ())
            .is_ok()
    );
    assert!(
        lib.function("unique")
            .unwrap()
            .prepare_call(same, ())
            .is_err()
    );
    assert!(
        lib.function("unique")
            .unwrap()
            .prepare_call(
                vec![
                    RawArgument::Slice {
                        address: 1024,
                        length: 8
                    },
                    RawArgument::Slice {
                        address: 1032,
                        length: 8
                    }
                ],
                ()
            )
            .is_ok()
    );
}

#[test]
fn every_truncated_manifest_is_rejected_without_panics_or_invalid_spans() {
    let full =
        source("enum E: u8 { A = 1 } fn f(nocapture in input: CSlice[u32]) -> ForeignVal[E];");
    let closing = full.rfind('}').unwrap();
    for end in 0..=closing {
        let error = parse(&full[..end], Limits::default()).unwrap_err();
        assert!(
            error.span.start <= error.span.end && error.span.end <= end,
            "{error:?}"
        );
    }
    assert!(parse(&full, Limits::default()).is_ok());
    for invalid in [
        source("fn f() -> ForeignVal[u8];").replace("Fixture", &"a".repeat(129)),
        source("fn f() -> ForeignVal[u8];").replace("bridge/fixture.h", "bridge/\\q.h"),
        source("fn f() -> ForeignVal[u8];").replace("Fixture", "Fíxture"),
        source("enum E: u64 { A = 18446744073709551616 } fn f() -> ForeignVal[E];"),
    ] {
        assert!(parse(&invalid, Limits::default()).is_err());
    }
}

#[test]
fn repeated_maximum_enums_share_one_variant_table_even_after_capture_and_clone() {
    let mut body = String::from("enum Big: u32 {");
    for variant in 0..256 {
        body.push_str(&format!(" V{variant} = {variant},"));
    }
    body.push_str(" }");
    // Over six thousand occurrences previously cloned the complete 256-variant
    // table before the canonical-output budget could reject the expansion.
    for function in 0..192 {
        body.push_str(&format!(" fn f{function}("));
        for parameter in 0..32 {
            body.push_str(&format!("p{parameter}: Big,"));
        }
        body.push_str(") -> ForeignVal[Big];");
    }
    let library = library(&body);
    let backing = library.enums()[0].variants().as_ptr();
    let mut occurrences = 0;
    for function in library.functions() {
        for parameter in function.parameters() {
            let ParameterType::Value(ValueType::Enum(enumeration)) = parameter.ty() else {
                panic!("enum parameter expected")
            };
            assert_eq!(enumeration.variants().as_ptr(), backing);
            occurrences += 1;
        }
        let ValueType::Enum(enumeration) = function.result() else {
            panic!("enum return expected")
        };
        assert_eq!(enumeration.variants().as_ptr(), backing);
        let captured = function.capture_return(255, ());
        assert!(captured.validate().is_ok());
    }
    assert_eq!(occurrences, 6144);
    let cloned = library.clone();
    assert_eq!(cloned.enums()[0].variants().as_ptr(), backing);
    let ParameterType::Value(ValueType::Enum(enumeration)) =
        cloned.functions()[0].parameters()[0].ty()
    else {
        panic!("enum parameter expected")
    };
    assert_eq!(enumeration.variants().as_ptr(), backing);
}

#[test]
fn host_detection_implies_the_complete_descriptor_data_model() {
    for target in [
        Target::LinuxX86_64,
        Target::LinuxAarch64,
        Target::DarwinAarch64,
        Target::WindowsX86_64,
    ] {
        if target.is_host() {
            assert_eq!(u32::from(target.pointer_bits()), usize::BITS);
            assert_eq!(target.little_endian(), cfg!(target_endian = "little"));
        }
    }
    #[cfg(any(not(target_pointer_width = "64"), not(target_endian = "little")))]
    assert!(
        ![
            Target::LinuxX86_64,
            Target::LinuxAarch64,
            Target::DarwinAarch64,
            Target::WindowsX86_64
        ]
        .into_iter()
        .any(Target::is_host)
    );
}

//! Single-element `nocapture` pointer parameters and `wire struct` pointees
//! (design §13.2 and the §29.3 `legacy_parse_batch_header` contract).
use vesper_ffi::sys::*;

fn source(body: &str) -> String {
    format!(
        r#"sys library Fixture (
        target = "x86_64-unknown-linux-gnu", abi = "sysv64",
        link = "static", name = "bridge_fixture",
        artifact = "blake3:{}"
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
fn pointer(f: &FunctionDescriptor, index: usize) -> PointerLayout {
    f.parameters()[index]
        .ty()
        .pointer_layout()
        .expect("pointer parameter")
}

const HEADER: &str = "wire struct Header {
    base_offset: i64be,
    batch_length: i32be,
    magic: u8,
    crc: u32be,
    records: i32be,
}";

#[test]
fn crc32_update_and_design_parse_batch_header_forms() {
    let lib = library(
        "fn crc32_update(nocapture inout crc: u32, nocapture in data: CSlice[u8]) -> ForeignVal[Unit];
         fn legacy_parse_batch_header(
             nocapture in raw_bytes: CSlice[u8],
             nocapture inout out_crc: u32,
             nocapture inout out_count: i32,
         ) -> ForeignVal[i32];",
    );
    let crc = lib.function("crc32_update").unwrap();
    assert_eq!(
        crc.parameters()[0].ty(),
        &ParameterType::Pointer {
            pointee: Pointee::Scalar(Scalar::U32),
            access: PointerAccess::Inout,
            nullable: false,
        }
    );
    assert_eq!(
        pointer(crc, 0),
        PointerLayout {
            size: 4,
            alignment: 4,
            access: PointerAccess::Inout,
            nullable: false
        }
    );
    // One C pointer, then the slice's pointer and element count.
    assert_eq!(
        crc.abi().parameters,
        vec![
            AbiType::Pointer,
            AbiType::Pointer,
            AbiType::Integer(Scalar::Usize)
        ]
    );
    assert_eq!(crc.abi().result, None);
    let parse = lib.function("legacy_parse_batch_header").unwrap();
    assert_eq!(
        parse.abi().parameters,
        vec![
            AbiType::Pointer,
            AbiType::Integer(Scalar::Usize),
            AbiType::Pointer,
            AbiType::Pointer
        ]
    );
    assert_eq!(pointer(parse, 2).size, 4);
    assert!(parse.parameters()[2].ty().pointer_layout().unwrap().access == PointerAccess::Inout);
    assert!(parse.parameters()[0].ty().pointer_layout().is_none());
}

#[test]
fn every_scalar_direction_and_struct_pointee_has_a_typed_layout() {
    let lib = library(&format!(
        "{HEADER}
         wire struct Pair {{ lo: u32, hi: u32 }}
         wire struct Nested {{ tag: u64, pair: Pair, pad: ZeroPad[4], bytes: ByteArray[4] }}
         fn f(
             nocapture in key: u64,
             nocapture out hdr: Header,
             nocapture inout pair: Pair,
             nocapture out maybe: Option[u16],
             nocapture in nested: Option[Nested],
             nocapture in byte: i8,
         ) -> ForeignVal[u32];"
    ));
    let f = lib.function("f").unwrap();
    let expect = [
        (8, 8, PointerAccess::In, false),
        (21, 1, PointerAccess::Out, false),
        (8, 4, PointerAccess::Inout, false),
        (2, 2, PointerAccess::Out, true),
        (24, 8, PointerAccess::In, true),
        (1, 1, PointerAccess::In, false),
    ];
    for (i, (size, alignment, access, nullable)) in expect.into_iter().enumerate() {
        assert_eq!(
            pointer(f, i),
            PointerLayout {
                size,
                alignment,
                access,
                nullable
            },
            "parameter {i}"
        );
    }
    assert_eq!(f.abi().parameters, vec![AbiType::Pointer; 6]);
    let header = lib.wire_struct("Header").unwrap();
    assert_eq!(header.layout().size(), 21);
    assert_eq!(header.layout().field("crc").unwrap().offset(), 13);
    assert_eq!(
        lib.wire_structs()
            .iter()
            .map(WireStructDescriptor::name)
            .collect::<Vec<_>>(),
        ["Header", "Nested", "Pair"]
    );
    assert!(PointerAccess::Out.writes() && PointerAccess::Inout.writes());
    assert!(!PointerAccess::In.writes());
}

#[test]
fn old_forms_keep_their_exact_canonical_identity() {
    // Pinned from main before single pointers existed (baseline 07322e3):
    // a descriptor using only the earlier forms must keep its identity.
    let lib = library(
        "enum State: u32 { Ok = 0, Full = 7 }
         fn encode(nocapture in src: CSlice[u8], nocapture inout dst: CSlice[u8], flags: u32) -> ForeignVal[State];
         fn available() -> ForeignVal[bool];
         fn address() -> ForeignVal[NullablePtr[u32]];",
    );
    assert_eq!(
        lib.identity().to_string(),
        "vesper.vsys/1:blake3:dfbc2d8024d5f0aedc5122826d98a3304c80161b30f2512d3e4002feb334cab1"
    );
    assert!(lib.wire_structs().is_empty());
}

#[test]
fn identity_covers_pointer_direction_nullability_pointee_and_struct_fields() {
    let base = format!(
        "{HEADER} fn f(nocapture out hdr: Header, nocapture inout n: u32) -> ForeignVal[Unit];"
    );
    let id = library(&base).identity();
    let variants = [
        base.replace("out hdr", "inout hdr"),
        base.replace("out hdr: Header", "out hdr: Option[Header]"),
        base.replace("inout n: u32", "inout n: i32"),
        base.replace("inout n", "in n"),
        base.replace("crc: u32be", "crc: u32le"),
        base.replace("records: i32be", "records: i32le"),
        base.replace("magic: u8,", "magic: i8,"),
        base.replace("records: i32be,", "records: i32be, extra: ByteArray[3],"),
        base.replace("base_offset", "first_offset"),
    ];
    for v in variants {
        assert_ne!(library(&v).identity(), id, "{v}");
    }
    // Declaration order of wire structs does not matter.
    let reordered = format!(
        "fn f(nocapture out hdr: Header, nocapture inout n: u32) -> ForeignVal[Unit]; {HEADER}"
    );
    assert_eq!(library(&reordered).identity(), id);
}

#[test]
fn omitted_nocapture_nullable_without_option_and_pointer_to_pointer_are_typed_errors() {
    for (body, kind, needle) in [
        (
            "fn f(inout out_crc: u32) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "nocapture",
        ),
        (
            "fn f(out hdr: u64) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "nocapture",
        ),
        (
            "fn f(in src: CSlice[u8]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "nocapture",
        ),
        (
            "fn f(nocapture inout p: NullablePtr[u32]) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "Option[T]",
        ),
        (
            "fn f(nocapture out p: Ptr[u8]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "pointer-to-pointer",
        ),
        (
            "fn f(nocapture out p: Option[Ptr[u8]]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "pointer-to-pointer",
        ),
        (
            "fn f(nocapture in p: Option[NullablePtr[u8]]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "pointer-to-pointer",
        ),
        (
            "fn f(p: Ptr[u32]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "raw-pointer",
        ),
        (
            "fn f(nocapture p: u32) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "in, out or inout",
        ),
        (
            "fn f(nocapture out p: CSlice[u8]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "out slice",
        ),
        (
            "fn f(nocapture in p: Option[Option[u8]]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "nested",
        ),
        (
            "fn f(nocapture out p: bool) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "every bit pattern",
        ),
        (
            "enum E: u8 { A = 1 } fn f(nocapture out p: E) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "every bit pattern",
        ),
        (
            "fn f(nocapture out p: Unit) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "void",
        ),
        (
            "fn f(nocapture out p: Missing) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "unknown pointee",
        ),
        (
            "fn f(nocapture out p: Option[u32) -> ForeignVal[Unit];",
            ErrorKind::Syntax,
            "expected ']'",
        ),
    ] {
        let e = rejects(body);
        assert_eq!(e.kind, kind, "{body}: {e}");
        assert!(e.message.contains(needle), "{body}: {e}");
        assert!(e.span.end <= source(body).len());
    }
}

#[test]
fn a_parameter_named_like_an_access_keyword_is_still_a_value() {
    let lib = library("fn f(in: u32, out: u8, inout: u16) -> ForeignVal[Unit];");
    let f = lib.function("f").unwrap();
    assert!(
        f.parameters()
            .iter()
            .all(|p| matches!(p.ty(), ParameterType::Value(_)))
    );
    assert_eq!(f.parameters()[1].name(), "out");
}

#[test]
fn wire_struct_pointees_reject_padding_partial_validity_and_recursion() {
    for (body, kind, needle) in [
        (
            "wire struct P { tag: u8, value: u32 } fn f(nocapture out p: P) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "ZeroPad[3]",
        ),
        (
            "wire struct P { value: u32, tag: u8 } fn f(nocapture out p: P) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "ZeroPad[3]",
        ),
        (
            "wire struct P { flag: bool } fn f(nocapture out p: P) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "every bit pattern",
        ),
        (
            "enum E: u8 { A = 1 } wire struct P { e: E } fn f(nocapture out p: P) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "sparse enums",
        ),
        (
            "wire struct P { n: usize } fn f(nocapture out p: P) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "usize",
        ),
        (
            "wire struct P { n: u8be } fn f(nocapture out p: P) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "byte order",
        ),
        (
            "wire struct A { b: B } wire struct B { a: A } fn f(nocapture out p: A) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "RecursiveStruct",
        ),
        (
            "wire struct A { b: Missing } fn f(nocapture out p: A) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "UnknownStruct",
        ),
        (
            "wire struct A { x: u8 } wire struct A { y: u8 } fn f() -> ForeignVal[Unit];",
            ErrorKind::Duplicate,
            "duplicate wire struct",
        ),
        (
            "wire struct A { x: u8, x: u8 } fn f() -> ForeignVal[Unit];",
            ErrorKind::Duplicate,
            "duplicate wire struct field",
        ),
        (
            "enum A: u8 { X = 1 } wire struct A { x: u8 } fn f() -> ForeignVal[Unit];",
            ErrorKind::Duplicate,
            "collides",
        ),
        (
            "wire struct A { } fn f() -> ForeignVal[Unit];",
            ErrorKind::Type,
            "empty",
        ),
        (
            "wire struct u32 { x: u8 } fn f() -> ForeignVal[Unit];",
            ErrorKind::Type,
            "reserved",
        ),
        (
            "wire struct A { x: ByteArray[0] } fn f() -> ForeignVal[Unit];",
            ErrorKind::Limit,
            "byte array",
        ),
        (
            "wire struct A { x: ByteArray[1048577] } fn f() -> ForeignVal[Unit];",
            ErrorKind::Limit,
            "byte array",
        ),
        (
            "wire struct A { x: ByteArray[1048576], y: u8 } fn f() -> ForeignVal[Unit];",
            ErrorKind::Limit,
            "exceeds",
        ),
    ] {
        let e = rejects(body);
        assert_eq!(e.kind, kind, "{body}: {e}");
        assert!(e.message.contains(needle), "{body}: {e}");
    }
}

#[test]
fn wire_struct_and_field_budgets_are_enforced() {
    let limits = Limits {
        wire_structs: 1,
        ..Limits::default()
    };
    let two = source("wire struct A { x: u8 } wire struct B { x: u8 } fn f() -> ForeignVal[Unit];");
    assert_eq!(parse(&two, limits).unwrap_err().kind, ErrorKind::Limit);
    let limits = Limits {
        wire_fields: 2,
        ..Limits::default()
    };
    let three = source("wire struct A { x: u8, y: u8, z: u8 } fn f() -> ForeignVal[Unit];");
    assert_eq!(parse(&three, limits).unwrap_err().kind, ErrorKind::Limit);
    for bad in [
        Limits {
            wire_structs: 0,
            ..Limits::default()
        },
        Limits {
            wire_fields: 257,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            parse(&source("fn f() -> ForeignVal[Unit];"), bad)
                .unwrap_err()
                .kind,
            ErrorKind::Limit
        );
    }
}

#[test]
fn call_preparation_checks_pointer_nullability_alignment_range_and_aliasing() {
    let lib = library(&format!(
        "{HEADER}
         fn f(
             nocapture in raw: CSlice[u8],
             nocapture inout crc: u32,
             nocapture out hdr: Option[Header],
             nocapture in key: u64,
         ) -> ForeignVal[i32];"
    ));
    let f = lib.function("f").unwrap();
    let args = |raw: u64, raw_len: u64, crc: u64, hdr: u64, key: u64| {
        vec![
            RawArgument::Slice {
                address: raw,
                length: raw_len,
            },
            RawArgument::Pointer { address: crc },
            RawArgument::Pointer { address: hdr },
            RawArgument::Pointer { address: key },
        ]
    };
    let prepared = f
        .prepare_call(args(0x1000, 64, 0x2000, 0x3000, 0x4000), "owner")
        .unwrap();
    assert_eq!(
        prepared.arguments(),
        &[
            (AbiType::Pointer, 0x1000),
            (AbiType::Integer(Scalar::Usize), 64),
            (AbiType::Pointer, 0x2000),
            (AbiType::Pointer, 0x3000),
            (AbiType::Pointer, 0x4000),
        ]
    );
    assert_eq!(prepared.into_parts().1, "owner");
    // Option[Header] admits null; the other single pointers do not.
    assert!(
        f.prepare_call(args(0x1000, 64, 0x2000, 0, 0x4000), ())
            .is_ok()
    );
    let check = |a: Vec<RawArgument>, arg: usize, error: ValidationError| {
        let failure = f.prepare_call(a.clone(), vec![7u8]).unwrap_err();
        assert_eq!(failure.error(), &error);
        assert_eq!(failure.argument(), Some(arg));
        let (_, _, returned, owner) = failure.into_parts();
        assert_eq!(returned, a);
        assert_eq!(owner, vec![7]);
    };
    check(
        args(0x1000, 64, 0, 0x3000, 0x4000),
        1,
        ValidationError::NullPointer,
    );
    check(
        args(0x1000, 64, 0x2000, 0x3000, 0),
        3,
        ValidationError::NullPointer,
    );
    check(
        args(0x1000, 64, 0x2002, 0x3000, 0x4000),
        1,
        ValidationError::MisalignedPointer { alignment: 4 },
    );
    check(
        args(0x1000, 64, 0x2000, 0x3000, 0x4004),
        3,
        ValidationError::MisalignedPointer { alignment: 8 },
    );
    check(
        args(0x1000, 64, 0x2000, u64::MAX - 3, 0x4000),
        2,
        ValidationError::PointerRangeOverflow,
    );
    // A written pointee inside a shared slice aliases it.
    check(
        args(0x1000, 64, 0x1010, 0x3000, 0x4000),
        1,
        ValidationError::OverlappingSlices { other: 0 },
    );
    // The 21-byte header overlaps the 4-byte crc it starts before.
    check(
        args(0x1000, 64, 0x2000, 0x1FF0, 0x4000),
        2,
        ValidationError::OverlappingSlices { other: 1 },
    );
    // Shared/shared overlap is allowed: `key` may sit inside `raw`.
    assert!(
        f.prepare_call(args(0x1000, 64, 0x2000, 0x3000, 0x1008), ())
            .is_ok()
    );
    // Adjacent ranges are disjoint.
    assert!(
        f.prepare_call(args(0x1000, 64, 0x1040, 0x1044, 0x4000), ())
            .is_ok()
    );
    // Wrong argument kinds and counts.
    let mut wrong = args(0x1000, 64, 0x2000, 0x3000, 0x4000);
    wrong[1] = RawArgument::Bits(0x2000);
    check(wrong, 1, ValidationError::ArgumentKind);
    let mut wrong = args(0x1000, 64, 0x2000, 0x3000, 0x4000);
    wrong[0] = RawArgument::Pointer { address: 0x1000 };
    check(wrong, 0, ValidationError::ArgumentKind);
    assert_eq!(
        f.prepare_call(vec![], ()).unwrap_err().error(),
        &ValidationError::ArgumentCount
    );
}

#[test]
fn truncated_pointer_manifests_never_panic() {
    let full = source(&format!(
        "{HEADER} fn f(nocapture out hdr: Option[Header], nocapture inout n: u32) -> ForeignVal[Unit];"
    ));
    for end in 0..full.len() {
        if let Err(e) = parse(&full[..end], Limits::default()) {
            assert!(
                e.span.start <= e.span.end && e.span.end <= end,
                "{end}: {e}"
            );
        }
    }
    assert!(parse(&full, Limits::default()).is_ok());
}

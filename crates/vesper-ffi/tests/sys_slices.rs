//! `.vsys` slices of wire structs, `Option` slices, and the per-assumption
//! report (parsing, identity, typed rejections, numeric checks).
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
const REC: &str = "wire struct Rec { id: u32, flags: u32, value: i64 }";

#[test]
fn struct_and_option_slices_parse_with_their_layouts() {
    let lib = library(&format!(
        "{REC}
         fn a(nocapture in r: CSlice[Rec]) -> ForeignVal[Unit];
         fn b(nocapture inout r: Option[CSlice[Rec]]) -> ForeignVal[Unit];
         fn c(nocapture in d: Option[CSlice[u8]], n: u32) -> ForeignVal[u32];"
    ));
    let rec = lib.wire_struct("Rec").unwrap().clone();
    let a = lib.function("a").unwrap();
    assert_eq!(
        a.parameters()[0].ty(),
        &ParameterType::Slice {
            element: Pointee::Struct(rec.clone()),
            access: Access::In,
            nullable: false
        }
    );
    assert_eq!(
        lib.function("b").unwrap().parameters()[0]
            .ty()
            .slice_layout(),
        Some(SliceLayout {
            element_size: 16,
            alignment: 8,
            access: Access::Inout,
            nullable: true
        })
    );
    let c = lib.function("c").unwrap();
    assert_eq!(
        c.parameters()[0].ty(),
        &ParameterType::Slice {
            element: Pointee::Scalar(Scalar::U8),
            access: Access::In,
            nullable: true
        }
    );
    // Every slice form lowers to (pointer, size_t).
    assert_eq!(
        c.abi().parameters,
        vec![
            AbiType::Pointer,
            AbiType::Integer(Scalar::Usize),
            AbiType::Integer(Scalar::U32)
        ]
    );
}

#[test]
fn option_and_struct_slices_have_distinct_identities() {
    let id = |p: &str| {
        library(&format!(
            "{REC} wire struct Two {{ a: u32, b: u32 }} fn f(nocapture in x: {p}) -> ForeignVal[Unit];"
        ))
        .identity()
    };
    let forms = [
        id("CSlice[u32]"),
        id("Option[CSlice[u32]]"),
        id("CSlice[Rec]"),
        id("Option[CSlice[Rec]]"),
        id("CSlice[Two]"),
        id("u32"),
        id("Option[u32]"),
    ];
    for (i, a) in forms.iter().enumerate() {
        for b in &forms[i + 1..] {
            assert_ne!(a, b);
        }
    }
    // The old scalar slice keeps its encoding: a library without wire
    // structs is identical whether or not this lane's code exists.
    let plain = library("fn f(nocapture in x: CSlice[u32]) -> ForeignVal[Unit];");
    let again = library("fn f(nocapture in x: CSlice[u32]) -> ForeignVal[Unit];");
    assert_eq!(plain.identity(), again.identity());
    assert!(
        plain
            .canonical_bytes()
            .windows(b"nocapture-slice/2".len())
            .all(|w| w != b"nocapture-slice/2")
    );
}

#[test]
fn unsupported_slice_forms_are_typed_errors() {
    for (body, kind, needle) in [
        (
            "fn f(nocapture out x: Option[CSlice[u8]]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "out slice",
        ),
        (
            "fn f(nocapture in x: CSlice[Option[u8]]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "slice elements are values",
        ),
        (
            "fn f(nocapture in x: CSlice[Ptr[u8]]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "slice elements are values",
        ),
        (
            "fn f(nocapture in x: CSlice[CSlice[u8]]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "nested",
        ),
        (
            "fn f(nocapture in x: Option[Option[CSlice[u8]]]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "nested",
        ),
        (
            "fn f(nocapture inout x: CSlice[bool]) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "every bit pattern",
        ),
        (
            "enum E: u8 { A = 1 } fn f(nocapture in x: CSlice[E]) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "every bit pattern",
        ),
        (
            "fn f(nocapture in x: CSlice[Missing]) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "unknown slice element",
        ),
        (
            "fn f(nocapture in x: CSlice[Unit]) -> ForeignVal[Unit];",
            ErrorKind::Type,
            "void",
        ),
        (
            "fn f(in x: Option[CSlice[u8]]) -> ForeignVal[Unit];",
            ErrorKind::Unsupported,
            "nocapture",
        ),
        (
            "fn f(nocapture in x: Option[CSlice[u8]) -> ForeignVal[Unit];",
            ErrorKind::Syntax,
            "",
        ),
    ] {
        let e = rejects(body);
        assert_eq!(e.kind, kind, "{body}: {e:?}");
        assert!(e.message.contains(needle), "{body}: {e:?}");
    }
}

#[test]
fn every_assumption_is_listed_with_when_it_can_be_checked() {
    let lib = library(&format!(
        "{REC} fn f(nocapture in r: CSlice[Rec], nocapture out o: Option[u32], n: u32) -> ForeignVal[Ptr[u32]];
         fn g(nocapture inout c: u32) -> ForeignVal[i32];"
    ));
    let f = lib.function("f").unwrap();
    let listed: Vec<(Position, Assumption, Checkability)> = f
        .assumptions()
        .into_iter()
        .map(|a| (a.position, a.assumption, a.checkability))
        .collect();
    use Assumption::*;
    use Checkability::*;
    let arg = Position::Argument;
    for expected in [
        (Position::Call, ArgumentShape, BeforeCall),
        (arg(0), NonNull, BeforeCall),
        (arg(0), Disjoint, BeforeCall),
        (arg(0), WithinLength, WithGuard),
        (arg(0), NoCaptureViaReturn, AfterReturn),
        (arg(0), NoRetention, Unverifiable),
        (arg(1), SingleElement, WithGuard),
        (arg(2), ValidBits, BeforeCall),
        (Position::Return, NonNull, AfterReturn),
        (Position::Return, Aligned, AfterReturn),
    ] {
        assert!(listed.contains(&expected), "{expected:?} in {listed:?}");
    }
    // No pointer is returned, so return containment is not listed.
    let g = lib.function("g").unwrap();
    assert!(
        g.assumptions()
            .iter()
            .all(|a| a.assumption != NoCaptureViaReturn)
    );
}

#[test]
fn reports_name_every_violation_and_prepare_call_reports_the_first() {
    let lib = library(&format!(
        "{REC} fn f(nocapture in r: CSlice[Rec], nocapture inout o: Option[u32], n: u8) -> ForeignVal[Unit];"
    ));
    let f = lib.function("f").unwrap();
    // Three independent problems: a misaligned struct slice, a misaligned
    // pointer and an out-of-range u8. The report names all three.
    let args = vec![
        RawArgument::Slice {
            address: 0x1004,
            length: 1,
        },
        RawArgument::Pointer { address: 0x2001 },
        RawArgument::Bits(256),
    ];
    let report = f.check_arguments(&args);
    let violations: Vec<_> = report
        .violations()
        .map(|c| (c.position, c.assumption))
        .collect();
    assert_eq!(
        violations,
        vec![
            (Position::Argument(0), Assumption::Aligned),
            (Position::Argument(1), Assumption::Aligned),
            (Position::Argument(2), Assumption::ValidBits),
        ]
    );
    let failure = f.prepare_call(args, ()).unwrap_err();
    assert_eq!(failure.argument(), Some(0));
    assert_eq!(
        failure.error(),
        &ValidationError::MisalignedAddress { alignment: 8 }
    );
    // Overlap of a written pointer with a struct slice element.
    let args = vec![
        RawArgument::Slice {
            address: 0x1000,
            length: 2,
        },
        RawArgument::Pointer { address: 0x1010 },
        RawArgument::Bits(1),
    ];
    let report = f.check_arguments(&args);
    assert_eq!(
        report.outcome(Position::Argument(1), Assumption::Disjoint),
        Some(&Outcome::Violated(ValidationError::OverlappingSlices {
            other: 0
        }))
    );
    // Adjacent is disjoint; a null Option pointer is not applicable.
    for o in [0x1020, 0] {
        let report = f.check_arguments(&[
            RawArgument::Slice {
                address: 0x1000,
                length: 2,
            },
            RawArgument::Pointer { address: o },
            RawArgument::Bits(1),
        ]);
        assert!(report.holds(), "{report:?}");
    }
    // Shape problems.
    assert_eq!(
        f.check_arguments(&[]).violations().next().unwrap().outcome,
        Outcome::Violated(ValidationError::ArgumentCount)
    );
    let report = f.check_arguments(&[
        RawArgument::Bits(0),
        RawArgument::Pointer { address: 0 },
        RawArgument::Bits(0),
    ]);
    assert_eq!(
        report.outcome(Position::Argument(0), Assumption::ArgumentShape),
        Some(&Outcome::Violated(ValidationError::ArgumentKind))
    );
}

#[test]
fn guard_observations_are_bounded_and_matched_to_borrows() {
    let lib = library("fn f(nocapture out o: u32, v: u32) -> ForeignVal[Unit];");
    let f = lib.function("f").unwrap();
    let args = [
        RawArgument::Pointer { address: 0x1000 },
        RawArgument::Bits(0),
    ];
    let guard = |argument, before: Vec<u8>, after: Vec<u8>| GuardObservation {
        argument,
        before,
        after,
    };
    let single = |guards: &[GuardObservation]| {
        f.check_boundary(&args, 0, guards)
            .outcome(Position::Argument(0), Assumption::SingleElement)
            .cloned()
    };
    assert_eq!(
        single(&[guard(0, vec![1, 2], vec![1, 2])]),
        Some(Outcome::Held)
    );
    assert_eq!(
        single(&[guard(0, vec![1, 2], vec![1, 3])]),
        Some(Outcome::Violated(ValidationError::GuardOverwritten {
            first_changed: 1
        }))
    );
    for bad in [
        vec![guard(0, vec![], vec![])],
        vec![guard(0, vec![1], vec![1, 2])],
        vec![guard(
            0,
            vec![0; MAX_GUARD_BYTES + 1],
            vec![0; MAX_GUARD_BYTES + 1],
        )],
        vec![guard(0, vec![1], vec![1]), guard(0, vec![1], vec![1])],
    ] {
        assert!(matches!(single(&bad), Some(Outcome::Unchecked(_))));
    }
    // A guard for a value argument is reported, not silently applied.
    let report = f.check_boundary(&args, 0, &[guard(1, vec![1], vec![2])]);
    assert!(report.holds());
    assert!(report.unchecked().any(|c| c.position == Position::Call));
    // A nonzero void return is a violated return assumption.
    let report = f.check_boundary(&args, 5, &[]);
    assert_eq!(
        report.outcome(Position::Return, Assumption::ValidBits),
        Some(&Outcome::Violated(ValidationError::NonzeroVoid))
    );
}

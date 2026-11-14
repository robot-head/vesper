//! Real C evidence for wire-struct slices, `Option` slices and pointers, C
//! layout probes, and per-assumption boundary checks. The C library and its C
//! host are compiled here with the host C compiler; the host prints the raw
//! arguments, returns and guard bytes of each call, and this test checks them
//! with the descriptor. This crate's test code stays safe Rust.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_ffi::sys::*;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-bridge-rest-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}
fn run(command: &mut Command) -> Output {
    let output = command.output().expect("run native fixture tool");
    assert!(
        output.status.success(),
        "command {command:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
/// The descriptor target of this host, when the host is one of them and has
/// a C compiler.
fn host() -> Option<(&'static str, &'static str)> {
    let target = [
        Target::LinuxX86_64,
        Target::LinuxAarch64,
        Target::DarwinAarch64,
    ]
    .into_iter()
    .find(|t| t.is_host());
    let Some(target) = target else {
        eprintln!(
            "C fixture runtime unavailable: host is not a Linux/Darwin 64-bit descriptor target"
        );
        return None;
    };
    if Command::new("cc").arg("--version").output().is_err() {
        eprintln!("C fixture runtime unavailable: missing cc");
        return None;
    }
    Some((target.triple(), target.convention().name()))
}
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vsys_rest")
}
fn cc(temp: &Temp, sources: &[PathBuf], out: &str) -> PathBuf {
    let exe = temp.0.join(out);
    run(Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-O2"])
        .arg("-I")
        .arg(fixtures())
        .args(sources)
        .arg("-o")
        .arg(&exe));
    exe
}
fn library(target: (&str, &str), body: &str) -> LibraryDescriptor {
    let source = format!(
        r#"sys library Rest (
        target = "{}", abi = "{}",
        link = "static", name = "vsys_rest", artifact = "blake3:{}",
        header = "vsys_rest.h"
    ) {{ {body} }}"#,
        target.0,
        target.1,
        "0".repeat(64)
    );
    parse(&source, Limits::default()).unwrap()
}

const SPEC: &str = "
    wire struct rest_rec { id: u32, flags: u32, value: i64 }
    fn rest_sum(nocapture in recs: CSlice[rest_rec]) -> ForeignVal[i64];
    fn rest_bump(nocapture inout recs: CSlice[rest_rec], flag: u32) -> ForeignVal[Unit];
    fn rest_checksum(nocapture in data: Option[CSlice[u8]]) -> ForeignVal[u32];
    fn rest_read(nocapture out out: Option[u32]) -> ForeignVal[i32];
    fn rest_fill_one(nocapture out out: u32) -> ForeignVal[Unit];
    fn rest_fill_two(nocapture out out: u32) -> ForeignVal[Unit];
    fn rest_overrun(nocapture inout recs: CSlice[rest_rec]) -> ForeignVal[Unit];
    fn rest_find(nocapture in xs: CSlice[u32], v: u32) -> ForeignVal[NullablePtr[u32]];
    fn rest_table() -> ForeignVal[Ptr[u32]];
";

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn n(s: &str) -> u64 {
    s.parse().unwrap()
}

#[test]
fn struct_and_option_slices_and_pointers_against_real_c() {
    let Some(target) = host() else { return };
    let temp = Temp::new();
    let exe = cc(
        &temp,
        &[
            fixtures().join("vsys_rest.c"),
            fixtures().join("vsys_rest_host.c"),
        ],
        "host",
    );
    let output = run(&mut Command::new(&exe));
    let text = String::from_utf8(output.stdout).unwrap();
    let lines: BTreeMap<&str, Vec<&str>> = text
        .lines()
        .map(|l| {
            let mut w = l.split(' ');
            (w.next().unwrap(), w.collect())
        })
        .collect();
    let lib = library(target, SPEC);
    let rec = lib.wire_struct("rest_rec").unwrap();
    assert_eq!((rec.layout().size(), rec.layout().alignment()), (16, 8));

    // Wire-struct `in` slice: stride 16, alignment 8, the C sum is right.
    let sum = lib.function("rest_sum").unwrap();
    assert_eq!(
        sum.parameters()[0].ty().slice_layout(),
        Some(SliceLayout {
            element_size: 16,
            alignment: 8,
            access: Access::In,
            nullable: false
        })
    );
    assert_eq!(
        sum.abi().parameters,
        vec![AbiType::Pointer, AbiType::Integer(Scalar::Usize)]
    );
    let l = &lines["sum"];
    let args = vec![RawArgument::Slice {
        address: n(l[0]),
        length: n(l[1]),
    }];
    let report = sum.check_boundary(&args, l[2].parse::<i64>().unwrap() as u64, &[]);
    assert!(report.holds(), "{report:?}");
    assert_eq!(l[2], "8"); // 2*10 + 3*(-4)
    assert!(sum.prepare_call(args.clone(), ()).is_ok());
    // A base that is 4- but not 8-aligned is rejected for this element.
    let misaligned = vec![RawArgument::Slice {
        address: n(l[0]) + 4,
        length: 2,
    }];
    assert_eq!(
        sum.prepare_call(misaligned, ()).unwrap_err().error(),
        &ValidationError::MisalignedAddress { alignment: 8 }
    );
    // A length whose byte range wraps is out of bounds (16 * len overflows).
    let report = sum.check_arguments(&[RawArgument::Slice {
        address: n(l[0]),
        length: u64::MAX / 8,
    }]);
    assert_eq!(
        report.outcome(Position::Argument(0), Assumption::InBounds),
        Some(&Outcome::Violated(ValidationError::SliceRangeOverflow))
    );

    // Wire-struct `inout` slice; the guard element after it is intact.
    let bump = lib.function("rest_bump").unwrap();
    let l = &lines["bump"];
    let args = vec![
        RawArgument::Slice {
            address: n(l[0]),
            length: n(l[1]),
        },
        RawArgument::Bits(0x10),
    ];
    let guard = GuardObservation {
        argument: 0,
        before: unhex(l[2]),
        after: unhex(l[3]),
    };
    let report = bump.check_boundary(&args, 0, &[guard]);
    assert!(report.holds(), "{report:?}");
    assert_eq!(
        report.outcome(Position::Argument(0), Assumption::WithinLength),
        Some(&Outcome::Held)
    );
    assert_eq!(&l[4..], ["16", "17", "11", "-3"]);

    // Option slice: (NULL, 0) is None; real bytes are checked as usual.
    let checksum = lib.function("rest_checksum").unwrap();
    let none = vec![RawArgument::Slice {
        address: 0,
        length: 0,
    }];
    let report = checksum.check_boundary(&none, n(lines["checksum_null"][0]), &[]);
    assert!(report.holds(), "{report:?}");
    assert!(matches!(
        report.outcome(Position::Argument(0), Assumption::NonNull),
        Some(Outcome::NotApplicable(_))
    ));
    assert_eq!(lines["checksum_null"][0], "4294967295");
    assert!(checksum.prepare_call(none, ()).is_ok());
    let l = &lines["checksum"];
    let some = vec![RawArgument::Slice {
        address: n(l[0]),
        length: n(l[1]),
    }];
    assert!(checksum.check_boundary(&some, n(l[2]), &[]).holds());
    let expected = [1u32, 2, 3]
        .iter()
        .fold(17u32, |c, b| c.wrapping_mul(31).wrapping_add(*b));
    assert_eq!(n(l[2]), u64::from(expected));
    // Negative: a null Option slice with a length is not None.
    assert_eq!(
        checksum
            .prepare_call(
                vec![RawArgument::Slice {
                    address: 0,
                    length: 3
                }],
                ()
            )
            .unwrap_err()
            .error(),
        &ValidationError::NullSliceWithLength
    );
    // Negative: the same null pointer for a non-Option slice.
    assert_eq!(
        sum.prepare_call(
            vec![RawArgument::Slice {
                address: 0,
                length: 0
            }],
            ()
        )
        .unwrap_err()
        .error(),
        &ValidationError::NullAddress
    );

    // Option single pointer: NULL is None and C returns -1; a real cell is
    // written.
    let read = lib.function("rest_read").unwrap();
    let report = read.check_boundary(
        &[RawArgument::Pointer { address: 0 }],
        lines["read_null"][0].parse::<i32>().unwrap() as u32 as u64,
        &[],
    );
    assert!(report.holds(), "{report:?}");
    assert_eq!(lines["read_null"][0], "-1");
    let l = &lines["read"];
    assert!(
        read.check_boundary(&[RawArgument::Pointer { address: n(l[0]) }], n(l[1]), &[])
            .holds()
    );
    assert_eq!(l[2], "7");

    // Single element: the C function that writes one element keeps its guard;
    // the one that writes two is caught per assumption.
    let one = lib.function("rest_fill_one").unwrap();
    let l = &lines["fill_one"];
    let report = one.check_boundary(
        &[RawArgument::Pointer { address: n(l[0]) }],
        0,
        &[GuardObservation {
            argument: 0,
            before: unhex(l[1]),
            after: unhex(l[2]),
        }],
    );
    assert_eq!(
        report.outcome(Position::Argument(0), Assumption::SingleElement),
        Some(&Outcome::Held)
    );
    assert!(report.holds());
    assert_eq!(l[3], "11");
    let two = lib.function("rest_fill_two").unwrap();
    let l = &lines["fill_two"];
    let args = [RawArgument::Pointer { address: n(l[0]) }];
    let report = two.check_boundary(
        &args,
        0,
        &[GuardObservation {
            argument: 0,
            before: unhex(l[1]),
            after: unhex(l[2]),
        }],
    );
    assert_eq!(
        report.outcome(Position::Argument(0), Assumption::SingleElement),
        Some(&Outcome::Violated(ValidationError::GuardOverwritten {
            first_changed: 0
        }))
    );
    assert_eq!(report.violations().count(), 1);
    // Without a guard the same call is reported unchecked, not held.
    let report = two.check_boundary(&args, 0, &[]);
    assert!(report.holds());
    assert!(matches!(
        report.outcome(Position::Argument(0), Assumption::SingleElement),
        Some(Outcome::Unchecked(_))
    ));

    // Slice length: writing recs[n] is caught at the guard element.
    let overrun = lib.function("rest_overrun").unwrap();
    let l = &lines["overrun"];
    let report = overrun.check_boundary(
        &[RawArgument::Slice {
            address: n(l[0]),
            length: n(l[1]),
        }],
        0,
        &[GuardObservation {
            argument: 0,
            before: unhex(l[2]),
            after: unhex(l[3]),
        }],
    );
    assert_eq!(
        report.outcome(Position::Argument(0), Assumption::WithinLength),
        Some(&Outcome::Violated(ValidationError::GuardOverwritten {
            first_changed: 4
        }))
    );

    // nocapture: a returned pointer into the borrowed slice is a violation;
    // a pointer to the library's own table is not.
    let find = lib.function("rest_find").unwrap();
    let l = &lines["find"];
    let args = [
        RawArgument::Slice {
            address: n(l[0]),
            length: n(l[1]),
        },
        RawArgument::Bits(7),
    ];
    assert_eq!(n(l[2]), n(l[0]) + 8);
    let report = find.check_boundary(&args, n(l[2]), &[]);
    assert_eq!(
        report.outcome(Position::Argument(0), Assumption::NoCaptureViaReturn),
        Some(&Outcome::Violated(ValidationError::ReturnAliasesBorrow {
            argument: 0
        }))
    );
    assert!(matches!(
        report.outcome(Position::Argument(0), Assumption::NoRetention),
        Some(Outcome::Unchecked(_))
    ));
    let table = n(lines["table"][0]);
    let report = find.check_boundary(&args, table, &[]);
    assert_eq!(
        report.outcome(Position::Argument(0), Assumption::NoCaptureViaReturn),
        Some(&Outcome::Held)
    );
    let t = lib.function("rest_table").unwrap();
    let report = t.check_boundary(&[], table, &[]);
    assert_eq!(
        report.outcome(Position::Return, Assumption::NonNull),
        Some(&Outcome::Held)
    );
    let report = t.check_boundary(&[], 0, &[]);
    assert_eq!(
        report.outcome(Position::Return, Assumption::NonNull),
        Some(&Outcome::Violated(ValidationError::NullAddress))
    );
    eprintln!(
        "executed real C wire-struct/Option slice fixture on {}; descriptor {}",
        target.0,
        lib.identity()
    );
}

/// Compile and run the probe for `name` against the fixture header.
fn probe(temp: &Temp, lib: &LibraryDescriptor, name: &str, c_type: &str) -> String {
    let w = lib.wire_struct(name).unwrap();
    let src = temp.0.join(format!("probe_{name}.c"));
    fs::write(&src, w.c_layout_probe(c_type, "vsys_rest.h").unwrap()).unwrap();
    let exe = cc(temp, &[src], &format!("probe_{name}"));
    String::from_utf8(run(&mut Command::new(exe)).stdout).unwrap()
}

#[test]
fn wire_struct_layouts_are_checked_against_the_c_header() {
    let Some(target) = host() else { return };
    let temp = Temp::new();
    let lib = library(
        target,
        "wire struct rest_rec { id: u32, flags: u32, value: i64 }
         wire struct rest_pair { tag: u16, kind: u16, pad: ZeroPad[4], rec: rest_rec }
         wire struct swapped { flags: u32, id: u32, value: i64 }
         wire struct narrow { id: u32, flags: u32, value: i32, tail: ZeroPad[4] }
         wire struct wire_be { id: u32be, flags: u32be, value: i64be }
         fn rest_sum(nocapture in recs: CSlice[rest_rec]) -> ForeignVal[i64];",
    );
    let rec = lib.wire_struct("rest_rec").unwrap();
    rec.check_c_layout(&probe(&temp, &lib, "rest_rec", "rest_rec_t"))
        .unwrap();
    rec.check_c_layout(&probe(&temp, &lib, "rest_rec", "struct rest_rec"))
        .unwrap();
    // The padded C struct has an explicit ZeroPad here; nested fields are
    // probed by path. C names the hole nothing, so `pad` is missing in C.
    let pair = lib.wire_struct("rest_pair").unwrap();
    let out = probe_raw(
        &temp,
        pair,
        "struct rest_pair",
        &["tag", "kind", "rec", "rec.id", "rec.flags", "rec.value"],
    );
    let problems = pair.check_c_layout(&out).unwrap_err();
    assert_eq!(
        problems,
        vec![CLayoutMismatch::MissingField {
            field: "pad".into()
        }]
    );
    // Same header, wrong field order: two offset mismatches.
    let swapped = lib.wire_struct("swapped").unwrap();
    let out = probe_raw(&temp, swapped, "rest_rec_t", &["flags", "id", "value"]);
    assert_eq!(
        swapped.check_c_layout(&out).unwrap_err(),
        vec![
            CLayoutMismatch::FieldOffset {
                field: "flags".into(),
                wire: 0,
                c: 4
            },
            CLayoutMismatch::FieldOffset {
                field: "id".into(),
                wire: 4,
                c: 0
            },
        ]
    );
    // A narrower field: same size, wrong field width and a missing pad.
    let narrow = lib.wire_struct("narrow").unwrap();
    let out = probe_raw(&temp, narrow, "rest_rec_t", &["id", "flags", "value"]);
    let problems = narrow.check_c_layout(&out).unwrap_err();
    assert!(problems.contains(&CLayoutMismatch::FieldSize {
        field: "value".into(),
        wire: 4,
        c: 8
    }));
    // Byte-aligned explicit-endian fields check a weaker alignment than C's.
    let be = lib.wire_struct("wire_be").unwrap();
    let out = probe(&temp, &lib, "wire_be", "rest_rec_t");
    assert_eq!(
        be.check_c_layout(&out).unwrap_err(),
        vec![CLayoutMismatch::Alignment { wire: 1, c: 8 }]
    );
    // Untrusted probe output is a typed mismatch, never a panic.
    for bad in [
        "",
        "not a probe",
        "vesper-c-layout/1\nsize x\nalign 8\n",
        "vesper-c-layout/1\nsize 16\n",
        "vesper-c-layout/1\nsize 16\nalign 8\nfield id 0\n",
    ] {
        assert!(matches!(
            rec.check_c_layout(bad).unwrap_err()[0],
            CLayoutMismatch::Malformed { .. }
        ));
    }
    assert!(
        rec.c_layout_probe("rest_rec_t; system(\"x\")", "vsys_rest.h")
            .is_err()
    );
    assert!(rec.c_layout_probe("rest_rec_t", "../vsys_rest.h").is_err());
}

/// A probe for a C type whose field names differ from the wire struct's: the
/// wire probe would not compile, so print the C fields that do exist and keep
/// the wire paths for comparison.
fn probe_raw(temp: &Temp, w: &WireStructDescriptor, c_type: &str, fields: &[&str]) -> String {
    let mut src = format!(
        "#include <stddef.h>\n#include <stdio.h>\n#include \"vsys_rest.h\"\nint main(void) {{\n printf(\"{C_LAYOUT_SCHEMA}\\n\");\n printf(\"size %zu\\n\", sizeof({c_type}));\n printf(\"align %zu\\n\", (size_t)_Alignof({c_type}));\n"
    );
    for f in fields {
        src.push_str(&format!(
            " printf(\"field {f} %zu %zu\\n\", offsetof({c_type}, {f}), sizeof((({c_type} *)0)->{f}));\n"
        ));
    }
    src.push_str(" return 0;\n}\n");
    let path = temp.0.join(format!("raw_{}.c", w.name()));
    fs::write(&path, src).unwrap();
    let exe = cc(temp, &[path], &format!("raw_{}", w.name()));
    String::from_utf8(run(&mut Command::new(exe)).stdout).unwrap()
}

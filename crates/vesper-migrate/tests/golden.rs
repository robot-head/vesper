//! Worked example (design §29.3): migrate a small C header and a small Rust
//! crate, and compare every artefact with the committed expected outputs.
//! Regenerate with `VESPER_MIGRATE_BLESS=1 cargo test -p vesper-migrate`.

use std::path::{Path, PathBuf};

use vesper_ffi::sys::{
    AbiType, LinkMode, ParameterType, Pointee, PointerAccess, PointerLayout, Scalar, Target,
};
use vesper_migrate::{Limits, Migration, Mode, RefusalReason, c, rust, synth};

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(fixture(rel))
        .unwrap()
        .replace("\r\n", "\n")
}

fn golden(rel: &str, actual: &str) {
    let path = fixture(rel);
    if std::env::var_os("VESPER_MIGRATE_BLESS").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; bless with VESPER_MIGRATE_BLESS=1", path.display()))
        .replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "golden mismatch for {rel}; bless with VESPER_MIGRATE_BLESS=1 after review"
    );
}

fn options(target: Target) -> synth::SysSpecOptions {
    synth::SysSpecOptions {
        library: "LegacyCodecAbi".into(),
        target,
        link: LinkMode::Static,
        link_name: "legacy_codec".into(),
        // A fixed digest keeps the golden text stable; artifact verification
        // against real bytes is vesper-ffi's own test.
        artifact_blake3: [0xab; 32],
        header: Some("legacy_codec.h".into()),
        pkg_config: Some("legacy-codec ^0.8.0".into()),
    }
}

fn c_migration() -> Migration {
    c::analyze_header("legacy_codec.h", &read("legacy_codec.h"), Limits::default()).unwrap()
}

fn rust_migration() -> Migration {
    rust::analyze_crate_dir(&fixture("legacy_codec"), Limits::default()).unwrap()
}

#[test]
fn c_header_golden_outputs() {
    let m = c_migration();
    let spec = synth::sys_spec(&m, &options(Target::LinuxX86_64)).unwrap();
    golden("expected/legacy_codec.c.vsys", &spec.text);
    golden(
        "expected/legacy_codec.c.vesper",
        &synth::vesper_declarations(&m),
    );
    golden("expected/legacy_codec.c.report.txt", &synth::report(&m));
}

#[test]
fn rust_crate_golden_outputs() {
    let m = rust_migration();
    let spec = synth::sys_spec(&m, &options(Target::LinuxX86_64)).unwrap();
    golden("expected/legacy_codec.rs.vsys", &spec.text);
    golden(
        "expected/legacy_codec.rs.vesper",
        &synth::vesper_declarations(&m),
    );
    golden("expected/legacy_codec.rs.report.txt", &synth::report(&m));
}

#[test]
fn c_sys_spec_round_trips_through_the_bridge_on_every_target() {
    let m = c_migration();
    for target in [
        Target::LinuxX86_64,
        Target::LinuxAarch64,
        Target::DarwinAarch64,
        Target::WindowsX86_64,
    ] {
        let spec = synth::sys_spec(&m, &options(target)).unwrap();
        let d = &spec.descriptor;
        assert_eq!(d.target(), target);
        assert_eq!(d.functions().len(), m.functions.len());
        let f = d.function("legacy_parse_batch_header_into").unwrap();
        assert_eq!(f.parameters().len(), 2);
        assert!(matches!(
            f.parameters()[0].ty(),
            ParameterType::Slice {
                element: Pointee::Scalar(Scalar::U8),
                access: vesper_ffi::sys::Access::In,
                nullable: false,
            }
        ));
        assert!(matches!(
            f.parameters()[1].ty(),
            ParameterType::Slice {
                element: Pointee::Scalar(Scalar::U32),
                access: vesper_ffi::sys::Access::Inout,
                nullable: false,
            }
        ));
        // The slice expands back to the C header's four machine arguments.
        assert_eq!(
            f.abi().parameters,
            vec![
                AbiType::Pointer,
                AbiType::Integer(Scalar::Usize),
                AbiType::Pointer,
                AbiType::Integer(Scalar::Usize)
            ]
        );
        assert_eq!(f.abi().result, Some(AbiType::Integer(Scalar::I32)));
        // A re-parse of the text gives the same content identity.
        let again = vesper_ffi::sys::parse(&spec.text, vesper_ffi::sys::Limits::default()).unwrap();
        assert_eq!(again.identity(), d.identity());
    }
}

#[test]
fn c_front_end_lifts_typedefs_arrays_constness_and_layouts() {
    let m = c_migration();
    let enc = m.function("legacy_encode").unwrap();
    assert_eq!(
        enc.params.len(),
        2,
        "array parameter decays, lengths fold into slices"
    );
    assert_eq!(
        (enc.params[0].mode, enc.params[1].mode),
        (Mode::In, Mode::Inout)
    );
    assert_eq!(
        m.function("legacy_status").unwrap().result,
        "ForeignVal[u32]",
        "typedef resolved"
    );
    assert_eq!(
        m.function("legacy_version").unwrap().result,
        "ForeignVal[u32]",
        "`unsigned int` is 32-bit on every target"
    );
    let sizes = synth::wire_sizes(&m);
    assert!(sizes.contains(&("batch_header".into(), 24, 8)));
    assert!(sizes.contains(&("decoder_stats".into(), 32, 8)));
    let reasons: Vec<_> = m
        .all_refusals()
        .iter()
        .map(|r| (r.item.clone(), r.reason))
        .collect();
    for expected in [
        ("struct padded_record", RefusalReason::WireLayout),
        ("union legacy_word", RefusalReason::Union),
        ("fn legacy_swap", RefusalReason::AliasingPointers),
        ("fn legacy_sum4", RefusalReason::ArrayWithoutLength),
        ("fn legacy_set_name", RefusalReason::ArrayWithoutLength),
        ("fn legacy_attach", RefusalReason::OpaquePointer),
        ("fn legacy_pack", RefusalReason::PointerParameter),
        ("fn legacy_offset", RefusalReason::PlatformDependentWidth),
        ("fn legacy_ratio", RefusalReason::Floating),
        ("fn legacy_log", RefusalReason::Variadic),
        ("fn legacy_register", RefusalReason::FunctionPointer),
        (
            "fn legacy_last_status",
            RefusalReason::ImplementationDefinedCarrier,
        ),
        ("fn legacy_flush", RefusalReason::UnsupportedItem),
        ("fn legacy_inline_helper", RefusalReason::UnsupportedItem),
        (
            "global legacy_global_counter",
            RefusalReason::UnsupportedItem,
        ),
        (
            "enum codec_status",
            RefusalReason::ImplementationDefinedCarrier,
        ),
    ] {
        assert!(
            reasons.contains(&(expected.0.to_string(), expected.1)),
            "missing refusal {expected:?} in {reasons:#?}"
        );
    }
    assert_eq!(
        m.enumeration("codec_status")
            .unwrap()
            .variants
            .last()
            .unwrap(),
        &("CODEC_LAST".to_string(), 3)
    );
}

#[test]
fn rust_front_end_lifts_modes_and_field_tree_footprints() {
    let m = rust_migration();
    let render = |name: &str| {
        let f = m.function(name).unwrap_or_else(|| panic!("{name} missing"));
        f.params
            .iter()
            .map(vesper_migrate::lift::render_param)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        render("parse_batch_header"),
        [
            "in raw_bytes: ByteSlice",
            "inout state.{last_crc, record_count}: DecoderStats"
        ]
    );
    assert_eq!(
        render("record_bytes_hint"),
        ["in state.{record_count}: DecoderStats", "per_record: u32"]
    );
    assert_eq!(
        render("DecoderStats::observe"),
        ["inout self.{bytes_seen}: DecoderStats", "len: usize"]
    );
    assert_eq!(
        render("DecoderStats::summary"),
        ["in self.{bytes_seen, last_crc}: DecoderStats"]
    );
    assert_eq!(
        render("DecoderStats::reset"),
        ["inout self: DecoderStats"],
        "whole-value write keeps the full footprint"
    );
    assert_eq!(
        render("DecoderStats::into_parts"),
        ["sink self: DecoderStats"]
    );
    assert_eq!(
        render("legacy_parse_batch_header"),
        [
            "in raw_bytes: ByteSlice",
            "inout out_crc: u32",
            "inout out_count: i32"
        ]
    );
    assert_eq!(
        m.function("parse_batch_header").unwrap().result,
        "Result[i64, ParseError]"
    );
    // Only unmangled extern "C" functions whose pointers are annotated (or
    // are references) reach the spec.
    let spec = synth::sys_spec(&m, &options(Target::LinuxX86_64)).unwrap();
    let names: Vec<_> = spec
        .descriptor
        .functions()
        .iter()
        .map(|f| f.symbol().to_string())
        .collect();
    assert_eq!(names.len(), 6, "{names:?}");
    for n in [
        "legacy_bump_crc",
        "legacy_classify",
        "legacy_header_len",
        "legacy_parse_batch_header",
        "legacy_parse_batch_header_into",
        "legacy_reset_stats",
    ] {
        assert!(names.iter().any(|x| x == n), "{n} missing from {names:?}");
    }
    let reasons: Vec<_> = m
        .all_refusals()
        .iter()
        .map(|r| (r.item.clone(), r.reason))
        .collect();
    for expected in [
        ("fn bump", RefusalReason::InteriorMutability),
        ("struct SharedHits", RefusalReason::InteriorMutability),
        ("fn rewind", RefusalReason::SelfReferentialLifetime),
        ("fn first_byte", RefusalReason::ReturnedBorrow),
        ("fn cursor_peek", RefusalReason::BorrowingType),
        ("struct Cursor", RefusalReason::BorrowingType),
        ("fn raw_peek", RefusalReason::RawPointerWithoutAnnotation),
        ("fn checksum", RefusalReason::Generic),
        ("fn on_record", RefusalReason::FunctionPointer),
        ("fn parse_batch_header: vsys", RefusalReason::NotCAbi),
        ("fn legacy_swap: vsys", RefusalReason::AliasingPointers),
    ] {
        assert!(
            reasons.contains(&(expected.0.to_string(), expected.1)),
            "missing refusal {expected:?} in {reasons:#?}"
        );
    }
    assert_eq!(
        synth::wire_sizes(&m),
        vec![("DecoderStats".to_string(), 16, 8)]
    );
}

#[test]
fn single_file_analysis_refuses_out_of_line_modules() {
    let m = rust::analyze_source(
        "legacy_codec",
        &read("legacy_codec/src/lib.rs"),
        Limits::default(),
    )
    .unwrap();
    assert!(
        m.refusals
            .iter()
            .any(|r| r.item == "mod parse" && r.reason == RefusalReason::UnsupportedItem)
    );
    // The shim's own signature is still lifted; items of unread modules are not.
    assert!(m.function("legacy_parse_batch_header").is_some());
    assert!(m.function("parse_batch_header").is_none());
}

#[test]
fn single_out_pointers_lift_to_the_design_signature() {
    // §29.3: the Rust shim lifts to exactly the design's sys-spec entry.
    let m = rust_migration();
    let spec = synth::sys_spec(&m, &options(Target::LinuxX86_64)).unwrap();
    assert!(spec.text.contains(
        "fn legacy_parse_batch_header(nocapture in raw_bytes: CSlice[u8], nocapture inout out_crc: u32, nocapture inout out_count: i32) -> ForeignVal[i32];"
    ));
    let f = spec
        .descriptor
        .function("legacy_parse_batch_header")
        .unwrap();
    assert_eq!(
        f.abi().parameters,
        vec![
            AbiType::Pointer,
            AbiType::Integer(Scalar::Usize),
            AbiType::Pointer,
            AbiType::Pointer
        ]
    );
    assert_eq!(
        f.parameters()[1].ty().pointer_layout(),
        Some(PointerLayout {
            size: 4,
            alignment: 4,
            access: PointerAccess::Inout,
            nullable: false
        })
    );
    let reset = spec.descriptor.function("legacy_reset_stats").unwrap();
    let ParameterType::Pointer {
        pointee: Pointee::Struct(w),
        access: PointerAccess::Out,
        nullable: false,
    } = reset.parameters()[0].ty()
    else {
        panic!("{:?}", reset.parameters()[0].ty())
    };
    assert_eq!((w.name(), w.layout().size()), ("DecoderStats", 16));
    // The C header's struct out-pointer carries its nested wire structs. The
    // header does not declare it nonnull, so it is lifted as `Option`.
    let c = c_migration();
    let spec = synth::sys_spec(&c, &options(Target::LinuxAarch64)).unwrap();
    let fill = spec.descriptor.function("legacy_fill").unwrap();
    assert_eq!(
        fill.parameters()[0].ty().pointer_layout(),
        Some(PointerLayout {
            size: 32,
            alignment: 8,
            access: PointerAccess::Inout,
            nullable: true
        })
    );
    assert_eq!(spec.descriptor.wire_structs().len(), 2);
    let read = spec.descriptor.function("legacy_read_header").unwrap();
    assert_eq!(read.parameters()[1].ty().pointer_layout().unwrap().size, 24);
}

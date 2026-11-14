//! `vesper migrate` lifts C pointer nullability (`Option` unless the header
//! says `nonnull`/`_Nonnull`), slices of wire structs, and the Rust
//! `nullable` annotation; and the wire structs it emits match the C header's
//! layout as compiled by the host C compiler.
use std::path::{Path, PathBuf};
use std::process::Command;

use vesper_ffi::sys::{LinkMode, ParameterType, Pointee, Target, ValueType};
use vesper_migrate::{
    Access, Limits, Migration, PointerAccess, RefusalReason, Scalar, SysParam, SysPointee, c, rust,
    synth,
};

fn header(src: &str) -> Migration {
    c::analyze_header("t.h", src, Limits::default()).unwrap()
}
fn opts() -> synth::SysSpecOptions {
    synth::SysSpecOptions {
        library: "T".into(),
        target: Target::LinuxX86_64,
        link: LinkMode::Static,
        link_name: "t".into(),
        artifact_blake3: [0; 32],
        header: None,
        pkg_config: None,
    }
}
fn params(m: &Migration, f: &str) -> Vec<SysParam> {
    m.function(f)
        .unwrap_or_else(|| panic!("{f}: {:#?}", m.all_refusals()))
        .sys
        .clone()
        .unwrap()
        .params
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}
fn refusal(m: &Migration, item: &str) -> RefusalReason {
    m.all_refusals()
        .into_iter()
        .find(|r| r.item == item)
        .unwrap_or_else(|| panic!("no refusal for {item}: {:#?}", m.all_refusals()))
        .reason
}
fn ptr(pointee: SysPointee, access: PointerAccess, nullable: bool) -> SysParam {
    SysParam::Pointer {
        pointee,
        access,
        nullable,
    }
}

#[test]
fn c_pointers_are_option_unless_the_header_says_nonnull() {
    let m = header(
        "struct pair { uint32_t lo; uint32_t hi; };
         void plain(uint32_t *p);
         void qualified(uint32_t * _Nonnull a, uint64_t * _Nullable b);
         void all(uint32_t *a, const uint8_t *d, size_t n) __attribute__((nonnull));
         void some(uint32_t *a, const uint8_t *d, size_t n) __attribute__((nonnull(2)));
         __attribute__((nonnull(1))) void leading(struct pair *p);
         const uint32_t *maybe(void);
         const uint32_t *always(void) __attribute__((returns_nonnull));
         const uint32_t * _Nonnull qualified_return(void);",
    );
    let u32p = |nullable| {
        ptr(
            SysPointee::Scalar(Scalar::U32),
            PointerAccess::Inout,
            nullable,
        )
    };
    let bytes = |nullable| SysParam::slice(SysPointee::Scalar(Scalar::U8), Access::In, nullable);
    assert_eq!(params(&m, "plain"), [u32p(true)]);
    assert_eq!(
        params(&m, "qualified"),
        [
            u32p(false),
            ptr(SysPointee::Scalar(Scalar::U64), PointerAccess::Inout, true)
        ]
    );
    assert_eq!(params(&m, "all"), [u32p(false), bytes(false)]);
    // 1-based indices: only `d` (parameter 2) is declared non-null.
    assert_eq!(params(&m, "some"), [u32p(true), bytes(false)]);
    assert_eq!(
        params(&m, "leading"),
        [ptr(
            SysPointee::Struct("pair".into()),
            PointerAccess::Inout,
            false
        )]
    );
    let spec = synth::sys_spec(&m, &opts()).unwrap();
    for line in [
        "fn plain(nocapture inout p: Option[u32]) -> ForeignVal[Unit];",
        "fn qualified(nocapture inout a: u32, nocapture inout b: Option[u64]) -> ForeignVal[Unit];",
        "fn all(nocapture inout a: u32, nocapture in d: CSlice[u8]) -> ForeignVal[Unit];",
        "fn some(nocapture inout a: Option[u32], nocapture in d: CSlice[u8]) -> ForeignVal[Unit];",
        "fn maybe() -> ForeignVal[NullablePtr[u32]];",
        "fn always() -> ForeignVal[Ptr[u32]];",
        "fn qualified_return() -> ForeignVal[Ptr[u32]];",
    ] {
        assert!(spec.text.contains(line), "{line}\n{}", spec.text);
    }
    let d = &spec.descriptor;
    assert_eq!(
        d.function("always").unwrap().result(),
        &ValueType::Address {
            pointee: Scalar::U32,
            nullable: false
        }
    );
    // Every lifted pointer says which way its nullability was decided.
    let plain = m.function("plain").unwrap();
    assert!(plain.assumptions[0].contains("nullable (`Option`)"));
    let all = m.function("all").unwrap();
    assert!(
        all.assumptions
            .iter()
            .all(|a| a.contains("non-null as the header declares"))
    );
}

#[test]
fn contradictory_or_wrong_nonnull_annotations_are_refused() {
    let m = header(
        "void conflict(uint32_t * _Nullable p) __attribute__((nonnull));
         void index_value(uint32_t a, uint32_t *b) __attribute__((nonnull(1)));
         void index_range(uint32_t *b) __attribute__((nonnull(2)));
         void index_zero(uint32_t *b) __attribute__((nonnull(0)));",
    );
    for f in ["conflict", "index_value", "index_range", "index_zero"] {
        assert_eq!(
            refusal(&m, &format!("fn {f}")),
            RefusalReason::BadAnnotation,
            "{f}"
        );
        assert!(m.function(f).is_none());
    }
}

#[test]
fn c_struct_pointer_and_length_lifts_to_a_wire_struct_slice() {
    let m = header(
        "typedef struct rec { uint32_t id; uint32_t flags; int64_t value; } rec_t;
         struct gap { uint8_t a; uint32_t b; };
         struct opaque;
         int64_t sum(const rec_t *recs, size_t n) __attribute__((nonnull));
         void bump(rec_t *recs, size_t n, uint32_t flag);
         void gaps(const struct gap *g, size_t n);
         void unknown(const struct opaque *o, size_t n);
         void alias(rec_t *one, const rec_t *many, size_t n);",
    );
    assert_eq!(
        params(&m, "sum"),
        [SysParam::ElementSlice {
            element: SysPointee::Struct("rec".into()),
            access: Access::In,
            nullable: false
        }]
    );
    assert_eq!(
        params(&m, "bump")[0],
        SysParam::ElementSlice {
            element: SysPointee::Struct("rec".into()),
            access: Access::Inout,
            nullable: true
        }
    );
    assert_eq!(refusal(&m, "fn gaps"), RefusalReason::PointerParameter);
    assert!(matches!(
        refusal(&m, "fn unknown"),
        RefusalReason::PointerParameter | RefusalReason::OpaquePointer
    ));
    // A written single pointer and a slice of the same struct may alias.
    assert_eq!(refusal(&m, "fn alias"), RefusalReason::AliasingPointers);
    let spec = synth::sys_spec(&m, &opts()).unwrap();
    assert!(
        spec.text
            .contains("wire struct rec { id: u32, flags: u32, value: i64 }")
    );
    assert!(
        spec.text
            .contains("fn sum(nocapture in recs: CSlice[rec]) -> ForeignVal[i64];")
    );
    assert!(spec.text.contains(
        "fn bump(nocapture inout recs: Option[CSlice[rec]], flag: u32) -> ForeignVal[Unit];"
    ));
    let ParameterType::Slice {
        element: Pointee::Struct(w),
        nullable: false,
        ..
    } = spec.descriptor.function("sum").unwrap().parameters()[0].ty()
    else {
        panic!()
    };
    assert_eq!((w.layout().size(), w.layout().alignment()), (16, 8));
}

#[test]
fn rust_nullable_annotation_and_struct_slices() {
    let m = rust::analyze_source(
        "t",
        r#"
        #[repr(C)] pub struct Pair { pub lo: u32, pub hi: u32 }
        /// vesper-migrate: out out nullable
        #[no_mangle] pub unsafe extern "C" fn maybe_out(out: *mut u32) -> i32 { 0 }
        /// vesper-migrate: pairs in len n
        #[no_mangle] pub unsafe extern "C" fn pairs(pairs: *const Pair, n: usize) -> u64 { 0 }
        /// vesper-migrate: data in len n nullable
        #[no_mangle] pub unsafe extern "C" fn maybe_bytes(data: *const u8, n: usize) -> u32 { 0 }
        /// vesper-migrate: data in nullable len n
        #[no_mangle] pub unsafe extern "C" fn misordered(data: *const u8, n: usize) -> u32 { 0 }
        "#,
        Limits::default(),
    )
    .unwrap();
    let spec = synth::sys_spec(&m, &opts()).unwrap();
    for line in [
        "fn maybe_out(nocapture out out: Option[u32]) -> ForeignVal[i32];",
        "fn pairs(nocapture in pairs: CSlice[Pair]) -> ForeignVal[u64];",
        "fn maybe_bytes(nocapture in data: Option[CSlice[u8]]) -> ForeignVal[u32];",
        "wire struct Pair { lo: u32, hi: u32 }",
    ] {
        assert!(spec.text.contains(line), "{line}\n{}", spec.text);
    }
    assert_eq!(refusal(&m, "fn misordered"), RefusalReason::BadAnnotation);
}

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

/// The worked-example header compiles as C (so its `nonnull` attributes are
/// valid), and every wire struct `vesper migrate` emitted from it matches the
/// layout the C compiler gives the same struct.
#[test]
fn lifted_wire_structs_match_the_compiled_c_header() {
    if !(cfg!(unix) && cfg!(target_pointer_width = "64")) {
        eprintln!("C layout probe unavailable: needs a 64-bit Unix host");
        return;
    }
    if Command::new("cc").arg("--version").output().is_err() {
        eprintln!("C layout probe unavailable: missing cc");
        return;
    }
    let src = std::fs::read_to_string(fixture("legacy_codec.h")).unwrap();
    let m = c::analyze_header("legacy_codec.h", &src, Limits::default()).unwrap();
    let spec = synth::sys_spec(&m, &opts()).unwrap();
    let dir = std::env::temp_dir().join(format!("vesper-migrate-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let structs = spec.descriptor.wire_structs();
    assert!(structs.len() >= 2);
    for w in structs {
        let probe = w
            .c_layout_probe(&format!("struct {}", w.name()), "legacy_codec.h")
            .unwrap();
        let c_file = dir.join(format!("{}.c", w.name()));
        let exe = dir.join(w.name());
        std::fs::write(&c_file, probe).unwrap();
        let out = Command::new("cc")
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I"])
            .arg(fixture(""))
            .arg(&c_file)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let run = Command::new(&exe).output().unwrap();
        assert!(run.status.success());
        let text = String::from_utf8(run.stdout).unwrap();
        w.check_c_layout(&text)
            .unwrap_or_else(|e| panic!("{}: {e:?}\n{text}", w.name()));
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "checked {} lifted wire structs against the compiled legacy_codec.h",
        structs.len()
    );
}

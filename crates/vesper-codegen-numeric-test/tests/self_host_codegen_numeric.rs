//! Self-hosting SH4 lane 11 (#1285), part 1: `@vesper/codegen-numeric`
//! against the Rust native lowering.
//!
//! The generated AOT runtimes (src/regions/aot_runtime.rs,
//! src/tiled/aot_runtime.rs, src/fip/aot_runtime.rs) are whole functions
//! defined into a module, so the Vesper port emits them through the
//! existing Cranelift shim without lane 1's lent builders. The reference is
//! the real compiler: `native::object_for_target` of programs that use
//! regions, `TiledSeq` and `Box`, whose objects hold the runtime. Every
//! runtime symbol (functions and fault messages) must have the same bytes
//! and the same relocations (offset, target symbol, kind and addend) in the
//! Vesper object, for each target and with optimize on and off.
use object::{Object, ObjectSection, ObjectSymbol, RelocationTarget};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use vesper::ffi_lowering::{self, ForeignChecked, Handler, Imports};
use vesper::vector::{self, Conv, Elem, Intrinsic, Profile, Shape};
use vesper::{check, native, parser};
use vesper_cranelift_sys::codes;
use vesper_cranelift_sys::cranelift_codegen::ir::{self, types};

/// The handle counters of the shim are process-wide.
static SERIAL: Mutex<()> = Mutex::new(());

/// Package files in assembly order after `@vesper/cranelift`.
const FILES: [&str; 5] = ["rt.ves", "regions.ves", "tiled.ves", "boxes.ves", "lib.ves"];

/// The ELF targets compared (the reader is built with ELF support only).
const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

/// `lib.ves`'s `RT_*` family bits.
const RT_REGIONS: u32 = 1;
const RT_TILED: u32 = 2;
const RT_BOX_ALLOC: u32 = 4;
const RT_BOX_FREE: u32 = 8;

/// A program and the runtimes its object holds.
struct Program {
    name: &'static str,
    source: &'static str,
    families: u32,
}

const PROGRAMS: [Program; 3] = [
    Program {
        name: "regions",
        source: r#"
copy struct Point {
    x: i64,
    y: i64,
}
fn main() -> i64 {
    region r(512) {
        let p = r.box(Point { x: 1, y: 2 });
        r.set(p, Point { x: 30, y: 2 });
        let q = r.get(p);
        wrapping_add(q.x, q.y)
    }
}
"#,
        families: RT_REGIONS,
    },
    Program {
        name: "tiled",
        source: r#"
affine struct Token { v: u64 }
tiled(8) affine struct Slot {
    hot id: u64,
    hot weight: u64,
    cold tok: Option[Token],
}

fn main() -> u64 {
    let mut s: TiledSeq[Slot] = TiledSeq::new[Slot]();
    s.push(Slot { id: 11, weight: 22, tok: Option::None });
    s.col_set[weight](0, 33);
    wrapping_add(s.col_get[id](0), s.col_get[weight](0))
}
"#,
        families: RT_REGIONS | RT_TILED,
    },
    Program {
        name: "boxes",
        source: r#"
fn main() -> i64 {
    let b = Box::new(41);
    let Box(v) = b;
    wrapping_add(v, 1)
}
"#,
        families: RT_BOX_ALLOC | RT_BOX_FREE,
    },
];

/// The runtime symbols of each family.
fn family_symbols(families: u32) -> Vec<String> {
    let mut out = vec![];
    if families & (RT_REGIONS | RT_TILED) != 0 {
        out.extend((0..6).map(|i| format!("vesper_region_message{i}")));
        for name in [
            "fault",
            "reclaim",
            "resolve",
            "find_mark",
            "open",
            "close",
            "alloc",
            "load",
            "store",
            "release",
            "checkpoint",
            "rewind",
            "commit",
            "used",
        ] {
            out.push(format!("vesper_region_{name}"));
        }
    }
    if families & RT_TILED != 0 {
        out.extend((0..3).map(|i| format!("vesper_tiled_msg_{i}")));
        for name in [
            "fault",
            "charge_region",
            "ensure_tiles",
            "new",
            "len",
            "tile_count",
            "push_begin",
            "hot_load",
            "hot_store",
            "cold_load",
            "cold_store",
            "tile_col_ptr",
        ] {
            out.push(format!("vesper_tiled_{name}"));
        }
    }
    if families & (RT_BOX_ALLOC | RT_BOX_FREE) != 0 {
        out.push("vesper_box_live".into());
    }
    if families & RT_BOX_ALLOC != 0 {
        out.push("vesper_box_alloc".into());
    }
    if families & RT_BOX_FREE != 0 {
        out.push("vesper_box_free".into());
    }
    out
}

/// The `.vsys` target of the host and its calling convention.
fn host_target() -> Option<(&'static str, &'static str)> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(("x86_64-unknown-linux-gnu", "sysv64"))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(("aarch64-unknown-linux-gnu", "aapcs64"))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(("aarch64-apple-darwin", "darwin64"))
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(("x86_64-pc-windows-msvc", "win64"))
    } else {
        None
    }
}

/// The `sys library` of `@vesper/cranelift`'s `// @export vsys` directive.
fn vsys(source: &str, target: (&str, &str)) -> String {
    let mut out = String::new();
    for line in source.lines() {
        if let Some(rest) = line.strip_prefix("// | ") {
            out.push_str(rest);
            out.push('\n');
        } else if line == "// |" {
            out.push('\n');
        }
    }
    assert!(out.contains("sys library Cranelift"), "no .vsys directive");
    out.replace("@TARGET@", target.0).replace("@ABI@", target.1)
}

/// Cranelift flags of the compiler's object ISA (`isa_with_flags`).
fn flags(optimize: bool) -> &'static str {
    if optimize {
        "enable_llvm_abi_extensions=true;is_pic=true;opt_level=speed"
    } else {
        "enable_llvm_abi_extensions=true;is_pic=true"
    }
}

/// One generated entry per (program, target, optimize).
fn configurations() -> Vec<(usize, &'static str, bool)> {
    let mut out = vec![];
    for p in 0..PROGRAMS.len() {
        for t in TARGETS {
            for o in [false, true] {
                out.push((p, t, o));
            }
        }
    }
    out
}

fn entry_name(i: usize) -> String {
    format!("numeric_runtime_{i}")
}

/// The repository root: Cargo runs tests in this crate's directory, Bazel in
/// the runfiles root.
fn repo() -> std::path::PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for base in [manifest.join("../.."), std::path::PathBuf::from(".")] {
        if base
            .join("packages/vesper-codegen-numeric/src/lib.ves")
            .exists()
        {
            return base;
        }
    }
    panic!("cannot find packages/vesper-codegen-numeric");
}

/// `@vesper/cranelift`, the package and one entry per configuration,
/// checked for the host.
fn checked() -> &'static ForeignChecked {
    static C: OnceLock<ForeignChecked> = OnceLock::new();
    C.get_or_init(|| {
        let repo = repo();
        let shim = std::fs::read_to_string(repo.join("packages/vesper-cranelift/src/lib.ves"))
            .unwrap();
        let mut source = shim.clone();
        for f in FILES {
            source.push('\n');
            source.push_str(
                &std::fs::read_to_string(repo.join("packages/vesper-codegen-numeric/src").join(f))
                    .unwrap(),
            );
        }
        for (i, (p, t, o)) in configurations().into_iter().enumerate() {
            source.push_str(&format!(
                "\npub fn {}() -> u64 eff [Ffi[Cranelift]] {{\n    runtime_object(in \"{t}\".as_bytes(), in \"{}\".as_bytes(), {})\n}}\n",
                entry_name(i),
                flags(o),
                PROGRAMS[p].families
            ));
        }
        let mut imports = Imports::new();
        imports
            .add(&vsys(&shim, host_target().unwrap()), Handler::Direct)
            .unwrap();
        ffi_lowering::check_source(&source, &imports).unwrap_or_else(|e| panic!("{e}"))
    })
}

fn vesper_object(i: usize) -> Vec<u8> {
    let symbols = vesper_cranelift_sys::symbols();
    let v = checked()
        .jit(&entry_name(i), &symbols)
        .unwrap_or_else(|e| panic!("{}: {e}", entry_name(i)));
    let id = match v {
        vesper::runtime::Value::Int(n) => n as u64,
        vesper::runtime::Value::UInt(n, _) => n,
        other => panic!("not a word: {other:?}"),
    };
    assert_ne!(
        id,
        0,
        "{}: the Vesper runtime emission failed",
        entry_name(i)
    );
    vesper_cranelift_sys::take_published(id).unwrap()
}

fn rust_object(p: &Program, target: &str, optimize: bool) -> Vec<u8> {
    let parsed = parser::parse(p.source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let c = check::check(&parsed.module);
    assert!(c.diagnostics.is_empty(), "{}: {:?}", p.name, c.diagnostics);
    native::object_with_options(&c.world, "main", Some(target), optimize)
        .unwrap_or_else(|e| panic!("{}: {e}", p.name))
        .0
}

/// A symbol's bytes and relocations (offset in the symbol, target symbol
/// name, flags and addend).
#[derive(Debug, PartialEq)]
struct Symbol {
    bytes: Vec<u8>,
    relocations: Vec<(u64, String, String, i64)>,
}

fn symbols(bytes: &[u8]) -> BTreeMap<String, Symbol> {
    let file = object::File::parse(bytes).expect("an ELF object");
    let mut out = BTreeMap::new();
    for s in file.symbols() {
        let Ok(name) = s.name() else { continue };
        let Some(index) = s.section_index() else {
            continue;
        };
        let section = file.section_by_index(index).unwrap();
        let data = section.data().unwrap();
        let (start, size) = (s.address(), s.size());
        let bytes = if data.is_empty() {
            vec![0; size as usize] // .bss
        } else {
            data[start as usize..(start + size) as usize].to_vec()
        };
        let mut relocations = vec![];
        for (offset, r) in section.relocations() {
            if offset < start || offset >= start + size {
                continue;
            }
            let target = match r.target() {
                RelocationTarget::Symbol(i) => file
                    .symbol_by_index(i)
                    .and_then(|t| t.name().map(str::to_string))
                    .unwrap_or_default(),
                other => format!("{other:?}"),
            };
            relocations.push((
                offset - start,
                target,
                format!("{:?}", r.flags()),
                r.addend(),
            ));
        }
        out.insert(name.to_string(), Symbol { bytes, relocations });
    }
    out
}

#[test]
fn runtime_symbols_match_the_rust_lowering() {
    if host_target().is_none() {
        eprintln!("no .vsys target for this host; comparison unavailable");
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let before = vesper_cranelift_sys::live_handles();
    let (mut compared, mut bad) = (0, 0);
    for (i, (p, t, o)) in configurations().into_iter().enumerate() {
        let program = &PROGRAMS[p];
        let rust = symbols(&rust_object(program, t, o));
        let vesper = symbols(&vesper_object(i));
        for name in family_symbols(program.families) {
            let r = rust
                .get(&name)
                .unwrap_or_else(|| panic!("{}: the Rust object lacks {name}", program.name));
            compared += 1;
            match vesper.get(&name) {
                Some(v) if v == r => {}
                Some(v) => {
                    bad += 1;
                    eprintln!(
                        "{} {t} optimize={o}: {name} differs: {} vs {} bytes, relocations {:?} vs {:?}",
                        program.name,
                        v.bytes.len(),
                        r.bytes.len(),
                        v.relocations,
                        r.relocations
                    );
                }
                None => {
                    bad += 1;
                    eprintln!(
                        "{} {t} optimize={o}: the Vesper object lacks {name}",
                        program.name
                    );
                }
            }
        }
    }
    eprintln!("runtime symbols: {bad} of {compared} differ");
    assert_eq!(bad, 0);
    assert_eq!(vesper_cranelift_sys::live_handles(), before);
}

// ------------------------------------------------------------ lane shapes

/// `lane_type` of src/native_vector.rs (private, so mirrored here).
fn lane_type(e: Elem) -> ir::Type {
    match e {
        Elem::U8 => types::I8,
        Elem::U32 => types::I32,
        Elem::U64 | Elem::I64 => types::I64,
        Elem::F32 => types::F32,
        Elem::F64 => types::F64,
    }
}
/// `chunk_type`: the lane type by `CHUNK_BITS / bits` lanes.
fn chunk_type(e: Elem) -> ir::Type {
    lane_type(e).by(vector::CHUNK_BITS / e.bits()).unwrap()
}
/// `mask_type`: the integer chunk of the lane width.
fn mask_type(e: Elem) -> ir::Type {
    match e {
        Elem::F32 => chunk_type(Elem::U32),
        Elem::F64 => chunk_type(Elem::U64),
        e => chunk_type(e),
    }
}

/// Every intrinsic path of `Intrinsic::lookup` and some that are not.
const PATHS: &str = "Vec::splat Vec::from Vec::add Vec::sub Vec::mul Vec::div Vec::min Vec::max \
Vec::and Vec::or Vec::xor Vec::saturating_add Vec::saturating_sub Vec::not Vec::sqrt Vec::abs \
Vec::neg Vec::mul_add Vec::to_bits Vec::from_bits Vec::widen_f64 Vec::nearest_f32 Vec::load \
Vec::store Vec::store_masked Vec::shl Vec::shr Vec::eq Vec::ne Vec::lt Vec::le Vec::gt Vec::ge \
Vec::select Vec::reduce_add Vec::reduce_xor Vec::reduce_or Vec::reduce_and Vec::reduce_min \
Vec::reduce_max Vec::extract Vec::replace Vec::shuffle Vec::load_masked Mask::and Mask::or \
Mask::xor Mask::not Mask::count Mask::bitmask Mask::any Mask::all Mask::in_bounds Mask::active \
Isa::supported Vec::bogus Vec::add::x Vec:: Mask::splat Mask::load Isa::other Isa:: Foo::add \
Seq::add vec::add Vec add ::add Vec:add";

/// The Rust decisions, in `vector_decisions`'s format (shapes.ves).
fn rust_decisions() -> String {
    let mut o = String::new();
    for e in Elem::ALL {
        let per = Shape::new(e, (vector::CHUNK_BITS / e.bits()) as u16)
            .unwrap()
            .lanes_per_chunk();
        o += &format!(
            "elem {} {} {}{} {} {} lane {} chunk {} mask {} per {per}\n",
            e.name(),
            e.bits(),
            u8::from(e.is_float()),
            u8::from(e.is_signed()),
            e.bits_elem().name(),
            e.mask(),
            codes::ty_code(lane_type(e)),
            codes::ty_code(chunk_type(e)),
            codes::ty_code(mask_type(e)),
        );
        for n in 0..=64u16 {
            let Some(s) = Shape::new(e, n) else { continue };
            o += &format!(
                "shape {s} {} {} {} {} {}",
                s.bits(),
                s.bytes(),
                s.chunks(),
                vector::layout_words(&s.vec_type()).unwrap(),
                s.required_profile().name()
            );
            assert_eq!(
                vector::shape_of(&s.mask_type()),
                Some((vector::Kind::Mask, s))
            );
            for c in [Conv::ToBits, Conv::FromBits, Conv::Widen, Conv::Narrow] {
                let target = c.result(s).map_or("-", |t| t.elem.name());
                let accepts = if c.accepts(e) { "+" } else { "" };
                o += &format!(" {c:?}={target}{accepts}");
            }
            o.push('\n');
        }
    }
    for p in Profile::ALL {
        let le: String = Profile::ALL
            .iter()
            .map(|q| if p.le(*q) { '1' } else { '0' })
            .collect();
        let req = match p.cranelift_requirements() {
            None => "-".to_string(),
            Some((arch, [])) => arch.to_string(),
            Some((arch, flags)) => format!("{arch} {}", flags.join(",")),
        };
        o += &format!("profile {} {} {le} {req}\n", p.name(), p.fixed_bits());
    }
    for path in PATHS.split(' ') {
        o += &format!("intrinsic {path} ");
        match Intrinsic::lookup(path) {
            None => o += "none",
            Some(Err(())) => o += "unknown",
            Some(Ok(i)) => {
                o += &format!(
                    "{i:?} {}{}{}",
                    u8::from(i.takes_shape_arguments()),
                    u8::from(i.mask_result()),
                    u8::from(i.mask_operand())
                );
                for args in 0..4 {
                    let r = i.constant_positions(args);
                    o += &format!(" {}..{}", r.start, r.end);
                }
                for e in Elem::ALL {
                    if let Some(err) = i.lane_error(e) {
                        o += &format!(" | {}: {err}", e.name());
                    }
                }
            }
        }
        o.push('\n');
    }
    o
}

/// `shapes.ves` alone, compiled as a world (it needs no Cranelift handle).
fn vesper_decisions(optimize: bool) -> String {
    let path = repo().join("packages/vesper-codegen-numeric/src/shapes.ves");
    let source = std::fs::read_to_string(path).unwrap();
    let parsed = parser::parse(&source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let c = check::check(&parsed.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let f = native::WordFunction::compile(&c.world, "vector_decisions", optimize).unwrap();
    let input = PATHS.replace(' ', "\n");
    let words = [input.len() as u64, 0, input.as_ptr() as u64];
    // SAFETY: the entry takes one `in Str`, passed as the address of its
    // three words `{len, 0, data}`, which live across the call, as does
    // `input`; it returns a live `ChunkSeq[u64]` header `{len, cap, data}`.
    let packed = unsafe {
        let handle = f.call(&[words.as_ptr() as u64]).unwrap();
        let header = handle as *const u64;
        let len = *header as usize;
        let data = *header.add(2) as *const u64;
        let out = std::slice::from_raw_parts(data, len).to_vec();
        vesper::seq::native_runtime::vesper_seq_free(handle);
        out
    };
    let mut bytes: Vec<u8> = packed[1..].iter().flat_map(|w| w.to_le_bytes()).collect();
    bytes.truncate(packed[0] as usize);
    String::from_utf8(bytes).unwrap()
}

#[test]
fn lane_shape_decisions_match_the_rust_lowering() {
    let rust = rust_decisions();
    for optimize in [false, true] {
        let vesper = vesper_decisions(optimize);
        for (i, (r, v)) in rust.lines().zip(vesper.lines()).enumerate() {
            assert_eq!(v, r, "line {} (optimize={optimize})", i + 1);
        }
        assert_eq!(vesper.lines().count(), rust.lines().count());
        eprintln!(
            "lane shapes: {} decision lines equal (optimize={optimize})",
            rust.lines().count()
        );
    }
}

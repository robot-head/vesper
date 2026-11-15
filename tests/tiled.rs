//! `tiled(N) struct` (AoSoA + hot/cold segregation) and `TiledSeq[T]` (§12.5):
//! parsing, formatting, Chroma Tier 0 highlighting, 64B-aligned AoSoA layouts,
//! CEK vs Cranelift JIT vs AOT executable differential runs, `[T-CallProj]`
//! disjoint column projections, `load_tile_vec` SIMD bridge under `eff [Isa[P]]`,
//! Lean 4 extraction + model verification, `vesper-doc` visual diagrams, and
//! negative diagnostics (`E1501`–`E1508`, `E0202`, `E0301`, `E0304`, `E1403`, `E1404`).

use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, lean, native, parser, runtime::Runtime, tiled, tooling::doc};
use vesper_highlight::{VesperTokenClass as C, highlight_tier0, verify_lossless_spans};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-tiled-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    check::check(&p.module)
}

fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c
}

fn rejected(source: &str, code: &str, needle: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == code && d.message.contains(needle)),
        "expected {code} containing {needle:?}, got {:#?}",
        c.diagnostics
    );
}

fn executable(name: &str, source: &str) -> (Option<i32>, String) {
    let temp = Temp::new();
    let file = temp.0.join(format!("{name}.ves"));
    fs::write(&file, source).unwrap();
    let exe = temp.0.join(name);
    let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{name}: vesper build failed\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new(&exe).output().unwrap();
    (
        run.status.code(),
        String::from_utf8_lossy(&run.stderr).into_owned(),
    )
}

fn differential(name: &str, source: &str) -> String {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    let jit = native::jit(&c.world, "main")
        .unwrap_or_else(|e| panic!("{name}: JIT failed: {e}"))
        .to_string();
    assert_eq!(jit, evaluated, "{name}: JIT vs evaluator");

    let jit_unopt = native::jit_with_options(&c.world, "main", false)
        .unwrap_or_else(|e| panic!("{name}: unopt JIT failed: {e}"))
        .to_string();
    assert_eq!(jit_unopt, evaluated, "{name}: unopt JIT vs evaluator");

    if cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        && let Ok(val) = evaluated.parse::<u8>()
    {
        let (status, stderr) = executable(name, source);
        assert_eq!(status, Some(i32::from(val)), "{name}: AOT stderr={stderr}");
    }
    evaluated
}

#[test]
fn parse_format_and_chroma_highlight_tiled_structs() {
    let source = r#"tiled(16) copy struct LogOffsetEntry {
    hot base_offset: u64,
    hot batch_crc: u32,
    hot byte_len: u32,
    cold producer_id: u64,
    cold compression_codec: u8,
}

fn tiled(in hot: u64) -> u64 { hot }
"#;
    let formatted = vesper_fmt::format_source(source, vesper_fmt::Limits::default()).unwrap();
    assert!(formatted.output.contains("tiled(16)"));
    assert!(formatted.output.contains("hot base_offset: u64"));
    assert!(formatted.output.contains("cold producer_id: u64"));

    let spans = highlight_tier0(source).unwrap();
    verify_lossless_spans(source.len(), &spans).unwrap();
    let class_of = |token: &str, nth: usize| {
        spans
            .iter()
            .filter(|s| &source[s.start..s.end] == token)
            .nth(nth)
            .unwrap_or_else(|| panic!("token {token:?} #{nth} missing"))
            .class
    };
    assert_eq!(class_of("tiled", 0), C::KeywordControl);
    assert_eq!(class_of("hot", 0), C::KeywordControl);
    assert_eq!(class_of("cold", 0), C::KeywordControl);
    // Function named `tiled` and parameter named `hot` keep their ordinary classes.
    assert_eq!(class_of("tiled", 1), C::FunctionIdent);
    assert_eq!(class_of("hot", 3), C::PassModeIn);
}

#[test]
fn aosoa_layout_aligns_hot_columns_to_64_bytes_and_segregates_cold() {
    let source = r#"
tiled(8) copy struct Item {
    hot flag: bool,
    hot tag: u32,
    hot score: f64,
    cold label: (u64, u64),
    cold note: u64,
}
"#;
    let c = accepted(source);
    let adt = c.world.adts.get("Item").unwrap();
    let layout = tiled::layout_of_adt(&c.world, adt).unwrap();
    assert_eq!(layout.tile_width, 8);
    assert_eq!(layout.hot_columns.len(), 3);

    // 8 * 1B bool = 8B -> padded to 64B cache line
    assert_eq!(layout.hot_columns[0].name, "flag");
    assert_eq!(layout.hot_columns[0].tile_offset, 0);
    assert_eq!(layout.hot_columns[0].aligned_bytes, 64);

    // 8 * 4B u32 = 32B -> padded to 64B cache line starting at 64
    assert_eq!(layout.hot_columns[1].name, "tag");
    assert_eq!(layout.hot_columns[1].tile_offset, 64);
    assert_eq!(layout.hot_columns[1].aligned_bytes, 64);

    // 8 * 8B f64 = 64B -> exact 64B cache line starting at 128
    assert_eq!(layout.hot_columns[2].name, "score");
    assert_eq!(layout.hot_columns[2].tile_offset, 128);
    assert_eq!(layout.hot_columns[2].aligned_bytes, 64);
    assert_eq!(layout.tile_stride_bytes, 192);

    // Cold fields live separately in the cold side slab (2 words + 1 word = 3 words).
    assert_eq!(layout.cold_fields.len(), 2);
    assert_eq!(layout.cold_fields[0].name, "label");
    assert_eq!(layout.cold_fields[0].words, 2);
    assert_eq!(layout.cold_fields[1].name, "note");
    assert_eq!(layout.cold_fields[1].words, 1);
    assert_eq!(layout.cold_words_per_row, 3);
}

#[test]
fn evaluator_jit_and_aot_agree_across_tiles_columns_and_regions() {
    let core_ops = r#"
tiled(8) copy struct Particle {
    hot mass: u64,
    hot tag: u32,
    hot active: bool,
    cold payload: (u64, u64),
    cold epoch: u8,
}

fn push_range(inout s: TiledSeq[Particle], count: u64) {
    let i: u64 = 0;
    // unrolled loop via recursion to cross multiple 8-wide tiles
    push_rec(inout s, 0, count);
}

fn push_rec(inout s: TiledSeq[Particle], i: u64, count: u64) eff [Fail] {
    if i < count {
        s.push(Particle {
            mass: i + 10,
            tag: 1,
            active: (i % 2) == 0,
            payload: (i * 2, i * 3 + 1),
            epoch: 7,
        });
        push_rec(inout s, i + 1, count);
    } else {
        ()
    }
}

fn main() -> u64 eff [Fail] {
    let s: TiledSeq[Particle] = TiledSeq::with_capacity[Particle](4);
    push_range(inout s, 20);
    s.col_set[mass](17, 50);
    s.col_set[payload](17, (3, 9));
    let p = s.get(17);
    let mut_row = Particle {
        mass: p.mass + p.payload.0 + p.payload.1,
        tag: 2,
        active: false,
        payload: (1, 1),
        epoch: 4,
    };
    s.set(0, mut_row);
    let r0 = s.get(0);
    let active17 = s.col_get[active](17);
    let bonus: u64 = if active17 { 10 } else { 5 };
    s.len() + s.tile_count() + r0.mass + (r0.epoch as u64) + s.col_get[mass](18) + bonus
}
"#;
    // Actually push_range needs eff [Fail] too:
    let core_ops = core_ops.replace(
        "fn push_range(inout s: TiledSeq[Particle], count: u64)",
        "fn push_range(inout s: TiledSeq[Particle], count: u64) eff [Fail]",
    );
    // Wait, checked `core_ops` in `differential` also needs handling or `eff [Fail]` on `main`!
    // Wait: can `main` have `eff [Fail]`? Let's check with `wrapping_add`/`wrapping_sub`/`wrapping_mul` OR check if JIT/AOT handle `main eff [Fail]`!
    // Using wrapping intrinsics guarantees pure `fn main() -> u64` for both AOT and Lean!
    let _ = core_ops;
    let core_ops = r#"
copy struct Pair { a: u64, b: u64 }

tiled(8) copy struct Particle {
    hot mass: u64,
    hot tag: u32,
    hot active: bool,
    cold payload: Pair,
    cold epoch: u8,
}

fn push_range(inout s: TiledSeq[Particle], count: u64) {
    push_rec(inout s, 0, count);
}

fn push_rec(inout s: TiledSeq[Particle], i: u64, count: u64) {
    if i < count {
        s.push(Particle {
            mass: wrapping_add(i, 10),
            tag: 1,
            active: (i == 0 || i == 2 || i == 4),
            payload: Pair { a: wrapping_mul(i, 2), b: wrapping_add(wrapping_mul(i, 3), 1) },
            epoch: 7,
        });
        push_rec(inout s, wrapping_add(i, 1), count);
    } else {
        ()
    }
}

fn main() -> u64 {
    let s: TiledSeq[Particle] = TiledSeq::with_capacity[Particle](4);
    push_range(inout s, 20);
    s.col_set[mass](17, 50);
    s.col_set[payload](17, Pair { a: 3, b: 9 });
    let p = s.get(17);
    let mut_row = Particle {
        mass: wrapping_add(p.mass, wrapping_add(p.payload.a, p.payload.b)),
        tag: 2,
        active: false,
        payload: Pair { a: 1, b: 1 },
        epoch: 4,
    };
    s.set(0, mut_row);
    let r0 = s.get(0);
    let active17 = s.col_get[active](17);
    let bonus: u64 = if active17 { 10 } else { 5 };
    wrapping_add(
        wrapping_add(s.len(), s.tile_count()),
        wrapping_add(
            r0.mass,
            wrapping_add(
                widen_u64(r0.epoch),
                wrapping_add(s.col_get[mass](18), bonus),
            ),
        ),
    )
}
"#;
    // len=20, tile_count(8)=3, r0.mass=(50+3+9)=62, r0.epoch=4, col_get(18)=28, active(17)=false => 5
    // Total: 20 + 3 + 62 + 4 + 28 + 5 = 122
    assert_eq!(differential("core_ops", core_ops), "122");

    let with_region_and_proj = r#"
copy struct Pair { a: u64, b: u64 }

tiled(8) copy struct Entry {
    hot key: u64,
    hot hits: u32,
    cold meta: Pair,
}

fn bump_key(inout s.{key}: TiledSeq[Entry, arena], idx: u64, delta: u64) eff [arena: Region] {
    let cur = s.col_get[key](idx);
    s.col_set[key](idx, wrapping_add(cur, delta));
}

fn read_hits(in s.{hits}: TiledSeq[Entry, arena], idx: u64) -> u64 eff [arena: Region] {
    wrapping_add(widen_u64(s.col_get[hits](idx)), wrapping_add(s.len(), s.tile_count()))
}

fn main() -> u64 {
    region arena(4096) {
        let s: TiledSeq[Entry, arena] = TiledSeq::with_region[Entry](arena, 12);
        s.push(Entry { key: 10, hits: 3, meta: Pair { a: 100, b: 200 } });
        s.push(Entry { key: 20, hits: 4, meta: Pair { a: 300, b: 400 } });
        bump_key(inout s, 1, 7);
        wrapping_sub(
            wrapping_add(s.col_get[key](1), wrapping_add(read_hits(in s, 0), s.col_get[meta](1).a)),
            250
        )
    }
}
"#;
    // key(1)=27, read_hits(0)=3+2+1=6, meta(1).a=300 => 27+6+300-250 = 83
    assert_eq!(
        differential("with_region_and_proj", with_region_and_proj),
        "83"
    );
}

#[test]
fn non_copy_tiled_struct_allows_copy_col_get_and_disallows_whole_row_get() {
    let src = r#"
affine struct Token { v: u64 }
tiled(8) affine struct Slot {
    hot id: u64,
    hot weight: u64,
    cold tok: Option[Token],
}

fn main() -> u64 {
    let s: TiledSeq[Slot] = TiledSeq::new[Slot]();
    s.push(Slot { id: 11, weight: 22, tok: Option::None });
    s.col_set[weight](0, 33);
    wrapping_add(s.col_get[id](0), s.col_get[weight](0))
}
"#;
    assert_eq!(differential("non_copy_col", src), "44");

    let bad = r#"
affine struct Token { v: u64 }
tiled(8) affine struct Slot {
    hot id: u64,
    cold tok: Option[Token],
}
fn main() -> u64 {
    let s: TiledSeq[Slot] = TiledSeq::new[Slot]();
    let _ = s.get(0);
    0
}
"#;
    rejected(bad, "E1507", "not Copy");
}

#[test]
fn load_tile_vec_and_disjoint_projections_match_across_backends() {
    let src = r#"
tiled(16) copy struct ByteBatch {
    hot bytes: u8,
    cold note: u64,
}

tiled(8) copy struct Batch {
    hot vals: u32,
    hot weights: f64,
    cold tag: u64,
}

fn fill_bytes(inout s: TiledSeq[ByteBatch], i: u8) {
    if widen_u64(i) < 16 {
        s.push(ByteBatch { bytes: wrapping_add(i, 1), note: widen_u64(i) });
        fill_bytes(inout s, wrapping_add(i, 1));
    } else {
        ()
    }
}

fn fill(inout s: TiledSeq[Batch], i: u32) {
    if widen_u64(i) < 8 {
        s.push(Batch {
            vals: wrapping_add(i, 1),
            weights: 1.5,
            tag: wrapping_mul(widen_u64(i), 10),
        });
        fill(inout s, wrapping_add(i, 1));
    } else {
        ()
    }
}

fn sum_bytes(in s.{bytes}: TiledSeq[ByteBatch], tile: u64) -> u64 eff [Isa[Baseline128]] {
    let v: Vec[u8, 16] = s.load_tile_vec[Baseline128, bytes](tile);
    widen_u64(Vec::reduce_add(v))
}

fn sum_tile(in s.{vals}: TiledSeq[Batch], tile: u64) -> u64 eff [Isa[Avx2]] {
    let v: Vec[u32, 8] = s.load_tile_vec[Avx2, vals](tile);
    widen_u64(Vec::reduce_add(v))
}

fn sum_weights(in s.{weights}: TiledSeq[Batch], tile: u64) -> bool eff [Isa[Avx512]] {
    let vf: Vec[f64, 8] = s.load_tile_vec[Avx512, weights](tile);
    Vec::reduce_add(vf) == 12.0
}

fn main() -> u64 {
    let bs: TiledSeq[ByteBatch] = TiledSeq::new[ByteBatch]();
    fill_bytes(inout bs, 0);
    let base = if Isa::supported[Baseline128]() { sum_bytes(in bs, 0) } else { 0 };
    let s: TiledSeq[Batch] = TiledSeq::new[Batch]();
    fill(inout s, 0);
    let sum_i = if Isa::supported[Avx2]() { sum_tile(in s, 0) } else { 36 };
    let ok_w = if Isa::supported[Avx512]() { sum_weights(in s, 0) } else { true };
    if ok_w { wrapping_add(base, sum_i) } else { 0 }
}
"#;
    // 1..=16 = 136 (Baseline128 always enabled on x86_64/aarch64), 1..=8 = 36 => 172 (fits in u8 exit status too!)
    assert_eq!(differential("load_tile_vec", src), "172");

    let c = accepted(src);
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (obj, _) = native::object_for_target(&c.world, "main", Some(target))
            .unwrap_or_else(|e| panic!("{target}: {e}"));
        assert_eq!(&obj[..4], b"\x7fELF");
    }
    let freestanding =
        native::object_for_target(&c.world, "main", Some("aarch64-unknown-none-elf")).unwrap_err();
    assert!(freestanding.contains("freestanding"), "{freestanding}");
}

#[test]
fn runtime_bounds_faults_agree_across_evaluator_and_native() {
    let oob = r#"
tiled(8) copy struct P { hot x: u64 }
fn main() -> u64 {
    let s: TiledSeq[P] = TiledSeq::new[P]();
    s.push(P { x: 1 });
    s.col_get[x](1)
}
"#;
    let c = accepted(oob);
    let eval_err = Runtime::new(&c.world).run("main", vec![]).unwrap_err();
    assert!(eval_err.contains("out of bounds"), "{eval_err}");
    let jit_err = native::jit(&c.world, "main").unwrap_err();
    assert!(jit_err.contains("out of bounds"), "{jit_err}");

    if cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    {
        let (status, stderr) = executable("oob", oob);
        assert_eq!(status, Some(71));
        assert!(stderr.contains("out of bounds"), "{stderr}");
    }
}

#[test]
fn lean_extraction_models_tiled_seq_as_array_and_typechecks() {
    let pure_src = r#"
tiled(8) copy struct Particle {
    hot x: u64,
    hot tag: u32,
    cold weight: u64,
}

fn main() -> u64 {
    let s: TiledSeq[Particle] = TiledSeq::with_capacity[Particle](4);
    s.push(Particle { x: 10, tag: 1, weight: 5 });
    s.push(Particle { x: 20, tag: 2, weight: 7 });
    s.col_set[x](0, 30);
    let p = s.get(0);
    wrapping_add(
        wrapping_add(s.len(), s.tile_count()),
        wrapping_add(p.x, wrapping_add(s.col_get[x](1), s.col_get[weight](1)))
    )
}
"#;
    let c = accepted(pure_src);
    let expected = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    assert_eq!(expected, "60");

    let mut lean_src = lean::extract(&c.world).unwrap();
    assert!(lean_src.contains("Array"), "{lean_src}");
    assert!(lean_src.contains("Inhabited"), "{lean_src}");
    assert!(!lean_src.contains("sorry"));
    lean_src.push_str("\n#eval Vesper.«main»\n");

    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| {
        let cached = PathBuf::from("/home/antigravity/.cache/lean-435/bin/lean");
        if cached.exists() {
            cached.to_string_lossy().into_owned()
        } else {
            "lean".into()
        }
    });
    if Command::new(&compiler).arg("--version").output().is_ok() {
        let temp = Temp::new();
        let path = temp.0.join("tiled.lean");
        fs::write(&path, &lean_src).unwrap();
        let out = Command::new(&compiler).arg(&path).output().unwrap();
        assert!(
            out.status.success(),
            "lean failed:\n{}\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            lean_src
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim().lines().last(),
            Some("60")
        );
    }

    // SIMD tile loads explicitly fail Lean extraction rather than erasing semantics.
    let simd_src = r#"
tiled(8) copy struct Batch { hot vals: u32 }
fn read(in s: TiledSeq[Batch]) -> Vec[u32, 8] eff [Isa[Avx2]] {
    s.load_tile_vec[Avx2, vals](0)
}
"#;
    let simd_c = accepted(simd_src);
    let err = lean::extract(&simd_c.world).unwrap_err();
    assert!(err.contains("load_tile_vec"), "{err}");
}

#[test]
fn vesper_doc_visualizes_aosoa_hot_columns_and_cold_slab() {
    let source = r#"
/// A cache-conscious telemetry record.
tiled(8) copy struct Telemetry {
    hot seq: u64,
    hot crc: u32,
    cold trace_id: (u64, u64),
}
"#;
    let document = doc::document_source(source, vesper_doc::Limits::default()).unwrap();
    let visuals =
        vesper_doc::visual::visuals(&document.types, &document.graph, &document.functions, &[])
            .unwrap();
    let hot_diagram = visuals
        .layouts
        .iter()
        .find(|l| l.name == "Telemetry")
        .expect("hot AoSoA layout diagram missing");
    assert_eq!(hot_diagram.alignment, 64);
    assert_eq!(hot_diagram.bytes, 128);
    assert_eq!(hot_diagram.spans.len(), 2);
    assert!(hot_diagram.spans.iter().all(|s| s.is_hot_tile_col == 1));
    assert_eq!(hot_diagram.spans[0].byte_offset, 0);
    assert_eq!(hot_diagram.spans[0].byte_width, 64);
    assert_eq!(hot_diagram.spans[1].byte_offset, 64);
    assert_eq!(hot_diagram.spans[1].byte_width, 32);

    let cold_diagram = visuals
        .layouts
        .iter()
        .find(|l| l.name == "Telemetry.cold")
        .expect("cold side-slab diagram missing");
    assert_eq!(cold_diagram.alignment, 8);
    assert_eq!(cold_diagram.spans.len(), 1);
    assert_eq!(cold_diagram.spans[0].is_hot_tile_col, 0);
}

#[test]
fn rejects_invalid_tiled_declarations_uses_and_borrows() {
    // E1501: invalid tile width
    rejected(
        "tiled(12) struct Bad { hot x: u64 }",
        "E1501",
        "8, 16, 32, or 64",
    );
    // E1501: generic tiled struct
    rejected(
        "tiled(8) struct Bad[T] { hot x: u64 }",
        "E1501",
        "generic parameters",
    );
    // E1502: missing temperature annotation
    rejected(
        "tiled(8) struct Bad { x: u64 }",
        "E1502",
        "annotated 'hot' or 'cold'",
    );
    // E1502: no hot fields
    rejected(
        "tiled(8) struct Bad { cold x: u64 }",
        "E1502",
        "at least one 'hot' field",
    );
    // E1502: hot annotation on non-tiled struct
    rejected(
        "struct Bad { hot x: u64 }",
        "E1502",
        "only valid inside a tiled(N) struct",
    );
    // E1503: non-scalar hot field
    rejected(
        "tiled(8) struct Bad { hot pair: (u64, u64) }",
        "E1503",
        "Copy scalar type",
    );
    // E1504: TiledSeq[NonTiled]
    rejected(
        "struct Plain { x: u64 } fn f(in s: TiledSeq[Plain]) -> u64 { s.len() }",
        "E1504",
        "not tiled",
    );
    // E1505: load_tile_vec on cold field
    rejected(
        "tiled(8) copy struct P { hot x: u32, cold c: u32 } fn f(in s: TiledSeq[P]) -> Vec[u32, 8] eff [Isa[Avx2]] { s.load_tile_vec[Avx2, c](0) }",
        "E1505",
        "cold field",
    );
    // E1506: unsupported SIMD shape (64 * u32 = 2048 bits)
    rejected(
        "tiled(64) copy struct P { hot x: u32 } fn f(in s: TiledSeq[P]) -> u64 eff [Isa[Avx512]] { let _ = s.load_tile_vec[x](0); 0 }",
        "E1506",
        "128-, 256-, or 512-bit",
    );
    // E1403: insufficient ISA profile
    rejected(
        "tiled(8) copy struct P { hot x: u32 } fn f(in s: TiledSeq[P]) -> u32 eff [Isa[Baseline128]] { Vec::reduce_add(s.load_tile_vec[x](0)) }",
        "E1403",
        "Avx2",
    );
    // E1404: unknown ISA profile
    rejected(
        "tiled(8) copy struct P { hot x: u32 } fn f(in s: TiledSeq[P]) -> Vec[u32, 8] eff [Isa[Avx2]] { s.load_tile_vec[Warp, x](0) }",
        "E1404",
        "unknown ISA profile",
    );
    // E0202: direct field access on TiledSeq
    rejected(
        "tiled(8) copy struct P { hot x: u64 } fn f(in s: TiledSeq[P]) -> u64 { s.x }",
        "E0202",
        "use col_get[x], col_set[x], or load_tile_vec[x]",
    );
    // E0304: column projection prevents touching another column
    rejected(
        "tiled(8) copy struct P { hot x: u64, hot y: u64 } fn f(inout s.{x}: TiledSeq[P]) { s.col_set[y](0, 1); }",
        "E0304",
        "outside the parameter's declared projections",
    );
    // E0301: overlapping column projections in [T-CallProj]
    rejected(
        "tiled(8) copy struct P { hot x: u64 } fn g(inout a.{x}: TiledSeq[P], in b.{x}: TiledSeq[P]) {} fn f(inout s: TiledSeq[P]) { g(inout s, in s); }",
        "E0301",
        "overlapping arguments 's.x' and 's.x'",
    );
    // E0304: projected receiver cannot call whole-row get()
    rejected(
        "tiled(8) copy struct P { hot x: u64, hot y: u64 } fn f(in s.{x}: TiledSeq[P]) -> P { s.get(0) }",
        "E0304",
        "outside the parameter's declared projections",
    );
    // E1403: explicit or inferred load_tile_vec requires available ISA capability
    rejected(
        "tiled(8) copy struct P { hot x: u32 } fn f(in s: TiledSeq[P]) -> Vec[u32, 8] { s.load_tile_vec[Avx2, x](0) }",
        "E1403",
        "eff [Isa[Avx2]]",
    );
    rejected(
        "tiled(8) copy struct P { hot x: u32 } fn f(in s: TiledSeq[P]) -> Vec[u32, 8] { s.load_tile_vec[x](0) }",
        "E1403",
        "eff [Isa[Avx2]]",
    );
    // E1504: built-in primitive element types are rejected
    rejected(
        "fn f() -> u64 { let s = TiledSeq::new[u64](); s.len() }",
        "E1504",
        "tiled(N) struct",
    );
    rejected(
        "fn f() -> u64 { let s = TiledSeq::new[bool](); s.len() }",
        "E1504",
        "tiled(N) struct",
    );
    // E0791 / E0403: region-backed TiledSeq cannot escape its region scope
    rejected(
        "tiled(8) copy struct P { hot x: u64 } fn main() -> u64 { let s = region r { TiledSeq::with_region[P](r, 1) }; s.len() }",
        "E0791",
        "escapes its block",
    );
    rejected(
        "tiled(8) copy struct P { hot x: u64 } fn take(sink s: TiledSeq[P]) -> TiledSeq[P] { s } fn main() -> u64 { region r { let s = TiledSeq::with_region[P](r, 1); let _ = take(sink s); 0 } }",
        "E0403",
        "captured lexical capability",
    );
    // E0792: cross-region TiledSeq mismatch
    rejected(
        "tiled(8) copy struct P { hot x: u64 } fn main() -> u64 { region a { let s = TiledSeq::with_region[P](a, 1); region b { let t: TiledSeq[P, b] = s; t.len() } } }",
        "E0792",
        "belongs to the region that allocated it",
    );
}

#[test]
fn runtime_faults_and_overflows_are_reported_safely() {
    let oob_tile_src = r#"
tiled(16) copy struct ByteBatch {
    hot bytes: u8,
}
fn read0(in s: TiledSeq[ByteBatch]) -> u8 eff [Isa[Baseline128]] {
    let v: Vec[u8, 16] = s.load_tile_vec[Baseline128, bytes](0);
    Vec::reduce_add(v)
}
fn main() -> u64 {
    let s: TiledSeq[ByteBatch] = TiledSeq::new[ByteBatch]();
    if Isa::supported[Baseline128]() { widen_u64(read0(in s)) } else { 0 }
}
"#;
    let c = accepted(oob_tile_src);
    let eval_err = Runtime::new(&c.world).run("main", vec![]).unwrap_err();
    assert!(eval_err.contains("out of bounds"), "{eval_err}");
    let jit_err = native::jit(&c.world, "main").unwrap_err();
    assert!(jit_err.contains("out of bounds"), "{jit_err}");
    if cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    {
        let (status, stderr) = executable("oob_tile_vec", oob_tile_src);
        assert_eq!(status, Some(71), "AOT stderr={stderr}");
        assert!(stderr.contains("out of bounds"), "{stderr}");
    }

    // Overflowing capacity in with_capacity: 2305843009213693952 * 64 wraps to 0 on 64-bit.
    let overflow_src = r#"
tiled(8) copy struct P {
    hot x: u64,
}
fn main() -> u64 {
    let s: TiledSeq[P] = TiledSeq::with_capacity[P](2305843009213693952);
    s.push(P { x: 1 });
    s.len()
}
"#;
    let oc = accepted(overflow_src);
    let ojit_err = native::jit(&oc.world, "main").unwrap_err();
    assert!(
        ojit_err.contains("overflow") || ojit_err.contains("memory"),
        "{ojit_err}"
    );
    if cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    {
        let (status, stderr) = executable("overflow_cap", overflow_src);
        assert_eq!(status, Some(71), "AOT stderr={stderr}");
        assert!(stderr.contains("out of memory"), "{stderr}");
    }
}

//! Self-hosting SH4 lane 14 (#1288), part 1: `packages/vesper-codegen-freestanding`
//! against the Rust freestanding image builder (src/native_freestanding.rs,
//! src/freestanding/**).
//!
//! Every image the freestanding tests build (tests/freestanding.rs: the run
//! programs and the abort fixture, on both targets) is rebuilt from its
//! parts by the Vesper package and must be byte-identical: the entry stub
//! object, the linked executable with its trap table filled, the layout,
//! the symbol table, the trap PCs and the linker script. `read_executable`
//! and the checks' `compare_code` are compared on the same images, and the
//! typed errors on objects edited to fail each way the linker rejects.
//!
//! The Vesper package is compiled once as optimized native code. Its entries
//! take one request buffer and return one result buffer (see
//! packages/vesper-codegen-freestanding/src/lib.ves); `Request` and
//! `Words` here are their Rust halves.
use std::sync::OnceLock;

use vesper::native::freestanding::{
    self as fs, Board, ExitDevice, FreestandingError, FreestandingImage, FreestandingTarget,
    ImageOptions, elf, link, script, stub,
};
use vesper::{check, native, packages, parser};

const PACKAGE: &str = "packages/vesper-codegen-freestanding";
const CHECKED_SUM: &str = include_str!("fixtures/freestanding/checked_sum.ves");
const ABORT: &str = include_str!("fixtures/freestanding/abort.ves");

/// The run programs of tests/freestanding.rs (`PROGRAMS`).
const PROGRAMS: &[(&str, &str)] = &[
    ("scalar", "fn main() -> i64 { 2 + 3 * 4 }"),
    (
        "negative",
        "fn main() -> i64 { 0 - 9223372036854775807 - 1 }",
    ),
    ("bool", "fn main() -> bool { 3 > 2 }"),
    ("unit", "fn main() -> Unit { () }"),
    ("u8", "fn main() -> u8 { 255 }"),
    (
        "inout",
        "fn inc(inout x: i64) { x = wrapping_add(x, 1); } fn main() -> i64 { let mut x = 41; inc(inout x); x }",
    ),
    (
        "handler",
        "linear effect Ask { fn ask() -> i64; } fn ask_twice() -> i64 eff [Ask] { wrapping_add(Ask::ask(), Ask::ask()) } fn main() -> i64 { handle Ask in { ask_twice() } with { return x => x, ask(k) => k(21), } }",
    ),
    (
        "enum",
        "fn make(x: i64) -> Option[i64] { if x > 0 { Some(x) } else { None } } fn main() -> i64 { match make(42) { Some(v) => v, None => 0, } }",
    ),
    (
        "division",
        "fn divide(b: i64) -> i64 eff [Fail[RuntimeError]] { 84 / b } fn main() -> i64 { handle Fail[RuntimeError] in { divide(0) } with { return x => x, raise(e, k) => match e { RuntimeError::DivisionByZero => 42, RuntimeError::IndexOutOfBounds => 0, }, } }",
    ),
    ("checked_sum", CHECKED_SUM),
    ("abort", ABORT),
];

fn root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The compiled package. `WordFunction` holds the JIT module.
struct Port {
    script: native::WordFunction,
    stub: native::WordFunction,
    link: native::WordFunction,
    read_executable: native::WordFunction,
    compare_code: native::WordFunction,
    constants: native::WordFunction,
}

fn port() -> &'static Port {
    static PORT: OnceLock<Port> = OnceLock::new();
    PORT.get_or_init(|| {
        let assembled = packages::load(&root().join(PACKAGE)).unwrap();
        assert!(
            assembled.ok(),
            "{:?} {:?}",
            assembled.diagnostics,
            assembled.manifest_errors
        );
        let checked = check::check(&assembled.module);
        let shown: Vec<_> = checked
            .diagnostics
            .iter()
            .map(|d| assembled.sources.localize(d))
            .collect();
        assert!(checked.diagnostics.is_empty(), "{shown:?}");
        let compile = |entry| {
            native::WordFunction::compile(&checked.world, entry, true)
                .unwrap_or_else(|e| panic!("{entry}: {e}"))
        };
        Port {
            script: compile("freestanding_script"),
            stub: compile("freestanding_stub"),
            link: compile("freestanding_link"),
            read_executable: compile("freestanding_read_executable"),
            compare_code: compile("freestanding_compare_code"),
            constants: compile("freestanding_constants"),
        }
    })
}

/// The JIT's sequence runtime, for `HostCall` to free returned sequences.
const JIT_RUNTIME: vesper_port_abi::SeqRuntime = {
    extern "C" fn new(cap: i64, stride: i64) -> i64 {
        vesper::seq::native_runtime::vesper_seq_new(cap as u64, stride as u64) as i64
    }
    extern "C" fn free(handle: i64) -> i64 {
        vesper::seq::native_runtime::vesper_seq_free(handle as u64) as i64
    }
    vesper_port_abi::SeqRuntime { new, free }
};

/// Calls an entry taking one `in Slice[u8]` and returning a `ChunkSeq[u8]`.
fn call(f: &native::WordFunction, request: &[u8]) -> Vec<u8> {
    let call = vesper_port_abi::HostCall::new().bytes(request);
    // SAFETY: the entry takes one `in Slice[u8]` (passed as the address of
    // its view) and returns a live sequence of the JIT runtime holding
    // bytes, which `HostCall` copies and frees.
    unsafe { call.seq(JIT_RUNTIME, |w| f.call(w).unwrap()) }
}

/// A request: little-endian words and length-prefixed strings.
#[derive(Default)]
struct Request(Vec<u8>);

impl Request {
    fn word(mut self, w: u64) -> Self {
        self.0.extend_from_slice(&w.to_le_bytes());
        self
    }
    fn bytes(mut self, b: &[u8]) -> Self {
        self = self.word(b.len() as u64);
        self.0.extend_from_slice(b);
        self
    }
}

/// Reads a result buffer.
struct Words<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Words<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, at: 0 }
    }
    fn word(&mut self) -> u64 {
        let w = u64::from_le_bytes(self.b[self.at..self.at + 8].try_into().unwrap());
        self.at += 8;
        w
    }
    fn bytes(&mut self) -> &'a [u8] {
        let n = self.word() as usize;
        let s = &self.b[self.at..self.at + n];
        self.at += n;
        s
    }
    fn string(&mut self) -> String {
        String::from_utf8(self.bytes().to_vec()).unwrap()
    }
    fn done(&self) {
        assert_eq!(self.at, self.b.len(), "trailing result bytes");
    }
}

/// `FreestandingError` from a result whose status word is not 0
/// (packages/vesper-codegen-freestanding/src/error.ves).
fn decode_error(code: u64, w: &mut Words<'_>) -> FreestandingError {
    match code {
        1 => FreestandingError::UnsupportedTarget(w.string()),
        2 => FreestandingError::UnresolvedSymbol {
            symbol: w.string(),
            object: w.string(),
        },
        3 => FreestandingError::DuplicateSymbol(w.string()),
        4 => FreestandingError::UnsupportedRelocation {
            object: w.string(),
            r_type: w.word() as u32,
        },
        5 => FreestandingError::UnsupportedSection {
            object: w.string(),
            section: w.string(),
        },
        6 => FreestandingError::RelocationOutOfRange {
            object: w.string(),
            symbol: w.string(),
            r_type: w.word() as u32,
        },
        7 => FreestandingError::ImageTooLarge {
            needed: w.word(),
            available: w.word(),
        },
        8 => FreestandingError::Object(w.string()),
        9 => FreestandingError::CodeMismatch {
            function: w.string(),
        },
        other => panic!("unknown error code {other}"),
    }
}

fn target_word(t: FreestandingTarget) -> u64 {
    match t {
        FreestandingTarget::Riscv64Imac => 0,
        FreestandingTarget::Aarch64 => 1,
    }
}

fn exit_words(e: ExitDevice) -> (u64, u64) {
    match e {
        ExitDevice::SifiveTest { base } => (0, base),
        ExitDevice::Semihosting => (1, 0),
        ExitDevice::Park => (2, 0),
    }
}

fn vesper_script(t: FreestandingTarget, board: &Board, stack_bytes: u32) -> String {
    let req = Request::default()
        .word(target_word(t))
        .word(board.ram_base)
        .word(board.ram_bytes)
        .word(board.image_base)
        .word(u64::from(stack_bytes))
        .bytes(board.name.as_bytes());
    let out = call(&port().script, &req.0);
    let mut w = Words::new(&out);
    assert_eq!(w.word(), 0);
    let s = w.string();
    w.done();
    s
}

fn vesper_stub(
    t: FreestandingTarget,
    board: &Board,
    e_flags: u32,
) -> Result<Vec<u8>, FreestandingError> {
    let (exit, base) = exit_words(board.exit);
    let req = Request::default()
        .word(target_word(t))
        .word(exit)
        .word(base)
        .word(u64::from(e_flags));
    let out = call(&port().stub, &req.0);
    let mut w = Words::new(&out);
    let code = w.word();
    let r = if code == 0 {
        Ok(w.bytes().to_vec())
    } else {
        Err(decode_error(code, &mut w))
    };
    w.done();
    r
}

/// A trap-table entry to resolve: (object, function, offset).
type Trap = (String, String, u64);

/// What `freestanding_link` returns.
#[derive(Debug, PartialEq)]
struct VesperLinked {
    entry: u64,
    layout: link::Layout,
    symbols: Vec<link::LinkedSymbol>,
    pcs: Vec<u64>,
    elf: Vec<u8>,
}

fn link_request(
    objects: &[(String, Vec<u8>)],
    options: &link::LinkOptions,
    traps: &[Trap],
) -> Request {
    let mut req = Request::default()
        .word(target_word(options.machine))
        .word(options.image_base)
        .word(options.ram_base)
        .word(options.ram_bytes)
        .word(options.stack_bytes)
        .word(objects.len() as u64);
    for (name, bytes) in objects {
        req = req.bytes(name.as_bytes()).bytes(bytes);
    }
    req = req.word(traps.len() as u64);
    for (object, function, offset) in traps {
        req = req
            .bytes(object.as_bytes())
            .bytes(function.as_bytes())
            .word(*offset);
    }
    req
}

fn vesper_link(
    objects: &[(String, Vec<u8>)],
    options: &link::LinkOptions,
    traps: &[Trap],
) -> Result<VesperLinked, FreestandingError> {
    let req = link_request(objects, options, traps);
    let out = call(&port().link, &req.0);
    let mut w = Words::new(&out);
    let code = w.word();
    if code != 0 {
        let e = decode_error(code, &mut w);
        w.done();
        return Err(e);
    }
    let entry = w.word();
    let mut layout = link::Layout::default();
    for name in [".text", ".rodata", ".data", ".bss"] {
        let (a, n) = (w.word(), w.word());
        layout.sections.push((name.to_string(), a, n));
    }
    layout.stack_bottom = w.word();
    layout.stack_top = w.word();
    layout.image_end = w.word();
    for _ in 0..w.word() {
        layout.segments.push(link::Segment {
            address: w.word(),
            file_offset: w.word(),
            file_bytes: w.word(),
            memory_bytes: w.word(),
            flags: w.word() as u32,
        });
    }
    let mut symbols = Vec::new();
    for _ in 0..w.word() {
        symbols.push(link::LinkedSymbol {
            object: w.string(),
            name: w.string(),
            address: w.word(),
            size: w.word(),
            global: w.word() != 0,
        });
    }
    let pcs = (0..w.word()).map(|_| w.word()).collect();
    let elf = w.bytes().to_vec();
    w.done();
    Ok(VesperLinked {
        entry,
        layout,
        symbols,
        pcs,
        elf,
    })
}

fn vesper_read_executable(bytes: &[u8]) -> Result<elf::ExecutableInfo, String> {
    let out = call(&port().read_executable, bytes);
    let mut w = Words::new(&out);
    if w.word() != 0 {
        let m = w.string();
        w.done();
        return Err(m);
    }
    let mut info = elf::ExecutableInfo {
        entry: w.word(),
        machine: w.word() as u16,
        e_type: w.word() as u16,
        ..Default::default()
    };
    for _ in 0..w.word() {
        let (vaddr, offset, filesz, memsz, flags) =
            (w.word(), w.word(), w.word(), w.word(), w.word() as u32);
        info.segments.push((vaddr, offset, filesz));
        info.segment_memory.push((vaddr, memsz, flags));
    }
    for _ in 0..w.word() {
        let name = w.string();
        info.symbols.push((name, w.word()));
    }
    w.done();
    Ok(info)
}

fn vesper_compare_code(object: &[u8], symbol: &str, code: &[u8]) -> Result<(), FreestandingError> {
    let req = Request::default()
        .bytes(object)
        .bytes(symbol.as_bytes())
        .bytes(code);
    let out = call(&port().compare_code, &req.0);
    let mut w = Words::new(&out);
    let code = w.word();
    let r = if code == 0 {
        Ok(())
    } else {
        Err(decode_error(code, &mut w))
    };
    w.done();
    r
}

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}

/// Every image of the freestanding tests, built by the Rust compiler.
fn images() -> &'static [(String, FreestandingImage)] {
    static IMAGES: OnceLock<Vec<(String, FreestandingImage)>> = OnceLock::new();
    IMAGES.get_or_init(|| {
        let mut out = Vec::new();
        for target in FreestandingTarget::ALL {
            for (name, source) in PROGRAMS {
                let image = fs::build_image(&world(source), "main", &ImageOptions::new(target))
                    .unwrap_or_else(|e| panic!("{name} on {target}: {e}"));
                out.push((format!("{name} on {target}"), image));
            }
        }
        out
    })
}

fn link_options(image: &FreestandingImage) -> link::LinkOptions {
    link::LinkOptions {
        machine: image.target,
        image_base: image.board.image_base,
        ram_base: image.board.ram_base,
        ram_bytes: image.board.ram_bytes,
        stack_bytes: u64::from(image.stack_bytes),
    }
}

/// The trap entries `build_image` resolved for `image`: each site's
/// function, the object that defines it and its offset in it.
fn trap_entries(image: &FreestandingImage) -> Vec<Trap> {
    image
        .traps
        .iter()
        .map(|t| {
            let sym = image
                .symbols
                .iter()
                .find(|s| s.name == t.function && s.object != "linker")
                .unwrap();
            (sym.object.clone(), t.function.clone(), t.pc - sym.address)
        })
        .collect()
}

#[test]
fn stub_constants_match_silicon() {
    let packed = unsafe { port().constants.call(&[]) }.unwrap();
    assert_eq!(packed & 0xff, u64::from(stub::riscv_mie_mask()));
    assert_eq!(
        (packed >> 8) & 0xff,
        u64::from(stub::aarch64_daifset_mask())
    );
    let mut shift = 16;
    for t in FreestandingTarget::ALL {
        for e in [
            ExitDevice::SifiveTest { base: 0 },
            ExitDevice::Semihosting,
            ExitDevice::Park,
        ] {
            assert_eq!(
                (packed >> shift) & 0xff,
                u64::from(stub::exit_stack_bytes(t, e)),
                "{t} {e:?}"
            );
            shift += 8;
        }
    }
}

#[test]
fn stubs_are_byte_identical() {
    for (name, image) in images() {
        let e_flags = elf::e_flags(&image.objects[1].1).unwrap();
        let ours = vesper_stub(image.target, &image.board, e_flags).unwrap();
        assert_eq!(ours, image.objects[0].1, "{name}");
    }
    // Every exit device on both targets, and an e_flags value no image has.
    for target in FreestandingTarget::ALL {
        for exit in [
            ExitDevice::SifiveTest { base: 0x10_0000 },
            ExitDevice::SifiveTest { base: 0x1234_5678 },
            ExitDevice::Semihosting,
            ExitDevice::Park,
        ] {
            let mut board = target.qemu_virt();
            board.exit = exit;
            for e_flags in [0, 5, 0xdead_beef] {
                let rust = stub::build(target, &board, e_flags);
                let ours = vesper_stub(target, &board, e_flags);
                assert_eq!(ours, rust, "{target} {exit:?} {e_flags:#x}");
            }
        }
    }
}

#[test]
fn linked_images_are_byte_identical() {
    for (name, image) in images() {
        let ours = vesper_link(&image.objects, &link_options(image), &trap_entries(image))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(ours.entry, image.entry_address, "{name}");
        assert_eq!(ours.layout, image.layout, "{name}");
        assert_eq!(ours.symbols, image.symbols, "{name}");
        let pcs: Vec<u64> = image.traps.iter().map(|t| t.pc).collect();
        assert_eq!(ours.pcs, pcs, "{name}");
        assert!(ours.elf == image.elf, "{name}: images differ");
        // And the Rust linker's unpatched output, with no trap entries,
        // when the image has none.
        if image.traps.is_empty() {
            let rust = link::link(&image.objects, &link_options(image)).unwrap();
            assert!(ours.elf == rust.elf, "{name}");
        }
    }
}

#[test]
fn linker_scripts_are_identical() {
    for (name, image) in images() {
        let ours = vesper_script(image.target, &image.board, image.stack_bytes);
        assert_eq!(ours, image.linker_script, "{name}");
    }
    for target in FreestandingTarget::ALL {
        let mut board = target.qemu_virt();
        board.name = "a board, with \"quotes\"".into();
        board.ram_base = 0;
        board.image_base = 0x123;
        for stack in [0, 1, 15, 16, 17, 4096, u32::MAX] {
            assert_eq!(
                vesper_script(target, &board, stack),
                script::linker_script(target, &board, stack),
                "{target} {stack}"
            );
        }
    }
}

#[test]
fn executables_read_the_same() {
    for (name, image) in images() {
        assert_eq!(
            vesper_read_executable(&image.elf),
            elf::read_executable(&image.elf),
            "{name}"
        );
        // Truncations and a damaged header give the same errors.
        for len in [0, 3, 63, 64, 100, image.elf.len() / 2, image.elf.len() - 1] {
            let cut = &image.elf[..len];
            assert_eq!(
                vesper_read_executable(cut),
                elf::read_executable(cut),
                "{name} cut to {len}"
            );
        }
        let mut bad = image.elf.clone();
        bad[4] = 1;
        assert_eq!(
            vesper_read_executable(&bad),
            elf::read_executable(&bad),
            "{name}"
        );
    }
}

/// `(name, file range)` of every defined function symbol of an object.
fn functions(object: &[u8]) -> Vec<(String, Vec<u8>)> {
    use object::{Object, ObjectSection, ObjectSymbol, SymbolKind, SymbolSection};
    let file = object::File::parse(object).unwrap();
    let mut out = Vec::new();
    for sym in file.symbols() {
        if sym.kind() != SymbolKind::Text {
            continue;
        }
        let SymbolSection::Section(index) = sym.section() else {
            continue;
        };
        let data = file.section_by_index(index).unwrap().data().unwrap();
        let start = sym.address() as usize;
        out.push((
            sym.name().unwrap().to_string(),
            data[start..start + sym.size() as usize].to_vec(),
        ));
    }
    out
}

#[test]
fn code_comparison_agrees() {
    for (name, image) in images() {
        for (object, bytes) in &image.objects[1..] {
            let fns = functions(bytes);
            assert!(!fns.is_empty(), "{name} {object}");
            for (symbol, code) in fns {
                assert_eq!(vesper_compare_code(bytes, &symbol, &code), Ok(()));
                let mismatch = Err(FreestandingError::CodeMismatch {
                    function: symbol.clone(),
                });
                let mut changed = code.clone();
                changed[0] ^= 1;
                assert_eq!(vesper_compare_code(bytes, &symbol, &changed), mismatch);
                assert_eq!(
                    vesper_compare_code(bytes, &symbol, &code[..code.len() - 1]),
                    mismatch
                );
            }
            let missing = Err(FreestandingError::CodeMismatch {
                function: "no_such_function".into(),
            });
            assert_eq!(vesper_compare_code(bytes, "no_such_function", &[]), missing);
        }
    }
}

/// `(file offset, entry count)` of the `.rela<section>` section of an object.
fn rela(object: &[u8], section: &str) -> (usize, usize) {
    use object::{Object, ObjectSection};
    let file = object::File::parse(object).unwrap();
    let s = file.section_by_name(&format!(".rela{section}")).unwrap();
    let (off, size) = s.file_range().unwrap();
    (off as usize, size as usize / 24)
}

/// Both linkers on `objects`: the same result, or the same error.
fn same_link(what: &str, objects: &[(String, Vec<u8>)], options: &link::LinkOptions) {
    let rust = link::link(objects, options);
    let ours = vesper_link(objects, options, &[]);
    match (rust, ours) {
        (Ok(r), Ok(o)) => {
            assert!(r.elf == o.elf, "{what}: images differ");
            assert_eq!(r.layout, o.layout, "{what}");
            assert_eq!(r.symbols, o.symbols, "{what}");
        }
        (Err(r), Err(o)) => assert_eq!(o, r, "{what}"),
        (r, o) => panic!(
            "{what}: Rust {:?}, Vesper {:?}",
            r.map(|_| ()),
            o.map(|_| ())
        ),
    }
}

#[test]
fn linker_errors_agree() {
    for (name, image) in images().iter().filter(|(n, _)| n.starts_with("scalar")) {
        let options = link_options(image);
        let objects = &image.objects;
        // Missing, doubled and moved inputs.
        same_link(&format!("{name}: no stub"), &objects[1..], &options);
        same_link(&format!("{name}: no runtime"), &objects[..2], &options);
        let mut twice = objects.clone();
        twice.push(objects[2].clone());
        same_link(&format!("{name}: runtime twice"), &twice, &options);
        let mut stubs = objects.clone();
        stubs.push(objects[0].clone());
        same_link(&format!("{name}: two stubs"), &stubs, &options);
        same_link(&format!("{name}: nothing"), &[], &options);
        // The other target.
        let mut other = options.clone();
        other.machine = match image.target {
            FreestandingTarget::Riscv64Imac => FreestandingTarget::Aarch64,
            FreestandingTarget::Aarch64 => FreestandingTarget::Riscv64Imac,
        };
        same_link(&format!("{name}: other target"), objects, &other);
        // RAM too small, image below RAM, and a stack that just fits.
        for (ram_bytes, image_base, stack) in [
            (0x100, options.image_base, 16 * 1024),
            (options.ram_bytes, options.ram_base - 16, 16 * 1024),
            (options.ram_bytes, options.image_base, 0),
            (options.ram_bytes, options.image_base, 17),
        ] {
            let mut o = options.clone();
            o.ram_bytes = ram_bytes;
            o.image_base = image_base;
            o.stack_bytes = stack;
            same_link(
                &format!("{name}: {ram_bytes:#x} {image_base:#x} {stack}"),
                objects,
                &o,
            );
        }
        // Different e_flags.
        let mut flags = objects.clone();
        flags[2].1[48] ^= 1;
        same_link(&format!("{name}: e_flags"), &flags, &options);
        // Not relocatable, not ELF, not ELF64.
        for (at, value) in [(16, 2u8), (0, 0), (4, 1)] {
            let mut bad = objects.clone();
            bad[1].1[at] = value;
            let rust = link::link(&bad, &options).unwrap_err();
            let ours = vesper_link(&bad, &options, &[]).unwrap_err();
            assert!(
                matches!(
                    (&rust, &ours),
                    (FreestandingError::Object(_), FreestandingError::Object(_))
                ),
                "{name} {at}: {rust:?} {ours:?}"
            );
            if at == 16 {
                assert_eq!(ours, rust);
            }
        }
        // An unknown relocation type, and R_RISCV_RELAX (skipped).
        let (off, count) = rela(&objects[0].1, ".text.boot");
        assert!(count > 0);
        for r_type in [1000u32, 51] {
            let mut bad = objects.clone();
            bad[0].1[off + 8..off + 12].copy_from_slice(&r_type.to_le_bytes());
            same_link(&format!("{name}: r_type {r_type}"), &bad, &options);
        }
        // A relocation against the null symbol, and one past its section.
        let mut bad = objects.clone();
        bad[0].1[off + 12..off + 16].copy_from_slice(&0u32.to_le_bytes());
        same_link(&format!("{name}: null symbol"), &bad, &options);
        let mut bad = objects.clone();
        bad[0].1[off..off + 8].copy_from_slice(&0x10_0000u64.to_le_bytes());
        same_link(&format!("{name}: offset"), &bad, &options);
    }
}

#[test]
fn stub_rejects_semihosting_on_riscv() {
    let mut board = FreestandingTarget::Riscv64Imac.qemu_virt();
    board.exit = ExitDevice::Semihosting;
    assert_eq!(
        vesper_stub(FreestandingTarget::Riscv64Imac, &board, 0),
        stub::build(FreestandingTarget::Riscv64Imac, &board, 0)
    );
}

/// Link time, Rust against Vesper, over every image (ABBA). Run with
/// `--ignored --nocapture`.
#[test]
#[ignore]
fn link_cost() {
    use std::time::Instant;
    let rounds: usize = std::env::var("LINK_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let inputs: Vec<_> = images()
        .iter()
        .map(|(_, i)| (i.objects.clone(), link_options(i), trap_entries(i)))
        .collect();
    let rust = || {
        let t = Instant::now();
        for _ in 0..rounds {
            for (objects, options, _) in &inputs {
                std::hint::black_box(link::link(objects, options).unwrap());
            }
        }
        t.elapsed()
    };
    // The requests are built once: the time is the call, with the result
    // copied out of the JIT's sequence.
    let requests: Vec<_> = inputs
        .iter()
        .map(|(objects, options, traps)| link_request(objects, options, traps))
        .collect();
    let ours = || {
        let t = Instant::now();
        for _ in 0..rounds {
            for req in &requests {
                std::hint::black_box(call(&port().link, &req.0));
            }
        }
        t.elapsed()
    };
    let _ = port();
    let (a1, b1, b2, a2) = (rust(), ours(), ours(), rust());
    let r = (a1 + a2).as_secs_f64();
    let v = (b1 + b2).as_secs_f64();
    println!(
        "link x{}: Rust {:.1} ms, Vesper {:.1} ms ({:.2}x)",
        rounds * inputs.len(),
        r * 500.0,
        v * 500.0,
        v / r
    );
}

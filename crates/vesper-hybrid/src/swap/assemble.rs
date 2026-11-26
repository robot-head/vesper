// The package assembly of the Vesper driver's package load (#1986): the
// driver reads the graph itself (modules/driver/src/load.ves, slice 2, the
// `load` swap) or the Rust loader reads it (`packages::read_for_target`),
// the driver assembles it with `@vesper/check-names-packages`
// (modules/driver/src/assemble.ves, which documents the stream), and this
// file reads the result back into `packages::Assembled`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use vesper::ast::{Diagnostic, Module};
use vesper::packages::{
    Assembled, Loading, PackageDiagnostic, PackageObject, PackageVsys, SourceFile, SourceMap,
    VSYS_TARGETS,
};

// The `ast` section reader of the parse swap.
use super::parse::parse_words;

use super::{Registry, Swap, env_off, on, open, stats};

/// The layout version this reader understands (`assembled_format()` in
/// modules/driver/src/assemble.ves).
const FORMAT: u64 = 1;

/// Package graphs the Vesper driver assembled.
static ASSEMBLED: AtomicU64 = AtomicU64::new(0);
/// Package graphs the Rust loader assembled because the Vesper assembler
/// does not cover them (manifest errors, oversized sources).
static FALLBACKS: AtomicU64 = AtomicU64::new(0);
/// Package graphs the Vesper driver read without their parses whose
/// modules had parse diagnostics: the Rust loader loaded them again.
static PARSE_FALLBACKS: AtomicU64 = AtomicU64::new(0);
/// The assembly's costs: Vesper is the driver's assembly, decode this
/// file's read-back.
static COST: stats::Cost = stats::Cost::new("assemble");

/// The Rust parts of the driver's package loads (`LoadPackage` and
/// `hd_assembled`), with stats on; with the assembly's Vesper time, the
/// whole load: what the ABBA timing compares with the
/// Vesper assembly on and off (tests/driver_packages.rs).
static LOAD: AtomicU64 = AtomicU64::new(0);

/// Times one part of a package load into [`LOAD`] (only with stats on).
pub(crate) struct LoadTimer(stats::Timer);

/// Starts timing a part of the driver's package load.
pub(crate) fn driver_load_timer() -> LoadTimer {
    LoadTimer(stats::Timer::start())
}

impl LoadTimer {
    pub(crate) fn stop(self) {
        self.0.lap(&LOAD);
    }
}

thread_local! {
    /// When the Vesper driver started its assembly, with stats on.
    static START: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Whether the Vesper driver assembles the package it loads: the assemble
/// swap is on (`VESPER_HYBRID_SWAPS`, and not `VESPER_HYBRID_ASSEMBLE=0`)
/// and the load leaves out test-only declarations (the assembler parses
/// every module with the ordinary parser). The Rust loader then reads the
/// graph without parsing it (`packages::read_for_target`).
pub(crate) fn driver_assembles(include_tests: bool) -> bool {
    on("assemble") && !env_off("VESPER_HYBRID_ASSEMBLE") && !include_tests
}

/// The input of the Vesper driver's assembly for `loading`, when the
/// Vesper assembler covers the graph; `None` loads it again in Rust.
/// Starts the clock of the assembly's Vesper time when stats are on.
pub(crate) fn driver_assembly_input(loading: &Loading<'_>) -> Option<Vec<u8>> {
    let input = loading.assembly_input();
    if input.is_none() {
        FALLBACKS.fetch_add(1, Relaxed);
    } else if stats::level() > 0 {
        START.with(|t| t.set(Some(std::time::Instant::now())));
    }
    input
}

/// The word cursor.
struct Words<'a> {
    w: &'a [u64],
    at: usize,
}

impl Words<'_> {
    fn next(&mut self) -> u64 {
        let v = self.w[self.at];
        self.at += 1;
        v
    }
    fn count(&mut self) -> usize {
        self.next() as usize
    }
    /// A text: its byte length, then its bytes eight per word.
    fn text(&mut self) -> String {
        let len = self.count();
        let words = &self.w[self.at..self.at + len.div_ceil(8)];
        self.at += words.len();
        let mut out = Vec::with_capacity(words.len() * 8);
        for w in words {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.truncate(len);
        String::from_utf8(out).expect("vesper-hybrid: assembled text is UTF-8")
    }
}

/// The graph of `loading`, as the Vesper driver assembled it into `words`
/// (`assembled_words` of modules/driver/src/assemble.ves): its modules
/// joined in load order like `Loader::assemble`'s, with the qualified
/// names and the assembler's diagnostics. A graph with parse diagnostics
/// is `rust`'s, the Rust loader's load, which reports them. A malformed
/// stream is a bug in the Vesper module, so it panics. At
/// `VESPER_HYBRID_STATS=time`, `rust` also runs to compare.
pub(crate) fn driver_assembled(
    loading: Loading<'_>,
    words: &[u64],
    rust: impl FnOnce() -> Result<Assembled, String>,
) -> Result<Assembled, String> {
    assembled_from(
        words,
        |module, qualified, diagnostics| loading.assembled(module, qualified, diagnostics),
        false,
        rust,
    )
}

/// The graph the Vesper driver read itself (`DriverGraph`), as it
/// assembled it into `words`: as [`driver_assembled`], and at
/// `VESPER_HYBRID_STATS=time` the comparison with the Rust loader's load
/// covers the whole [`Assembled`] (sources, `.vsys` libraries, objects).
pub(crate) fn driver_graph_assembled(
    graph: DriverGraph,
    words: &[u64],
    rust: impl FnOnce() -> Result<Assembled, String>,
) -> Result<Assembled, String> {
    assembled_from(
        words,
        |module, qualified, diagnostics| graph.finish(module, qualified, diagnostics),
        true,
        rust,
    )
}

fn assembled_from(
    words: &[u64],
    finish: impl FnOnce(Module, BTreeMap<String, String>, Vec<PackageDiagnostic>) -> Assembled,
    whole: bool,
    rust: impl FnOnce() -> Result<Assembled, String>,
) -> Result<Assembled, String> {
    if let Some(start) = START.with(|t| t.take()) {
        COST.vesper
            .fetch_add(start.elapsed().as_nanos() as u64, Relaxed);
    }
    let mut r = Words { w: words, at: 0 };
    assert_eq!(r.next(), FORMAT, "vesper-hybrid: assembled_words format");
    if r.next() != 0 {
        assert_eq!(r.at, words.len(), "vesper-hybrid: trailing assembled words");
        PARSE_FALLBACKS.fetch_add(1, Relaxed);
        return rust();
    }
    ASSEMBLED.fetch_add(1, Relaxed);
    let timer = stats::Timer::start();
    let mut module = Module::default();
    for _ in 0..r.count() {
        let _file = r.next();
        let m = parse_words::read_ast_section(words, &mut r.at).module;
        module.items.extend(m.items);
        module.trait_bounds.extend(m.trait_bounds);
        module.model_uses.extend(m.model_uses);
    }
    module.public_items = (0..r.count()).map(|_| r.text()).collect();
    let mut qualified = BTreeMap::new();
    for _ in 0..r.count() {
        let internal = r.text();
        qualified.insert(internal, r.text());
    }
    let n = r.count();
    let mut diagnostics = Vec::with_capacity(n);
    for _ in 0..n {
        let file = r.count();
        let code = format!("E{:04}", r.next());
        let start = r.count();
        let end = r.count();
        let mut d = Diagnostic::new(&code, r.text(), start..end);
        if r.next() == 1 {
            d = d.with_help(r.text());
        }
        diagnostics.push(PackageDiagnostic {
            file: Some(file),
            diagnostic: d,
        });
    }
    assert_eq!(r.at, words.len(), "vesper-hybrid: trailing assembled words");
    let assembled = finish(module, qualified, diagnostics);
    timer.lap(&COST.decode);
    COST.call(false, rust, |rust| {
        rust.as_ref().is_ok_and(|rust| {
            rust.module == assembled.module
                && rust.sources.qualified == assembled.sources.qualified
                && format!("{:?}", rust.diagnostics) == format!("{:?}", assembled.diagnostics)
                && (!whole || same_graph(rust, &assembled))
        })
    });
    Ok(assembled)
}

/// Whether two loads of one package agree on everything but the module,
/// its qualified names and its diagnostics.
fn same_graph(a: &Assembled, b: &Assembled) -> bool {
    let files = |x: &Assembled| -> Vec<(String, String, usize)> {
        x.sources
            .files
            .iter()
            .map(|f| (f.display.clone(), f.text.clone(), f.base))
            .collect()
    };
    files(a) == files(b)
        && a.manifest_errors.is_empty()
        && b.manifest_errors.is_empty()
        && a.package == b.package
        && a.root_module == b.root_module
        && a.root_package_files == b.root_package_files
        && a.vsys == b.vsys
        && a.objects == b.objects
}

/// Package graphs the Vesper driver read itself (#1986 slice 2).
static READ: AtomicU64 = AtomicU64::new(0);
/// Package graphs the Vesper driver started to read and handed back to the
/// Rust loader (a manifest or I/O error, or a graph it does not cover).
static READ_FALLBACKS: AtomicU64 = AtomicU64::new(0);

/// Whether the Vesper driver reads the package graph itself (the load swap
/// is on, and not `VESPER_HYBRID_LOAD=0`), when it assembles it.
pub(crate) fn driver_reads(include_tests: bool) -> bool {
    driver_assembles(include_tests) && on("load") && !env_off("VESPER_HYBRID_LOAD")
}

/// The Vesper driver starts its read: with stats on, its Vesper time runs
/// from here to `hd_assembled`.
pub(crate) fn driver_read_begin() {
    if stats::level() > 0 {
        START.with(|t| t.set(Some(std::time::Instant::now())));
    }
}

/// The Vesper driver's read handed the graph back to the Rust loader: the
/// time it took counts as the load's Vesper time.
pub(crate) fn driver_read_fallback() {
    READ_FALLBACKS.fetch_add(1, Relaxed);
    if let Some(start) = START.with(|t| t.take()) {
        COST.vesper
            .fetch_add(start.elapsed().as_nanos() as u64, Relaxed);
    }
}

/// The package graph the Vesper driver read (the `Graph` step, the layout
/// is in src/driver/load.rs): the assembler's input, from which the
/// sources come, and the `.vsys` libraries and objects.
pub(crate) struct DriverGraph {
    input: Vec<u8>,
    vsys: Vec<PackageVsys>,
    objects: Vec<PackageObject>,
}

/// The byte cursor of the `Graph` stream.
struct Bytes<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Bytes<'a> {
    fn n(&mut self) -> Result<usize, String> {
        let w = self
            .b
            .get(self.at..self.at + 8)
            .ok_or("vesper-hybrid: short package graph")?;
        self.at += 8;
        usize::try_from(u64::from_le_bytes(w.try_into().expect("eight bytes")))
            .map_err(|_| "vesper-hybrid: package graph count".to_owned())
    }
    fn s(&mut self) -> Result<&'a str, String> {
        let len = self.n()?;
        let bytes = self
            .b
            .get(self.at..self.at + len)
            .ok_or("vesper-hybrid: short package graph text")?;
        self.at += len.next_multiple_of(8);
        std::str::from_utf8(bytes).map_err(|e| format!("vesper-hybrid: package graph text: {e}"))
    }
    fn target(&mut self) -> Result<Option<&'static str>, String> {
        let t = self.n()?;
        Ok(VSYS_TARGETS.get(t).map(|t| t.triple))
    }
}

/// The `Graph` step's stream.
pub(crate) fn driver_graph(arg: &[u8]) -> Result<DriverGraph, String> {
    let mut r = Bytes { b: arg, at: 0 };
    let len = r.n()?;
    let input = arg
        .get(8..8 + len)
        .ok_or("vesper-hybrid: short package graph input")?
        .to_vec();
    r.at = 8 + len;
    let mut vsys = vec![];
    for _ in 0..r.n()? {
        let (package, display, text) = (r.s()?, r.s()?, r.s()?);
        let mut targets = vec![];
        for _ in 0..r.n()? {
            let t = r
                .target()?
                .ok_or("vesper-hybrid: package graph .vsys target")?;
            targets.push((t, r.s()?.to_owned(), r.s()?.to_owned()));
        }
        vsys.push(PackageVsys {
            package: package.to_owned(),
            display: display.to_owned(),
            text: text.to_owned(),
            targets,
        });
    }
    let mut objects = vec![];
    for _ in 0..r.n()? {
        let (package, display) = (r.s()?, r.s()?);
        let mut files = vec![];
        for _ in 0..r.n()? {
            let t = r.target()?;
            files.push((t, PathBuf::from(r.s()?)));
        }
        objects.push(PackageObject {
            package: package.to_owned(),
            display: display.to_owned(),
            files,
        });
    }
    if r.at != arg.len() {
        return Err("vesper-hybrid: trailing package graph bytes".into());
    }
    READ.fetch_add(1, Relaxed);
    Ok(DriverGraph {
        input,
        vsys,
        objects,
    })
}

impl DriverGraph {
    /// The [`Assembled`] of the graph around the assembler's results: the
    /// sources, the root package and its files come from the assembler's
    /// input (packages/vesper-check-names-packages/src/load.ves), as
    /// `Loading::finish` takes them from the loader.
    fn finish(
        self,
        module: Module,
        qualified: BTreeMap<String, String>,
        diagnostics: Vec<PackageDiagnostic>,
    ) -> Assembled {
        let mut r = Bytes {
            b: &self.input,
            at: 0,
        };
        let bad = "vesper-hybrid: the driver's assembler input";
        let mut package = String::new();
        for p in 0..r.n().expect(bad) {
            let name = r.s().expect(bad);
            if p == 0 {
                package = name.to_owned();
            }
            for _ in 0..r.n().expect(bad) {
                r.s().expect(bad);
                r.n().expect(bad);
            }
            for _ in 0..r.n().expect(bad) {
                r.s().expect(bad);
            }
        }
        let mut files = vec![];
        for _ in 0..r.n().expect(bad) {
            let display = r.s().expect(bad).to_owned();
            let base = r.n().expect(bad);
            files.push(SourceFile {
                display,
                text: String::new(),
                base,
            });
        }
        let (mut root_module, mut root_package_files) = (None, vec![]);
        for _ in 0..r.n().expect(bad) {
            let p = r.n().expect(bad);
            let segments = r.n().expect(bad);
            for _ in 0..segments {
                r.s().expect(bad);
            }
            let file = r.n().expect(bad);
            files[file].text = r.s().expect(bad).to_owned();
            if p == 0 {
                if segments == 0 && root_module.is_none() {
                    root_module = Some(file);
                }
                root_package_files.push(file);
            }
        }
        Assembled {
            module,
            sources: SourceMap { files, qualified },
            diagnostics,
            manifest_errors: vec![],
            package,
            root_module,
            root_package_files,
            vsys: self.vsys,
            objects: self.objects,
        }
    }
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[
    open(
        "assemble",
        "driver package assembly; 0 diffs; no flip-gate configuration",
    ),
    open(
        "load",
        "driver package graph read (#1986 slice 2); 0 diffs; no flip-gate configuration",
    ),
];

pub(super) fn register(_r: &mut Registry) {}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: assemble answered {} package loads from Vesper ({} assembled in Rust, \
         {} with parse errors)",
        ASSEMBLED.load(Relaxed),
        FALLBACKS.load(Relaxed),
        PARSE_FALLBACKS.load(Relaxed),
    );
    eprintln!(
        "vesper-hybrid: load read {} package graphs in Vesper ({} handed back to Rust)",
        READ.load(Relaxed),
        READ_FALLBACKS.load(Relaxed),
    );
    eprintln!(
        "vesper-hybrid: assemble: package load {:.3} ms",
        (LOAD.load(Relaxed) + COST.vesper.load(Relaxed)) as f64 / 1e6
    );
    COST.report();
}

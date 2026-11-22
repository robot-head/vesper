//! SH4 lane 13 (#1287), part 3: the native emission of foreign (`.vsys`)
//! calls in Vesper. `ffi_hook` of packages/vesper-ffi-lowering/emit
//! (`@vesper/ffi-emit`) answers the `ffi` requests of
//! `Codegen::foreign_perform` (`vesper::codegen_swap::ffi`); every
//! function's CLIF text and every object must equal the Rust build's, with
//! the optimizer on and off, for every target a library can be built for.
//!
//! Corpus: the `.vsys` fixtures with their Vesper programs (every function
//! that performs a foreign effect is an entry), the packages that import
//! `.vsys` libraries, and hand-written programs that reach every parameter
//! form, result form, `errno`, C strings and `isolated(mpk)`.
#[path = "../../../tests/common/check_harness.rs"]
mod check_harness;

use std::collections::BTreeSet;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use vesper::codegen_swap::{self, CodegenHooks, Lent};
use vesper::ffi_lowering::{self, ForeignChecked, Handler, Imports};
use vesper::native::WordFunction;
use vesper_codegen_swap_test::*;

fn repo() -> std::path::PathBuf {
    check_harness::repo()
}

const TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

fn abi(target: &str) -> &'static str {
    match target {
        "x86_64-unknown-linux-gnu" => "sysv64",
        "x86_64-pc-windows-msvc" => "win64",
        "aarch64-apple-darwin" => "darwin64",
        _ => "aapcs64",
    }
}

fn hook() -> &'static WordFunction {
    static HOOK: OnceLock<WordFunction> = OnceLock::new();
    // Compiled with no hook in effect (a hook compiled under its own
    // `with_hooks` would call itself while its `OnceLock` initializes).
    HOOK.get_or_init(|| {
        codegen_swap::with_hooks(CodegenHooks::default(), || {
            let checked = foreign_package(&repo(), "packages/vesper-ffi-lowering/emit");
            hook_function(&checked, "ffi_hook")
        })
    })
}

fn ffi(lent: &mut Lent<'_, '_>) -> Result<bool, String> {
    vesper_hook(lent, hook())
}

fn hooks() -> CodegenHooks {
    CodegenHooks {
        ffi: Some(ffi),
        ..CodegenHooks::default()
    }
}

fn big_stack<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn_scoped(s, f)
            .expect("spawn a compile thread")
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

/// One program: a checked module and the entries to build.
struct Program {
    name: String,
    checked: ForeignChecked,
    target: &'static str,
    entries: Vec<String>,
}

/// Every function whose row performs a foreign effect.
fn foreign_entries(checked: &ForeignChecked) -> Vec<String> {
    checked
        .world()
        .functions
        .iter()
        .filter(|(_, f)| {
            f.effects
                .instances
                .iter()
                .any(|e| ffi_lowering::is_foreign_effect(&e.name))
        })
        .map(|(n, _)| n.clone())
        .collect()
}

const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn fill(vsys: &str, target: &str) -> String {
    vsys.replace("@DIGEST@", ZERO_DIGEST)
        .replace("@TARGET@", target)
        .replace("@ABI@", abi(target))
        .replace("x86_64-unknown-linux-gnu", target)
        .replace("\"sysv64\"", &format!("\"{}\"", abi(target)))
}

fn source_program(
    name: &str,
    source: &str,
    libraries: &[(&str, Handler)],
    target: &'static str,
) -> Result<Program, String> {
    let mut imports = Imports::new();
    for (vsys, handler) in libraries {
        imports
            .add(&fill(vsys, target), *handler)
            .map_err(|e| format!("{name}: {e}"))?;
    }
    let checked = ffi_lowering::check_source(source, &imports).map_err(|e| {
        let more: Vec<String> = e
            .diagnostics
            .iter()
            .map(|d| format!("{} at {:?}", d.message, d.span))
            .collect();
        format!("{name}: {e}; {}", more.join("; "))
    })?;
    let entries = foreign_entries(&checked);
    Ok(Program {
        name: format!("{name} [{target}]"),
        checked,
        target,
        entries,
    })
}

fn package_program(dir: &str, target: &'static str) -> Result<Program, String> {
    let p = vesper::packages::load(&repo().join(dir)).map_err(|e| format!("{dir}: {e:?}"))?;
    if !p.ok() {
        return Err(format!("{dir}: does not load"));
    }
    let imports = p
        .imports(Some(target))
        .map_err(|e| format!("{dir}: {e:?}"))?;
    let checked =
        ffi_lowering::check_module(&p.module, &imports).map_err(|e| format!("{dir}: {e}"))?;
    let entries = foreign_entries(&checked);
    Ok(Program {
        name: format!("{dir} [{target}]"),
        checked,
        target,
        entries,
    })
}

/// The hand-written programs: one library reaching every parameter and
/// result form the native lowering has.
const FORMS_VSYS: &str = r#"sys library Forms (
    target = "x86_64-unknown-linux-gnu", abi = "sysv64",
    link = "static", name = "forms", artifact = "blake3:@DIGEST@"
) {
    enum Mode: u8 { Off = 0, On = 3, Auto = 200 }
    enum Wide: u64 { Low = 5, High = 9 }
    wire struct Le { a: u16le, p: ZeroPad[2], b: i32le, c: u64 }
    wire struct Be { a: u16be, p: ZeroPad[2], b: i32be, tag: ByteArray[2], tail: ZeroPad[2] }
    wire struct Outer { inner: Be, n: u8, p: ZeroPad[3], more: u64 }
    wire struct Words { x: u64, y: u64 }
    linear extern type Conn;
    affine extern type Buf;
    fn forms_open() -> ForeignVal[Option[Conn]];
    fn forms_must() -> ForeignVal[NonNull[Conn]];
    fn forms_close(sink c: Conn) -> ForeignVal[Unit];
    fn forms_buf() -> ForeignVal[Buf];
    fn forms_use(in c: Conn, inout b: Buf) -> ForeignVal[u64];
    fn forms_i8(x: i8, y: u8, z: i16, w: u16) -> ForeignVal[i8];
    fn forms_u16(x: u32, y: i32, z: u64, w: i64) -> ForeignVal[u16];
    fn forms_usize(n: usize) -> ForeignVal[usize];
    fn forms_bool(b: bool) -> ForeignVal[bool];
    fn forms_mode(m: Mode, w: Wide) -> ForeignVal[Mode];
    fn forms_wide() -> ForeignVal[Wide];
    fn forms_errno(x: u32) -> ForeignVal[i32] errno;
    fn forms_unit_errno() -> ForeignVal[Unit] errno;
    fn forms_le(nocapture in a: Le, nocapture inout b: Le) -> ForeignVal[Unit];
    fn forms_be(nocapture inout b: Be, nocapture out o: Outer) -> ForeignVal[u32];
    fn forms_words(nocapture inout w: Words) -> ForeignVal[Unit];
    fn forms_opt(nocapture in a: Option[u16], nocapture inout b: Option[Be]) -> ForeignVal[Unit];
    fn forms_ptr(nocapture in a: u8, nocapture inout b: i32, nocapture out c: u64) -> ForeignVal[Unit];
    fn forms_slices(nocapture in a: CSlice[u16], nocapture inout b: CSlice[Le], nocapture in c: CSlice[u64]) -> ForeignVal[Unit];
    fn forms_opt_slice(nocapture inout a: Option[CSlice[i32]]) -> ForeignVal[Unit];
    fn forms_lent(nocapture in a: CSlice[u8], nocapture inout b: CSlice[u64]) -> ForeignVal[u64];
    fn forms_cstr(nocapture in a: CStr, nocapture in b: CStr) -> ForeignVal[i32];
    fn forms_cstr_errno(nocapture in a: CStr) -> ForeignVal[u32] errno;
}"#;

const FORMS_SOURCE: &str = r#"
fn handles() -> u64 eff [Ffi[Forms]] {
    let n = match Forms::forms_open() {
        Some(c) => {
            let r = match Forms::forms_buf() {
                Ok(b) => {
                    let mut b = b;
                    Forms::forms_use(in c, inout b)
                },
                Err(e) => e.raw,
            };
            Forms::forms_close(c);
            r
        },
        None => 0,
    };
    let c = Forms::forms_must();
    Forms::forms_close(c);
    n
}
fn scalars() -> u64 eff [Ffi[Forms]] {
    let a = Forms::forms_i8(-3, 250, -300, 60000);
    let b = Forms::forms_u16(7, -7, 9, -9);
    Forms::forms_usize(5);
    wrapping_add(widen_u64(wrapping_u8(a)), widen_u64(b))
}
fn validated() -> u64 eff [Ffi[Forms]] {
    let a: u64 = match Forms::forms_bool(true) {
        Ok(b) => if b { 1 } else { 2 },
        Err(e) => e.raw,
    };
    let b: u64 = match Forms::forms_mode(Mode::Auto, Wide::Low) {
        Ok(m) => match m {
            Off => 10,
            On => 20,
            Auto => 30,
        },
        Err(e) => e.raw,
    };
    let c: u64 = match Forms::forms_wide() {
        Ok(_) => 100,
        Err(e) => e.raw,
    };
    wrapping_add(wrapping_add(a, b), c)
}
fn errnos() -> u64 eff [Ffi[Forms]] {
    let (v, e) = Forms::forms_errno(3);
    let (_, f) = Forms::forms_unit_errno();
    wrapping_add(widen_u64(e), widen_u64(f))
}
fn structs() -> u64 eff [Ffi[Forms]] {
    let a = Le { a: 1, b: -2, c: 3 };
    let mut b = Le { a: 4, b: 5, c: 6 };
    Forms::forms_le(in a, inout b);
    let mut be = Be { a: 1, b: 2, tag: (3, 4) };
    let mut o = Outer { inner: Be { a: 0, b: 0, tag: (0, 0) }, n: 0, more: 0 };
    let r = Forms::forms_be(inout be, inout o);
    let mut w = Words { x: 1, y: 2 };
    Forms::forms_words(inout w);
    wrapping_add(widen_u64(r), w.x)
}
fn options() -> u64 eff [Ffi[Forms]] {
    let a: Option[u16] = Some(7);
    let mut b: Option[Be] = Some(Be { a: 1, b: 2, tag: (3, 4) });
    Forms::forms_opt(in a, inout b);
    let n: Option[u16] = None;
    let mut m: Option[Be] = None;
    Forms::forms_opt(in n, inout m);
    1
}
fn pointers() -> u64 eff [Ffi[Forms]] {
    let a: u8 = 3;
    let mut b: i32 = -4;
    let mut c: u64 = 0;
    Forms::forms_ptr(in a, inout b, inout c);
    wrapping_add(c, widen_u64(wrapping_u32(b)))
}
fn slices() -> u64 eff [Ffi[Forms]] {
    let a: (u16, u16, u16) = (1, 2, 3);
    let mut b: (Le, Le) = (Le { a: 1, b: 2, c: 3 }, Le { a: 4, b: 5, c: 6 });
    let c: (u64, u64) = (7, 8);
    Forms::forms_slices(in a, inout b, in c);
    let e: () = ();
    let mut f: (Le,) = (Le { a: 0, b: 0, c: 0 },);
    Forms::forms_slices(in e, inout f, in e);
    let mut s: Option[(i32, i32)] = Some((1, -1));
    Forms::forms_opt_slice(inout s);
    1
}
fn lent() -> u64 eff [Ffi[Forms]] {
    let mut s = ChunkSeq::new[u8]();
    s.push(1);
    s.push(2);
    let mut t = ChunkSeq::new[u64]();
    t.push(3);
    Forms::forms_lent(in s.as_slice(), inout t.as_slice_mut())
}
fn strings() -> u64 eff [Ffi[Forms]] {
    let a: u64 = match Forms::forms_cstr(in "abc".as_bytes(), in "d".as_bytes()) {
        Ok(v) => widen_u64(wrapping_u32(v)),
        Err(e) => e.raw,
    };
    let b: u64 = match Forms::forms_cstr_errno(in "x".as_bytes()) {
        Ok((v, e)) => wrapping_add(widen_u64(v), widen_u64(e)),
        Err(e) => e.raw,
    };
    wrapping_add(a, b)
}
"#;

/// A program: its name, source and `.vsys` libraries with their handlers.
type Source<'a> = (&'a str, &'a str, Vec<(&'a str, Handler)>);

fn fixture(path: &str) -> String {
    std::fs::read_to_string(repo().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn programs() -> (Vec<Program>, Vec<String>) {
    let mut out = vec![];
    let mut skipped = vec![];
    let bridge_vsys = fixture("tests/fixtures/ffi_lowering/bridge.vsys");
    let bridge = fixture("tests/fixtures/ffi_lowering/bridge.ves");
    let sandbox_vsys = fixture("tests/fixtures/ffi_lowering/sandbox.vsys");
    let sandbox = fixture("tests/fixtures/ffi_lowering/sandbox.ves");
    let aead_vsys = fixture("tests/fixtures/ffi_handles/aead.vsys");
    let aead = fixture("tests/fixtures/ffi_handles/aead.ves");
    for target in TARGETS {
        let sources: Vec<Source<'_>> = vec![
            ("bridge", &bridge, vec![(&bridge_vsys, Handler::Direct)]),
            (
                "bridge mpk",
                &bridge,
                vec![(&bridge_vsys, Handler::IsolatedMpk)],
            ),
            (
                "sandbox",
                &sandbox,
                vec![(&sandbox_vsys, Handler::IsolatedMpk)],
            ),
            ("aead", &aead, vec![(&aead_vsys, Handler::Direct)]),
            ("forms", FORMS_SOURCE, vec![(FORMS_VSYS, Handler::Direct)]),
        ];
        for (name, source, libraries) in sources {
            match source_program(name, source, &libraries, target) {
                Ok(p) => out.push(p),
                Err(e) => skipped.push(e),
            }
        }
        // The packages repeat the same few call shapes many times: they
        // run for the host only, every `FFI_EMIT_STEP`-th entry (default
        // 8).
        if Some(target) != host_target() {
            continue;
        }
        for dir in [
            "packages/vesper-std/fs",
            "packages/vesper-std/process",
            "packages/vesper-std/env",
            "packages/vesper-cranelift",
            "packages/vesper-codegen-base",
        ] {
            match package_program(dir, target) {
                Ok(mut p) => {
                    let step = std::env::var("FFI_EMIT_STEP")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(8usize)
                        .max(1);
                    p.entries = p.entries.into_iter().step_by(step).collect();
                    out.push(p)
                }
                Err(e) => skipped.push(e),
            }
        }
    }
    (out, skipped)
}

/// Builds `entry` of `p` with the foreign-call plan in place.
fn build_foreign(p: &Program, entry: &str, optimize: bool) -> Result<Build, String> {
    p.checked
        .with_lowering(Some(p.target), |w| {
            vesper::native::object_with_options(w, entry, Some(p.target), optimize)
        })
        .map(|(object, clif)| Build {
            clif: split_clif(&clif),
            object,
        })
        .map_err(|e| e.to_string())
}

fn compare_foreign(p: &Program, entry: &str, optimize: bool) -> Diff {
    let name = format!("{} {entry} (optimize {optimize})", p.name);
    let mut d = Diff::default();
    let rust = build_foreign(p, entry, optimize);
    let hooked = codegen_swap::with_hooks(hooks(), || build_foreign(p, entry, optimize));
    match (rust, hooked) {
        (Ok(r), Ok(h)) => {
            d.worlds = 1;
            d.functions = r.clif.len();
            for (key, text) in &r.clif {
                match h.clif.get(key) {
                    Some(t) if t == text => {}
                    Some(t) => d
                        .clif
                        .push((name.clone(), key.clone(), first_difference(text, t))),
                    None => d.clif.push((name.clone(), key.clone(), "missing".into())),
                }
            }
            for key in h.clif.keys().filter(|k| !r.clif.contains_key(*k)) {
                d.clif.push((name.clone(), key.clone(), "extra".into()));
            }
            if r.object != h.object {
                d.objects.push(name);
            }
        }
        (Err(r), Err(h)) if r == h => {}
        (r, h) => {
            let show = |x: Result<Build, String>| match x {
                Ok(_) => "ok".to_string(),
                Err(e) => e,
            };
            d.failures.push((name, show(r), show(h)));
        }
    }
    d
}

#[test]
fn foreign_call_emission_matches_rust() {
    big_stack(|| {
        let t = std::time::Instant::now();
        hook();
        eprintln!("hook compiled in {:?}", t.elapsed());
        let (mut programs, skipped) = programs();
        if let Ok(only) = std::env::var("FFI_EMIT_ONLY") {
            programs.retain(|p| p.name.contains(&only));
        }
        for s in &skipped {
            eprintln!("skipped: {s}");
        }
        // The fixtures and the hand-written forms build for every target.
        for name in ["bridge", "bridge mpk", "sandbox", "aead", "forms"] {
            for target in TARGETS {
                let want = format!("{name} [{target}]");
                assert!(
                    programs.iter().any(|p| p.name == want)
                        || std::env::var("FFI_EMIT_ONLY").is_ok(),
                    "{want} did not check: {skipped:?}"
                );
            }
        }
        let mut total = Diff::default();
        let mut built = 0usize;
        let mut failed_both = BTreeSet::new();
        let before = HANDLED.load(Ordering::Relaxed);
        for p in &programs {
            eprintln!(
                "{} ({} entries) at {:?}",
                p.name,
                p.entries.len(),
                t.elapsed()
            );
            for entry in &p.entries {
                for optimize in [true, false] {
                    let d = compare_foreign(p, entry, optimize);
                    if !d.is_clean() {
                        eprintln!("{}", d.summary());
                    }
                    if d.worlds == 0 && d.failures.is_empty() {
                        failed_both.insert(format!("{} {entry}", p.name));
                    } else {
                        built += 1;
                    }
                    total.add(d);
                }
            }
        }
        let handled = HANDLED.load(Ordering::Relaxed) - before;
        eprintln!(
            "{} programs, {built} builds, {} functions, {} built on neither side; {handled} requests answered from Vesper, {} declined",
            programs.len(),
            total.functions,
            failed_both.len(),
            DECLINED.load(Ordering::Relaxed)
        );
        for f in failed_both.iter().take(10) {
            eprintln!("neither: {f}");
        }
        assert!(handled > 0, "the Vesper hook answered no request");
        assert!(total.is_clean(), "{}", total.summary());
    });
}

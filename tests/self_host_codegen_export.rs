//! Self-hosting SH4 lane 12 (#1286): `@vesper/codegen-export` against the
//! Rust export ABI in src/native_export.rs and src/native_export/**.
//!
//! Part 1 ports the Rust binding generators (`rust_binding`, ABI v1-v4).
//! The corpus is every Vesper library a port crate or the hybrid build
//! links (the `Port::new` calls of crates/*/build.rs, listed in [`PORTS`]),
//! compiled with the `vesper` executable exactly as the port harness does
//! (`build --lib`, with `--no-opt`: the binding does not depend on the
//! optimizer), plus every interface again under other binding options.
//!
//! - `rust_bindings_match_committed`: the Rust generator on each
//!   interface reproduces the crate's committed binding, which checks
//!   that [`PORTS`] builds each library as its build script does.
//! - `vesper_bindings_match_rust`: the Vesper generator's text equals the
//!   Rust generator's, byte for byte, for every interface and every
//!   options variant.
//!
//! `CODEGEN_EXPORT_PORTS` limits the corpus to a comma-separated list of
//! libraries (the default is all of them).
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "self_host_codegen_export/words.rs"]
mod words;

use check_harness::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use vesper::native::export::{BindingOptions, LibraryInterface, rust_binding};

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-codegen-export"))
}

/// How a port's build script assembles its Vesper source.
#[derive(Clone, Copy)]
enum Source {
    /// A Loom package directory or one `.ves` file, relative to the crate.
    Path(&'static str),
    /// The files named by the string literals of the build script's
    /// `const <list>` (`<dir>/<name>` plus `suffix`), concatenated, each
    /// followed by `sep`.
    Joined {
        list: &'static str,
        dir: &'static str,
        suffix: &'static str,
        sep: &'static str,
    },
}

/// One library a port links: crate, library name, source, `rust_types`,
/// committed binding (relative to the crate).
type Port = (
    &'static str,
    &'static str,
    Source,
    Option<&'static str>,
    &'static str,
);

const B: &str = "port/binding.rs";
const CRATE_PORT: Option<&str> = Some("crate::port");
const VESPER: Source = Source::Path("vesper");

/// Every `Port::new` of crates/*/build.rs.
const PORTS: &[Port] = &[
    ("vesper-aegis", "aegis", VESPER, None, B),
    ("vesper-alloc", "alloc", VESPER, None, B),
    ("vesper-aura", "aura", VESPER, None, B),
    ("vesper-beacon", "beacon", VESPER, CRATE_PORT, B),
    ("vesper-bench", "bench", VESPER, CRATE_PORT, B),
    ("vesper-chorus", "chorus", VESPER, None, B),
    ("vesper-crucible", "crucible", VESPER, None, B),
    ("vesper-diagnostics", "diagnostics", VESPER, CRATE_PORT, B),
    ("vesper-ebpf", "ebpf", VESPER, CRATE_PORT, B),
    ("vesper-flux", "flux", VESPER, None, B),
    ("vesper-fmt", "fmt", VESPER, CRATE_PORT, B),
    ("vesper-forge", "forge", VESPER, None, B),
    ("vesper-grammar", "grammar", VESPER, None, B),
    ("vesper-highlight", "highlight", VESPER, CRATE_PORT, B),
    (
        "vesper-hybrid",
        "hybrid_layout",
        Source::Path("modules/layout"),
        None,
        "port/layout.rs",
    ),
    (
        "vesper-hybrid",
        "hybrid_parse",
        Source::Path("modules/parse"),
        None,
        "port/parse.rs",
    ),
    (
        "vesper-hybrid",
        "hybrid_check_layout",
        Source::Path("modules/check_layout"),
        None,
        "port/check_layout.rs",
    ),
    (
        "vesper-hybrid",
        "hybrid_ir_check",
        Source::Path("modules/ir_check"),
        None,
        "port/ir_check.rs",
    ),
    (
        "vesper-ignis",
        "ignis",
        Source::Joined {
            list: "SOURCES",
            dir: "vesper/src",
            suffix: "",
            sep: "",
        },
        Some("crate::port::ffi"),
        B,
    ),
    ("vesper-kinetic", "kinetic", VESPER, CRATE_PORT, B),
    ("vesper-lean-syntax", "lean_syntax", VESPER, CRATE_PORT, B),
    ("vesper-lint", "lint", VESPER, CRATE_PORT, B),
    ("vesper-loom-audit", "loom_audit", VESPER, None, B),
    ("vesper-loom-calculus", "loom_calculus", VESPER, None, B),
    ("vesper-loom-solver", "loom_solver", VESPER, None, B),
    ("vesper-pact", "pact", VESPER, None, B),
    (
        "vesper-port-build",
        "port_build",
        Source::Path("vesper/app"),
        None,
        B,
    ),
    (
        "vesper-port-pilot",
        "pilot",
        Source::Path("vesper/pilot.ves"),
        Some("crate"),
        B,
    ),
    (
        "vesper-port-pilot",
        "kit",
        Source::Path("vesper/kit.ves"),
        Some("crate::kit"),
        "port/kit.rs",
    ),
    (
        "vesper-port-pilot",
        "gauge",
        Source::Path("vesper/gauge.ves"),
        Some("crate::gauge"),
        "port/gauge.rs",
    ),
    (
        "vesper-port-pilot",
        "widen",
        Source::Path("vesper/widen.ves"),
        None,
        "port/widen.rs",
    ),
    (
        "vesper-port-pilot",
        "arrays",
        Source::Path("vesper/arrays.ves"),
        None,
        "port/arrays.rs",
    ),
    (
        "vesper-prism",
        "decoder",
        Source::Path("vesper/decoder"),
        Some("crate::forward::decoder::abi"),
        "port/decoder.rs",
    ),
    (
        "vesper-prism",
        "response",
        Source::Path("vesper/response"),
        Some("crate::forward::response::abi"),
        "port/response.rs",
    ),
    (
        "vesper-prism",
        "target",
        Source::Path("vesper/target"),
        Some("crate::forward::target::abi"),
        "port/target.rs",
    ),
    (
        "vesper-prism",
        "router",
        Source::Path("vesper/router"),
        Some("crate::forward::router::abi"),
        "port/router.rs",
    ),
    (
        "vesper-prism",
        "connection",
        Source::Path("vesper/connection"),
        Some("crate::forward::connection::abi"),
        "port/connection.rs",
    ),
    (
        "vesper-prism-h3",
        "h3",
        VESPER,
        Some("crate::qpack::port::abi"),
        B,
    ),
    ("vesper-quarry", "quarry", VESPER, None, B),
    ("vesper-resonance", "resonance", VESPER, None, B),
    ("vesper-semver", "semver", VESPER, None, B),
    (
        "vesper-serde",
        "serde",
        VESPER,
        Some("crate::port_types"),
        B,
    ),
    ("vesper-silicon", "silicon", VESPER, None, B),
    ("vesper-simpulse", "simpulse", VESPER, None, B),
    ("vesper-span", "span", VESPER, CRATE_PORT, B),
    ("vesper-std-driver", "stddriver", VESPER, None, B),
    ("vesper-tensor", "tensor", VESPER, None, B),
    (
        "vesper-warp-bbr",
        "bbr",
        Source::Joined {
            list: "PARTS",
            dir: "vesper/src",
            suffix: ".ves",
            sep: "\n",
        },
        Some("crate::port_types"),
        B,
    ),
    (
        "vesper-warp-tcp",
        "tcp",
        Source::Joined {
            list: "PARTS",
            dir: "vesper",
            suffix: ".ves",
            sep: "\n",
        },
        Some("crate::ported::types"),
        B,
    ),
    (
        "vesper-warp-wheel",
        "wheel",
        Source::Joined {
            list: "PARTS",
            dir: "vesper/src",
            suffix: ".ves",
            sep: "\n",
        },
        Some("crate"),
        B,
    ),
    ("vesper-warp", "warp", VESPER, None, B),
];

/// The string literals of `const <list>` in a build script.
fn listed(script: &str, list: &str) -> Vec<String> {
    let start = script
        .find(&format!("const {list}"))
        .unwrap_or_else(|| panic!("no const {list}"));
    let body = &script[start..];
    let body = &body[body.find('=').expect("=")..body.find("];").expect("];")];
    body.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// One compiled library.
struct Built {
    crate_dir: PathBuf,
    library: &'static str,
    rust_types: Option<&'static str>,
    binding: &'static str,
    interface: LibraryInterface,
}

fn selected(library: &str) -> bool {
    match std::env::var("CODEGEN_EXPORT_PORTS") {
        Ok(list) if !list.is_empty() => list.split(',').any(|l| l.trim() == library),
        _ => true,
    }
}

fn build(port: &Port, scratch: &Path) -> Result<Built, String> {
    let (krate, library, source, rust_types, binding) = *port;
    let dir = repo().join("crates").join(krate);
    let input = match source {
        Source::Path(p) => dir.join(p),
        Source::Joined {
            list,
            dir: sub,
            suffix,
            sep,
        } => {
            let script =
                std::fs::read_to_string(dir.join("build.rs")).map_err(|e| e.to_string())?;
            let mut text = String::new();
            for part in listed(&script, list) {
                let path = dir.join(sub).join(format!("{part}{suffix}"));
                text.push_str(
                    &std::fs::read_to_string(&path)
                        .map_err(|e| format!("{}: {e}", path.display()))?,
                );
                text.push_str(sep);
            }
            let joined = scratch.join(format!("{library}.ves"));
            std::fs::write(&joined, text).map_err(|e| e.to_string())?;
            joined
        }
    };
    let json = scratch.join(format!("{library}.json"));
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(["build", "--no-opt", "--lib", "--lib-name", library])
        .arg(&input)
        .arg("-o")
        .arg(scratch.join(format!("{library}.o")))
        .arg("--interface")
        .arg(&json)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "{library}: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let text = std::fs::read_to_string(&json).map_err(|e| e.to_string())?;
    let interface = serde_json::from_str(&text).map_err(|e| format!("{library}: {e}"))?;
    Ok(Built {
        crate_dir: dir,
        library,
        rust_types,
        binding,
        interface,
    })
}

/// Every selected library, compiled once per test binary (four at a time).
fn corpus() -> &'static [Built] {
    static C: OnceLock<Vec<Built>> = OnceLock::new();
    C.get_or_init(|| {
        let scratch =
            std::env::temp_dir().join(format!("vesper-codegen-export-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).expect("scratch directory");
        let ports: Vec<&Port> = PORTS.iter().filter(|p| selected(p.1)).collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results = std::sync::Mutex::new(vec![]);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(p) = ports.get(i) else { break };
                        let r = build(p, &scratch);
                        results.lock().unwrap().push((i, r));
                    }
                });
            }
        });
        let _ = std::fs::remove_dir_all(&scratch);
        let mut results = results.into_inner().unwrap();
        results.sort_by_key(|(i, _)| *i);
        let mut out = vec![];
        for (_, r) in results {
            match r {
                Ok(b) => out.push(b),
                Err(e) => panic!("build failed: {e}"),
            }
        }
        out
    })
}

fn options(b: &Built) -> BindingOptions {
    BindingOptions {
        types_path: b.rust_types.map(str::to_string),
    }
}

/// The options a library is checked under: its own, plus no types path
/// and a made-up one (so every library exercises both generator paths).
fn variants(b: &Built) -> Vec<BindingOptions> {
    let mut v = vec![options(b)];
    for alt in [None, Some("crate::alt::types")] {
        let alt = alt.map(str::to_string);
        if alt != v[0].types_path {
            v.push(BindingOptions { types_path: alt });
        }
    }
    v
}

#[test]
fn rust_bindings_match_committed() {
    let mut stale = vec![];
    for b in corpus() {
        let committed = std::fs::read_to_string(b.crate_dir.join(b.binding))
            .unwrap_or_default()
            .replace("\r\n", "\n");
        if rust_binding(&b.interface, &options(b)) != committed {
            stale.push(b.library);
        }
    }
    eprintln!(
        "committed bindings: {} of {} reproduced",
        corpus().len() - stale.len(),
        corpus().len()
    );
    assert!(stale.is_empty(), "not reproduced: {stale:?}");
}

#[test]
fn vesper_bindings_match_rust() {
    let entry = package().entry("binding_text", true);
    let (mut bad, mut cases, mut bytes) = (0, 0, 0usize);
    let mut abis = std::collections::BTreeMap::<String, usize>::new();
    for b in corpus() {
        *abis.entry(b.interface.abi.clone()).or_default() += 1;
        for o in variants(b) {
            cases += 1;
            let rust = rust_binding(&b.interface, &o);
            bytes += rust.len();
            let vesper = entry.text(&words::binding_input(&b.interface, &o));
            if let Some(d) = first_difference(&rust, &vesper) {
                bad += 1;
                if bad <= 5 {
                    eprintln!(
                        "binding: {} (types_path {:?}) differs at {d}",
                        b.library, o.types_path
                    );
                }
            }
        }
    }
    eprintln!(
        "bindings: {bad} of {cases} differ ({} libraries, {bytes} bytes, {abis:?})",
        corpus().len()
    );
    assert_eq!(bad, 0);
}

// ------------------------------------------------------------- directives

/// The input of `directives_dump`: the source, the `@TARGET@` and `@ABI@`
/// values and `vsys_target`'s error (empty on success), each a byte length
/// word and the bytes padded to a word.
fn directives_input(source: &str, target: Option<&str>) -> Vec<u8> {
    let (triple, abi, error) = match vesper::packages::vsys_target(target) {
        Ok((t, a)) => (t.to_string(), a.to_string(), String::new()),
        Err(e) => (String::new(), String::new(), e),
    };
    let mut out = vec![];
    for s in [source, &triple, &abi, &error] {
        out.extend((s.len() as u64).to_le_bytes());
        out.extend(s.as_bytes());
        while !out.len().is_multiple_of(8) {
            out.push(0);
        }
    }
    out
}

/// `Directives::parse` as the dump `parse_directives` writes. The `.vsys`
/// descriptor parse stays in Rust, so a `vsys` import shows its handler,
/// Rust path and filled-in text, not its library name and symbols.
fn directives_dump(source: &str, target: Option<&str>) -> String {
    use std::fmt::Write as _;
    let d = match vesper::native::export::Directives::parse(source, target) {
        Ok(d) => d,
        Err(e) => return format!("error {e}\n"),
    };
    let mut o = format!(
        "abi {}\noptions {}\n",
        d.abi.map_or("none".to_string(), |a| a.to_string()),
        d.option_mirrors as u8
    );
    for i in &d.instances {
        let _ = writeln!(
            o,
            "instance {} {} {} {}",
            i.name.as_deref().unwrap_or("-"),
            i.function,
            i.spelling(),
            i.alias()
        );
    }
    for l in &d.links {
        let _ = writeln!(
            o,
            "link {} {} {}",
            l.library,
            l.handler.map_or("-", |h| h.name()),
            l.rust_path.as_deref().unwrap_or("-")
        );
    }
    for v in &d.vsys {
        let _ = writeln!(
            o,
            "vsys {} {} {}",
            v.handler.name(),
            v.rust_path.as_deref().unwrap_or("-"),
            v.source.len()
        );
        o.push_str(&v.source);
    }
    o
}

/// Every `.ves` source under crates/, packages/, examples/ and tests/ that
/// names an `@export` directive.
fn directive_sources() -> Vec<(String, String)> {
    let mut out = vec![];
    let mut todo: Vec<PathBuf> = ["crates", "packages", "examples", "tests"]
        .iter()
        .map(|d| repo().join(d))
        .collect();
    while let Some(dir) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != "target") {
                    todo.push(p);
                }
            } else if p.extension().is_some_and(|x| x == "ves")
                && let Ok(text) = std::fs::read_to_string(&p)
                && text.contains("// @export")
            {
                out.push((p.display().to_string(), text));
            }
        }
    }
    out.sort();
    out
}

/// Directive lines that exercise every branch, refusals included. The
/// `.vsys` bodies are valid descriptors (their parse is Rust's).
const DIRECTIVE_CASES: &[&str] = &[
    "// @export abi 3\n",
    "// @export abi 4\n",
    "// @export abi 3 options\n",
    "// @export abi 3 options options\n",
    "// @export abi 5\n",
    "// @export abi\n",
    "// @export abi 3\n// @export abi 4\n",
    "// @export abi 3 fast\n",
    "// @export abilities\n",
    "  \t// @export abi 3   \r\n",
    "// @export instance largest_u64 = largest[u64]\n",
    "// @export instance largest[i32]\n",
    "// @export instance pair[(u8, u16), Option[u32]]\n",
    "// @export instance unit[()]\n",
    "// @export instance one[(u8)]\n",
    "// @export instance one[(u8,)]\n",
    "// @export instance nested[Result[Option[u8], (i64, bool)]]\n",
    "// @export instance trailing[u8,]\n",
    "// @export instance lit[Words[3]]\n",
    "// @export instance Bad = f[u8]\n",
    "// @export instance = f[u8]\n",
    "// @export instance f(u8)\n",
    "// @export instance f[u8\n",
    "// @export instance [u8]\n",
    "// @export instance f g[u8]\n",
    "// @export instance f[]\n",
    "// @export instance f[u8 u16]\n",
    "// @export instance f[,]\n",
    "// @export instance mixed_Case[MyType, my_type]\n",
    "// @export instance lower_args[my_type[u8]]\n",
    "// @export vsys Clock direct rust=crate::shim\n",
    "// @export vsys Clock isolated(mpk)\n",
    "// @export vsys Clock\n",
    "// @export vsys Clock direct direct\n",
    "// @export vsys Clock rust=\n",
    "// @export vsys Clock rust=a rust=b\n",
    "// @export vsys Clock\n// @export vsys Clock direct\n",
    "// @export vsys Clock\n// @export vsys Timer rust=t\n",
    "// @export vsys\n",
    "// @export vsys indirect\n",
    "// @export vsys direct fast\n",
    "// @export vsys direct rust=\n",
    "// @export frobnicate\n",
    "// @export\n",
    "fn f() -> Unit {\n    ()\n}\n// not // @export abi 3\n",
    "// @export vsys direct rust=crate::shim rust=crate::other\n// | sys library Clock (target = \"@TARGET@\", abi = \"@ABI@\", link = \"static\",\n// |     name = \"vesper_port_pilot\", artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {\n// |     fn pilot_clock_ticks() -> ForeignVal[u64];\n// | }\nfn after() -> Unit {\n    ()\n}\n",
    "// @export vsys isolated(mpk)\n  // |sys library Clock (target = \"@TARGET@\", abi = \"@ABI@\", link = \"static\",\n// |  name = \"c\", artifact = \"blake3:0000000000000000000000000000000000000000000000000000000000000000\") {\n// |  fn c_ticks() -> ForeignVal[u64];\n// | }\n",
];

#[test]
fn vesper_directives_match_rust() {
    let entry = package().entry("directives_dump", true);
    let mut cases: Vec<(String, String, Option<&str>)> = directive_sources()
        .into_iter()
        .map(|(p, t)| (p, t, None))
        .collect();
    for (i, c) in DIRECTIVE_CASES.iter().enumerate() {
        cases.push((format!("case {i}"), c.to_string(), None));
    }
    // The vsys cases again for the other hosted targets and a bad one.
    let vsys: Vec<_> = DIRECTIVE_CASES
        .iter()
        .filter(|c| c.contains("// |"))
        .collect();
    for target in [
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "riscv64gc-unknown-linux-gnu",
        "not-a-target",
    ] {
        for (i, c) in vsys.iter().enumerate() {
            cases.push((format!("vsys {i} on {target}"), c.to_string(), Some(target)));
        }
    }
    let (mut bad, mut errors) = (0, 0);
    for (name, source, target) in &cases {
        let rust = directives_dump(source, *target);
        errors += rust.starts_with("error ") as usize;
        let vesper = entry.text(&directives_input(source, *target));
        if let Some(d) = first_difference(&rust, &vesper) {
            bad += 1;
            if bad <= 8 {
                eprintln!("directives: {name} differs at {d}\nrust:   {rust}vesper: {vesper}");
            }
        }
    }
    eprintln!(
        "directives: {bad} of {} sources differ ({errors} refused)",
        cases.len()
    );
    assert!(cases.len() > 60, "only {} cases", cases.len());
    assert_eq!(bad, 0);
}

// ----------------------------------------------------------------- timing

/// ABBA over the whole corpus (every library under its own options): the
/// Rust generator, against the Vesper generator alone (inputs encoded
/// beforehand) and with the encoding and the text unpacking included.
#[test]
#[ignore = "timing; CHECK_HARNESS_SPEED=1"]
fn binding_cost() {
    if !speed() {
        return;
    }
    let entry = package().entry("binding_text", true);
    let cases: Vec<(&LibraryInterface, BindingOptions)> = corpus()
        .iter()
        .map(|b| (&b.interface, options(b)))
        .collect();
    let inputs: Vec<Vec<u8>> = cases
        .iter()
        .map(|(i, o)| words::binding_input(i, o))
        .collect();
    let encode = time(|| {
        for (i, o) in &cases {
            std::hint::black_box(words::binding_input(i, o));
        }
    });
    eprintln!("encode: {encode:?} for {} libraries", cases.len());
    abba(
        "binding generator",
        &mut || {
            time(|| {
                for (i, o) in &cases {
                    std::hint::black_box(rust_binding(i, o));
                }
            })
        },
        &mut || {
            time(|| {
                for input in &inputs {
                    std::hint::black_box(entry.words(input));
                }
            })
        },
    );
    abba(
        "binding generator with encode and unpack",
        &mut || {
            time(|| {
                for (i, o) in &cases {
                    std::hint::black_box(rust_binding(i, o));
                }
            })
        },
        &mut || {
            time(|| {
                for (i, o) in &cases {
                    std::hint::black_box(entry.text(&words::binding_input(i, o)));
                }
            })
        },
    );
}

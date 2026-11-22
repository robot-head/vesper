//! The libraries every port crate links (the `Port::new` calls of
//! crates/*/build.rs), shared by tests/self_host_codegen_export.rs and the
//! hybrid's export glue differential
//! (crates/vesper-hybrid/tests/codegen_export.rs).
use std::path::{Path, PathBuf};

/// How a port's build script assembles its Vesper source.
#[derive(Clone, Copy)]
pub enum Source {
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
pub type Port = (
    &'static str,
    &'static str,
    Source,
    Option<&'static str>,
    &'static str,
);

pub const B: &str = "port/binding.rs";
pub const CRATE_PORT: Option<&str> = Some("crate::port");
pub const VESPER: Source = Source::Path("vesper");

/// Every `Port::new` of crates/*/build.rs.
pub const PORTS: &[Port] = &[
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
pub fn listed(script: &str, list: &str) -> Vec<String> {
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

/// The source `vesper build --lib` compiles for `port`: its path, or the
/// joined files written to `scratch/<library>.ves`.
pub fn port_input(port: &Port, repo: &Path, scratch: &Path) -> Result<PathBuf, String> {
    let (krate, library, source, _, _) = *port;
    let dir = repo.join("crates").join(krate);
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
    Ok(input)
}

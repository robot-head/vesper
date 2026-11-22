//! Under `vesper-port`, link the Vesper port in vesper/ and set
//! cfg(vesper_port); see "Crates the compiler links" in
//! docs/maintenance/porting-a-crate.md. Without it this package builds the
//! Rust code, like the seed package (seed/), which has no build script.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The harness sets the cfg under `vesper-port`; declare it either way.
    println!("cargo:rustc-check-cfg=cfg({})", vesper_port_harness::CFG);
    #[cfg(feature = "vesper-port")]
    {
        let port = vesper_port_harness::Port::new("highlight", "vesper")
            .binding("port/binding.rs")
            .rust_types("crate::port");
        // One divergent edit per ported area (the effect pre-pass, the
        // identifier classes, the CST type walk), the same edits as
        // BUILD.bazel; port/test-port.sh asserts that the tests catch each.
        #[cfg(feature = "vesper-port-divergent")]
        let port = {
            println!("cargo:rerun-if-env-changed=VESPER_HIGHLIGHT_DIVERGE");
            let only = std::env::var("VESPER_HIGHLIGHT_DIVERGE").ok();
            EDITS
                .iter()
                .filter(|(area, _, _)| only.as_deref().is_none_or(|a| a == *area))
                .fold(port, |port, (_, from, to)| port.edit(from, to))
        };
        port.build();
    }
}

/// The divergence checks (mirrored in BUILD.bazel and asserted by
/// port/test-port.sh).
#[cfg(feature = "vesper-port-divergent")]
const EDITS: &[(&str, &str, &str)] = &[
    // The effect pre-pass: `handle E` no longer names an effect.
    (
        "effects",
        "note(\n                    inout fx,\n                    in src,\n                    w32(in t.start, j),\n                    token_end(in t, j),\n                    sym_at(in t, j),\n                    Linear::Keep\n                );\n            }\n        } else if k == K_L_BRACKET {",
        "}\n        } else if k == K_L_BRACKET {",
    ),
    // The identifier classes: `loop` is never a keyword.
    (
        "words",
        "        after == K_L_BRACE\n    } else if w == W_FOR {",
        "        false\n    } else if w == W_FOR {",
    ),
    // The CST walk: tokens inside `TYPE` nodes are not types.
    (
        "types",
        "            in_type.try_set(widen_u64(ev), true);",
        "            in_type.try_set(widen_u64(ev), false);",
    ),
];

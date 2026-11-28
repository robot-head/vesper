//! Vesper tool subcommands that live outside the compiler binary
//! (self-hosting SH0 lane 2, #1124).
//!
//! `vesper <tool> ARGS...` runs the `vesper-<tool>` executable built from this
//! crate (see `src/tool_commands.rs` in the root package). Each tool links the
//! libraries it needs, so the compiler's own closure, which Stage 2 must port,
//! no longer contains them. Tools that need compiler services (checking,
//! native batches, interfaces) call the `vesper` library.
//!
//! The libraries are the ones the compiler linked before the split, the frozen
//! Rust seed copies: under the owner's parity rule no tool switches to a
//! Vesper port here.
//!
//! SH6 lane 3 (#1398) moved `attest`, `cas`, `fuzz`, `verify --refute`,
//! `migrate`, `replay`, `semver`, `simd`, `top` and `traceparent` here, so
//! vesper-simd, vesper-migrate, vesper-loom-trust and vesper-loom-admission
//! left the compiler's closure (tests/compiler_closure.rs guards it). Part 2
//! moved `highlight` and `shot`, so vesper-highlight (with its rasterizer)
//! left too.
pub mod atomic_file;
pub mod attest;
pub mod audit;
pub mod bench;
pub mod cas;
pub mod compiler;
pub mod fuzz;
pub mod grammar;
pub mod highlight;
pub mod json;
pub mod migrate;
pub mod native_kernel;
pub mod proof_gate;
pub mod publish;
pub mod replay;
pub mod resolve;
pub mod sbom;
pub mod semver;
pub mod shot;
pub mod simd;
pub mod tee;
pub mod top;
pub mod traceparent;

use std::process;

/// Stack for the tool thread, the same as the compiler CLI's (see
/// `CLI_STACK_BYTES` in the root `src/main.rs`): tools check and lower source.
const TOOL_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Run one tool's entry point the way the compiler ran it: on a large-stack
/// thread, with errors printed as `vesper: <error>` and exit status 1.
pub fn main(run: fn(Vec<String>) -> Result<(), String>) {
    let result = std::thread::Builder::new()
        .name("vesper".to_owned())
        .stack_size(TOOL_STACK_BYTES)
        .spawn(move || run(std::env::args().skip(1).collect()))
        .expect("spawn the vesper tool thread")
        .join();
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("vesper: {e}");
            process::exit(1);
        }
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[cfg(test)]
mod compiler_free {
    //! The tools are leaving the Rust compiler library (#2670, part of
    //! #1397): the shipped `vesper` answers them over a process boundary
    //! instead. This table lists the modules that still link it, and may only
    //! shrink. When it is empty, the `vesper` dependency goes.

    /// The path every use of the compiler library starts with.
    const COMPILER_PATH: &str = "vesper::";

    /// Every source file of this crate, with whether it may still link the
    /// compiler library.
    const SOURCES: &[(&str, &str, bool)] = &[
        ("atomic_file.rs", include_str!("atomic_file.rs"), false),
        ("attest.rs", include_str!("attest.rs"), false),
        ("audit.rs", include_str!("audit.rs"), false),
        ("bench.rs", include_str!("bench.rs"), false),
        ("cas.rs", include_str!("cas.rs"), false),
        ("compiler.rs", include_str!("compiler.rs"), false),
        ("fuzz.rs", include_str!("fuzz.rs"), false),
        ("grammar.rs", include_str!("grammar.rs"), false),
        ("highlight.rs", include_str!("highlight.rs"), false),
        ("json.rs", include_str!("json.rs"), false),
        ("migrate.rs", include_str!("migrate.rs"), false),
        ("native_kernel.rs", include_str!("native_kernel.rs"), false),
        ("proof_gate.rs", include_str!("proof_gate.rs"), false),
        ("publish.rs", include_str!("publish.rs"), false),
        ("replay.rs", include_str!("replay.rs"), false),
        ("resolve.rs", include_str!("resolve.rs"), false),
        ("sbom.rs", include_str!("sbom.rs"), false),
        ("semver.rs", include_str!("semver.rs"), false),
        ("shot.rs", include_str!("shot.rs"), false),
        ("simd.rs", include_str!("simd.rs"), false),
        ("tee.rs", include_str!("tee.rs"), false),
        ("top.rs", include_str!("top.rs"), false),
        ("traceparent.rs", include_str!("traceparent.rs"), false),
    ];

    /// Whether `source` names the compiler library: `vesper::` not preceded
    /// by an identifier character (so `vesper_span::` and `crate::` paths do
    /// not count).
    fn links_compiler(source: &str) -> bool {
        source.match_indices(COMPILER_PATH).any(|(at, _)| {
            !source[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    }

    #[test]
    fn only_listed_modules_link_the_compiler() {
        for (name, source, allowed) in SOURCES {
            let links = links_compiler(source);
            assert!(
                *allowed || !links,
                "{name} links the Rust compiler library; ask the shipped `vesper` instead (#2670)"
            );
            assert!(
                !*allowed || links,
                "{name} no longer links the compiler library: remove it from the table"
            );
        }
    }
}

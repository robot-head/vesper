// The verified code generator (#1886, part 4d-2), from
// `modules/verified_codegen`: `vesper::verified_codegen::build`, the
// `compile` behind `vesper build --codegen verified` and the driver's
// verified build step, built by `@vesper/verified-codegen`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/verified_codegen.rs"]
mod verified_codegen_binding;

// The input words (#1886), shared with the root package's
// tests/self_host_verified_codegen.rs.
#[path = "../../../../tests/common/verified_words.rs"]
mod verified_words;

use super::{Registry, Swap, env_off, ir_words, must, on, open, pass};
use verified_words::{COMPILE_INPUT_FORMAT, compile_input, decode_build};
use vesper::verified_codegen as verified;

/// Builds whose artifact came from Vesper.
static VERIFIED_CODEGEN_CALLS: AtomicU64 = AtomicU64::new(0);
/// Whether `register` installed the swap.
static INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Builds the Vesper driver built with @vesper/verified-codegen
/// (`verified_build_input`), and those Rust's code generator built for it
/// (the swap off, or a program the input cannot hold).
static DRIVER_VERIFIED_INPUTS: AtomicU64 = AtomicU64::new(0);
static DRIVER_VERIFIED_RUST_BUILDS: AtomicU64 = AtomicU64::new(0);

/// `vesper::verified_codegen::VerifiedBuildTable` from
/// `modules/verified_codegen`. A program the word format cannot hold keeps
/// Rust's build.
const VESPER_VERIFIED_BUILD: verified::VerifiedBuildTable = verified::VerifiedBuildTable {
    build: |world, entry, options| {
        let Some(input) = compile_input(world, entry, options)? else {
            return verified::rust_build(world, entry, options);
        };
        VERIFIED_CODEGEN_CALLS.fetch_add(1, Relaxed);
        decode_build(
            &verified_codegen_binding::compile_input_words(&input),
            options.target,
        )
    },
};

/// The first word of [`verified_build_input`]'s text: the code
/// generator's input follows, or the artifact Rust's code generator built
/// (`VERIFIED_PORT_INPUT`/`VERIFIED_RUST_BUILT` in
/// modules/driver/src/lib.ves).
const VERIFIED_PORT_INPUT: u64 = 0;
const VERIFIED_RUST_BUILT: u64 = 1;

/// The target of the Vesper driver's `[arch, os]` codes (`VERIFIED_*` in
/// modules/driver/src/lib.ves).
fn driver_target(arch: u8, os: u8) -> Result<verified::Target, String> {
    const X86_64: u8 = 1;
    const AARCH64: u8 = 2;
    const RISCV64: u8 = 3;
    const LINUX: u8 = 1;
    const MACOS: u8 = 2;
    const WINDOWS: u8 = 3;
    let arch = match arch {
        X86_64 => verified::Arch::X86_64,
        AARCH64 => verified::Arch::Aarch64,
        RISCV64 => verified::Arch::Riscv64,
        _ => return Err("driver: bad verified target".into()),
    };
    let os = match os {
        LINUX => verified::Os::Linux,
        MACOS => verified::Os::MacOs,
        WINDOWS => verified::Os::Windows,
        _ => return Err("driver: bad verified target".into()),
    };
    Ok(verified::Target { arch, os })
}

/// The Vesper driver's `vesper build --codegen verified` (#1886, #1895)
/// for the target it parsed: `VERIFIED_PORT_INPUT` and then
/// `compile_input_words`'s input, for @vesper/verified-codegen to build in
/// the driver; or, when the swap is off or the program does not encode
/// (as the swap's build table does), `VERIFIED_RUST_BUILT`, the code size,
/// the artifact's byte length and its bytes. The driver writes and links
/// the artifact either way. Errors are the build's display.
pub(crate) fn verified_build_input(
    world: &vesper::check::World,
    entry: &str,
    arch: u8,
    os: u8,
    optimize: bool,
) -> Result<Vec<u8>, String> {
    let options = verified::Options {
        optimize,
        ..verified::Options::new(driver_target(arch, os)?)
    };
    if INSTALLED.load(Relaxed)
        && let Some(input) = compile_input(world, entry, &options).map_err(|e| e.to_string())?
    {
        DRIVER_VERIFIED_INPUTS.fetch_add(1, Relaxed);
        let mut out = VERIFIED_PORT_INPUT.to_le_bytes().to_vec();
        out.extend(input);
        return Ok(out);
    }
    DRIVER_VERIFIED_RUST_BUILDS.fetch_add(1, Relaxed);
    let built = verified::rust_build(world, entry, &options).map_err(|e| e.to_string())?;
    let mut out = VERIFIED_RUST_BUILT.to_le_bytes().to_vec();
    out.extend((built.code_len as u64).to_le_bytes());
    out.extend((built.bytes.len() as u64).to_le_bytes());
    out.extend(built.bytes);
    Ok(out)
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[
    pass("verified_codegen", "verified-codegen", "#1886"),
    open(
        "verified_coverage_listing",
        "opt-in through VESPER_HYBRID_VERIFIED_COVERAGE=1",
    ),
];

pub(super) fn register(_r: &mut Registry) {
    if on("verified_codegen") && !env_off("VESPER_HYBRID_VERIFIED") {
        // The input layout the Rust encoder writes must be the module's.
        if verified_codegen_binding::format() != COMPILE_INPUT_FORMAT {
            std::process::abort();
        }
        must(verified::install_verified_build(VESPER_VERIFIED_BUILD));
        INSTALLED.store(true, Relaxed);
    }
    if on("verified_coverage_listing")
        && std::env::var_os("VESPER_HYBRID_VERIFIED_COVERAGE").is_some_and(|v| v == "1")
    {
        must(verified::install_coverage_listing(|report| {
            String::from_utf8(verified_codegen_binding::coverage_listing_bytes(
                &report.listing_input(),
            ))
            .ok()
        }));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: verified_codegen built {} verified artifacts from Vesper",
        VERIFIED_CODEGEN_CALLS.load(Relaxed),
    );
    eprintln!(
        "vesper-hybrid: driver built {} verified executables in Vesper ({} built by Rust's code generator)",
        DRIVER_VERIFIED_INPUTS.load(Relaxed),
        DRIVER_VERIFIED_RUST_BUILDS.load(Relaxed),
    );
}

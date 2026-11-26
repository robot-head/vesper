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

use super::{Registry, Swap, env_off, ir_words, must, on, open};
use vesper::verified_codegen as verified;

/// Builds whose artifact came from Vesper.
static VERIFIED_CODEGEN_CALLS: AtomicU64 = AtomicU64::new(0);
/// Whether `register` installed the swap.
static INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Builds the Vesper driver took over (`verified_build_input`), and those
/// it then handed back to the Rust step.
pub(crate) static DRIVER_VERIFIED_INPUTS: AtomicU64 = AtomicU64::new(0);
pub(crate) static DRIVER_VERIFIED_FALLBACKS: AtomicU64 = AtomicU64::new(0);

/// `vesper::verified_codegen::VerifiedBuildTable` from
/// `modules/verified_codegen`. A program the word format cannot hold keeps
/// Rust's build.
const VESPER_VERIFIED_BUILD: verified::VerifiedBuildTable = verified::VerifiedBuildTable {
    build: |world, entry, options| {
        let Some(input) = compile_input(world, entry, options)? else {
            return verified::rust_build(world, entry, options);
        };
        VERIFIED_CODEGEN_CALLS.fetch_add(1, Relaxed);
        decode(
            &verified_codegen_binding::compile_input_words(&input),
            options.target,
        )
    },
};

/// `compile_input_words`'s input: the target words and the front-half
/// input of `entry` built for `target` (default: the host).
fn compile_input(
    world: &vesper::check::World,
    entry: &str,
    options: &verified::Options,
) -> Result<Option<Vec<u8>>, verified::VerifiedError> {
    let (program, max) = verified::optimized_program(world, entry, options)?;
    let Some((front, _)) = verified_words::mono_input(world, &program, entry, max, Some(options))
    else {
        return Ok(None);
    };
    let mut input: Vec<u8> = verified_words::target_words(options)
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    input.extend_from_slice(&front);
    Ok(Some(input))
}

/// The input of the Vesper driver's `vesper build --codegen verified`
/// (#1886): the summary's target text (`X86_64 Linux`) as a byte length
/// and the bytes padded to words, then `compile_input_words`'s input.
/// `None` when the swap is off, the target is not one whose artifact is a
/// Linux ELF executable (the Mach-O and COFF objects need their linkers),
/// or anything before the code generator fails or does not encode: the
/// Rust step then builds it, with its messages.
/// `VESPER_HYBRID_DRIVER_VERIFIED=0` keeps the build on the Rust step.
pub(crate) fn verified_build_input(
    world: &vesper::check::World,
    entry: &str,
    target: Option<&str>,
    optimize: bool,
) -> Option<Vec<u8>> {
    if !INSTALLED.load(Relaxed) || super::env_off("VESPER_HYBRID_DRIVER_VERIFIED") {
        return None;
    }
    let target = match target {
        Some(t) => verified::Target::from_triple(t).ok()?,
        None => verified::Target::host()?,
    };
    if verified::container_of(target) != Some(verified::Container::ElfExecutable) {
        return None;
    }
    let options = verified::Options {
        optimize,
        ..verified::Options::new(target)
    };
    let input = compile_input(world, entry, &options).ok()??;
    let label = format!("{:?} {:?}", target.arch, target.os);
    let mut out = (label.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(label.as_bytes());
    out.resize(8 + label.len().div_ceil(8) * 8, 0);
    out.extend(input);
    Some(out)
}

/// `compile_input_words`'s two packings (each a byte length, then the
/// bytes, eight to a word): `error: <display>\n`, or the build's summary
/// (`code <bytes>` first) and then the artifact.
fn decode(
    words: &[u64],
    target: verified::Target,
) -> Result<verified::Built, verified::VerifiedError> {
    let invalid =
        |m: &str| verified::VerifiedError::Invalid(format!("Vesper verified codegen: {m}"));
    let unpack = |at: usize| -> Option<(Vec<u8>, usize)> {
        let len = usize::try_from(*words.get(at)?).ok()?;
        let n = len.div_ceil(8);
        let packed = words.get(at + 1..at.checked_add(1 + n)?)?;
        let mut bytes = Vec::with_capacity(n * 8);
        for w in packed {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        bytes.truncate(len);
        Some((bytes, at + 1 + n))
    };
    let (text, at) = unpack(0).ok_or_else(|| invalid("truncated output"))?;
    let (bytes, _) = unpack(at).ok_or_else(|| invalid("truncated output"))?;
    let text = String::from_utf8(text).map_err(|e| invalid(&e.to_string()))?;
    if let Some(e) = text.strip_prefix("error: ") {
        return Err(verified::VerifiedError::from_display(
            e.strip_suffix('\n').unwrap_or(e),
        ));
    }
    let code_len = text
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("code "))
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| invalid("no code size"))?;
    let container = verified::container_of(target).ok_or_else(|| invalid("no container"))?;
    Ok(verified::Built {
        container,
        bytes,
        code_len,
    })
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[
    open(
        "verified_codegen",
        "0 diffs over 3,640 corpus compiles, 1.03x end to end; no flip-gate configuration",
    ),
    open(
        "verified_coverage_listing",
        "opt-in through VESPER_HYBRID_VERIFIED_COVERAGE=1",
    ),
];

pub(super) fn register(_r: &mut Registry) {
    if on("verified_codegen") && !env_off("VESPER_HYBRID_VERIFIED") {
        // The input layout the Rust encoder writes must be the module's.
        if verified_codegen_binding::format() != 1 {
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
        "vesper-hybrid: driver built {} verified executables in Vesper ({} fell back to Rust)",
        DRIVER_VERIFIED_INPUTS.load(Relaxed) - DRIVER_VERIFIED_FALLBACKS.load(Relaxed),
        DRIVER_VERIFIED_FALLBACKS.load(Relaxed),
    );
}

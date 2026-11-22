//! `vesper build --tee`: the runtime-measurement extension a TDX guest (or a
//! CCA realm) performs for this executable, computed through Vesper Aegis.
//!
//! The executable is digested with BLAKE3 (the digest Loom and Aegis use for
//! build artifacts) and the digest is extended once into a zeroed SHA-384
//! register padded to 48 bytes, exactly as `vesper_aegis::tdx` checks
//! RTMR[2]/RTMR[3] (`vesper_aegis::claims::single_extend`); the SHA-512/64-byte
//! form is the CCA REM value. Nothing is extended into hardware here: no TDX
//! or CCA guest runs at build time. The values are what a relying party
//! compares with a quote that measured this exact file.
use sha2::{Digest, Sha384, Sha512};
use std::{fs, path::Path};

pub const SCHEMA: &str = "vesper.tee-measurement/1";

/// One extend of a zeroed register with a 32-byte digest, the extend data
/// zero-padded to `pad_to` bytes: `H(0^len(H) || digest || 0^(pad_to - 32))`.
/// The same computation as `vesper_aegis::claims::single_extend`, done here
/// so the tools do not link Aegis: Bazel builds Aegis's Vesper port with the
/// compiler, whose runfiles include these tools (SH6 lane 8, #1403).
fn single_extend<H: Digest>(pad_to: usize, data: &[u8; 32]) -> Vec<u8> {
    let mut h = H::new();
    h.update(vec![0u8; <H as Digest>::output_size()]);
    h.update(data);
    h.update(vec![0u8; pad_to.saturating_sub(data.len())]);
    h.finalize().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Measure `path` and write `<path>.tee.json` beside it.
pub fn measure_executable(path: &Path) -> Result<(), String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let digest: [u8; 32] = *blake3::hash(&bytes).as_bytes();
    let rtmr = single_extend::<Sha384>(48, &digest);
    let rem = single_extend::<Sha512>(64, &digest);
    let report = serde_json::json!({
        "schema": SCHEMA,
        "artifact": path.file_name().map(|n| n.to_string_lossy().into_owned()),
        "artifact_bytes": bytes.len(),
        "blake3": hex(&digest),
        "tdx_rtmr_single_extend_sha384": hex(&rtmr),
        "cca_rem_single_extend_sha512": hex(&rem),
        "hardware_extend": "not performed: no TDX or CCA guest at build time",
    });
    let mut out = path.as_os_str().to_owned();
    out.push(".tee.json");
    let out = std::path::PathBuf::from(out);
    let text = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    fs::write(&out, text).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("tee measurement {} (RTMR {})", out.display(), hex(&rtmr));
    Ok(())
}

/// `vesper-tee measure <executable>`: what `vesper build --tee` runs.
pub fn run(args: Vec<String>) -> Result<(), String> {
    match args.as_slice() {
        [help] if help == "--help" || help == "-h" => {
            println!(
                "Usage: vesper-tee measure <executable>\n  Write <executable>.tee.json: the TDX RTMR / CCA REM single-extend values\n  (vesper build --tee runs this after linking)"
            );
            Ok(())
        }
        [measure, path] if measure == "measure" => measure_executable(Path::new(path)),
        _ => Err("usage: vesper-tee measure <executable>".into()),
    }
}

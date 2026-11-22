//! Host SIMD tooling, separate from source-language ISA capability typing.
use serde_json::json;
use std::{fs::File, io::Read};
use vesper_simd::{IsaProfileKind, JitScanner};

const HELP: &str = "Usage: vesper simd scan <file> --delimiter <0..255> [--profile baseline128] [--json]\n\nCount delimiter bytes and XOR all input bytes with a runtime-checked native\nBaseline128 scanner. Reads in bounded chunks. Unsupported CPU profiles fail\nexplicitly. This host tool does not implement source-language Isa effects.\n";

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("--help" | "-h") => {
            print!("{HELP}");
            return Ok(());
        }
        Some("scan") => {}
        _ => return Err(format!("expected 'simd scan'; {HELP}")),
    }
    let mut input = None;
    let mut delimiter = None;
    let mut profile = None;
    let mut json_output = false;
    let mut positional_only = false;
    while let Some(arg) = args.next() {
        if !positional_only {
            match arg.as_str() {
                "--help" | "-h" => {
                    print!("{HELP}");
                    return Ok(());
                }
                "--" => {
                    positional_only = true;
                    continue;
                }
                "--delimiter" => {
                    if delimiter.is_some() {
                        return Err("--delimiter may only be supplied once".into());
                    }
                    let value = args.next().ok_or("--delimiter requires a byte (0..255)")?;
                    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                        return Err("--delimiter requires a decimal byte (0..255)".into());
                    }
                    delimiter = Some(value.parse::<u8>().map_err(|_| "delimiter exceeds 255")?);
                    continue;
                }
                "--profile" => {
                    if profile.is_some() {
                        return Err("--profile may only be supplied once".into());
                    }
                    profile = Some(match args.next().as_deref() {
                        Some("baseline128") => IsaProfileKind::Baseline128,
                        Some("avx2") => IsaProfileKind::Avx2,
                        Some("avx512") => IsaProfileKind::Avx512,
                        Some("sve2") => IsaProfileKind::Sve2,
                        Some("rvv1") => IsaProfileKind::Rvv1,
                        _ => return Err("unknown or missing SIMD profile".into()),
                    });
                    continue;
                }
                "--json" => {
                    if json_output {
                        return Err("--json may only be supplied once".into());
                    }
                    json_output = true;
                    continue;
                }
                _ if arg.starts_with('-') => return Err(format!("unknown simd option '{arg}'")),
                _ => {}
            }
        }
        if input.replace(arg).is_some() {
            return Err("simd scan accepts one input file".into());
        }
    }
    let input = input.ok_or("simd scan requires an input file")?;
    let delimiter = delimiter.ok_or("simd scan requires --delimiter <0..255>")?;
    let scanner = JitScanner::compile_profile(profile.unwrap_or(IsaProfileKind::Baseline128))
        .map_err(|e| e.to_string())?;
    let mut file = File::open(&input).map_err(|e| format!("{input}: {e}"))?;
    let mut buffer = [0u8; 64 * 1024];
    let (mut bytes, mut matches, mut xor) = (0u64, 0u64, 0u8);
    loop {
        let len = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(len) => len,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("{input}: {e}")),
        };
        let result = scanner.scan(&buffer[..len], delimiter);
        bytes = bytes
            .checked_add(len as u64)
            .ok_or("input length exceeds u64")?;
        matches = matches
            .checked_add(result.matches)
            .ok_or("match count exceeds u64")?;
        xor ^= result.xor;
    }
    if json_output {
        let metadata = scanner.metadata();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version":1,"execution":"native_jit","bytes":bytes,
                "delimiter":delimiter,"matches":matches,"xor":xor,
                "code":{"profile":"baseline128","target":metadata.target,
                    "vector_bits":metadata.vector_bits,"tail_strategy":metadata.tail_strategy,
                    "abi":metadata.abi,"codegen_version":metadata.codegen_version,
                    "codegen_settings":metadata.codegen_settings,
                    "enabled_isa_flags":metadata.enabled_isa_flags}
            }))
            .map_err(|e| e.to_string())?
        );
    } else {
        println!("bytes={bytes} delimiter={delimiter} matches={matches} xor={xor}");
    }
    Ok(())
}

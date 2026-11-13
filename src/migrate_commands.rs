//! `vesper migrate --source <c-header | rust-crate-dir | file.rs>`: lift
//! legacy C or Rust signatures through `vesper-migrate` (§29.3) and write the
//! Bridge `.vsys` spec and the Vesper declarations (integration B, #136).
use std::{
    fs,
    path::{Path, PathBuf},
};
use vesper_ffi::sys::{LinkMode, Target};
use vesper_migrate::{Limits, Migration, c, rust, synth};

const HELP: &str = "Usage: vesper migrate --source <path> -o <dir> [options]\n\nLift a preprocessor-free C header (.h), a Rust crate directory (Cargo.toml + src/lib.rs)\nor a single Rust file (.rs) into Vesper declarations and a Bridge .vsys spec.\n\nOptions:\n  --source <path>        Legacy input (required)\n  -o <dir>               Output directory for <stem>.ves, <stem>.vsys and <stem>.migrate.txt\n  --library <Name>       .vsys library name (default: <Stem>Abi)\n  --target <triple>      .vsys target (default: x86_64-unknown-linux-gnu)\n  --link <mode>          static or dynamic (default: static)\n  --link-name <name>     Linked library name (default: the stem)\n  --artifact <file>      Library artifact whose BLAKE3 the .vsys pins (required for .vsys)\n  --pkg-config <req>     pkg-config requirement recorded in the .vsys\n\nWithout --artifact no .vsys is written: a spec must pin the real artifact digest.\n";

fn target(triple: &str) -> Result<Target, String> {
    [
        Target::LinuxX86_64,
        Target::LinuxAarch64,
        Target::DarwinAarch64,
        Target::WindowsX86_64,
    ]
    .into_iter()
    .find(|t| t.triple() == triple)
    .ok_or_else(|| {
        format!(
            "unsupported .vsys target '{triple}'; expected x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu, aarch64-apple-darwin or x86_64-pc-windows-msvc"
        )
    })
}

fn camel(stem: &str) -> String {
    stem.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut c = s.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn analyze(source: &Path) -> Result<(Migration, String, Option<String>), String> {
    let limits = Limits::default();
    let fail = |e: vesper_migrate::MigrateError| format!("{}: {e}", source.display());
    if source.is_dir() {
        let m = rust::analyze_crate_dir(source, limits).map_err(fail)?;
        let stem = match &m.origin {
            vesper_migrate::Origin::RustCrate { name } => name.clone(),
            vesper_migrate::Origin::CHeader { header } => header.clone(),
        };
        return Ok((m, stem, None));
    }
    let text = fs::read_to_string(source).map_err(|e| format!("{}: {e}", source.display()))?;
    let file = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("source has no file name")?;
    let stem = source
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("source has no file stem")?;
    match source.extension().and_then(|e| e.to_str()) {
        Some("h") => Ok((
            c::analyze_header(&file, &text, limits).map_err(fail)?,
            stem,
            Some(file),
        )),
        Some("rs") => Ok((
            rust::analyze_source(&stem, &text, limits).map_err(fail)?,
            stem,
            None,
        )),
        _ => Err(format!(
            "{}: expected a C header (.h), a Rust file (.rs) or a crate directory",
            source.display()
        )),
    }
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut source = None;
    let mut out = None;
    let mut library = None;
    let mut triple = String::from("x86_64-unknown-linux-gnu");
    let mut link = LinkMode::Static;
    let mut link_name = None;
    let mut artifact = None;
    let mut pkg_config = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| it.next().ok_or_else(|| format!("{flag} requires a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "--source" => source = Some(PathBuf::from(value(&arg)?)),
            "-o" => out = Some(PathBuf::from(value(&arg)?)),
            "--library" => library = Some(value(&arg)?),
            "--target" => triple = value(&arg)?,
            "--link" => {
                link = match value(&arg)?.as_str() {
                    "static" => LinkMode::Static,
                    "dynamic" => LinkMode::Dynamic,
                    other => {
                        return Err(format!("--link must be static or dynamic, not '{other}'"));
                    }
                }
            }
            "--link-name" => link_name = Some(value(&arg)?),
            "--artifact" => artifact = Some(PathBuf::from(value(&arg)?)),
            "--pkg-config" => pkg_config = Some(value(&arg)?),
            other => {
                return Err(format!(
                    "unknown migrate option '{other}'; see vesper migrate --help"
                ));
            }
        }
    }
    let source = source.ok_or("migrate requires --source <path>; see vesper migrate --help")?;
    let out = out.ok_or("migrate requires -o <dir>; see vesper migrate --help")?;
    let target = target(&triple)?;
    let (migration, stem, header) = analyze(&source)?;
    fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let write = |name: String, text: &str| -> Result<PathBuf, String> {
        let path = out.join(name);
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    };
    let report = synth::report(&migration);
    let declarations = write(
        format!("{stem}.ves"),
        &synth::vesper_declarations(&migration),
    )?;
    let report_path = write(format!("{stem}.migrate.txt"), &report)?;
    let lifted = migration.functions.iter().filter(|f| f.sys.is_ok()).count();
    println!(
        "migrate {}: {} function(s) ({} with a C ABI), {} struct(s), {} enum(s), {} refusal(s)",
        source.display(),
        migration.functions.len(),
        lifted,
        migration.structs.len(),
        migration.enums.len(),
        migration.all_refusals().len()
    );
    println!("wrote {}", declarations.display());
    println!("wrote {}", report_path.display());
    match artifact {
        None => println!(
            "no .vsys written: pass --artifact <library file> so the spec pins its BLAKE3 digest"
        ),
        Some(artifact) => {
            let bytes = fs::read(&artifact).map_err(|e| format!("{}: {e}", artifact.display()))?;
            let options = synth::SysSpecOptions {
                library: library.unwrap_or_else(|| format!("{}Abi", camel(&stem))),
                target,
                link,
                link_name: link_name.unwrap_or_else(|| stem.clone()),
                artifact_blake3: *blake3::hash(&bytes).as_bytes(),
                header,
                pkg_config,
            };
            let spec = synth::sys_spec(&migration, &options)
                .map_err(|e| format!("{}: {e}", source.display()))?;
            let path = write(format!("{stem}.vsys"), &spec.text)?;
            println!(
                "wrote {} ({} function(s) omitted from the spec)",
                path.display(),
                spec.omitted.len()
            );
        }
    }
    Ok(())
}

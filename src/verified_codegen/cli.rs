//! `vesper build --codegen verified`: write a static ELF executable, or link
//! the Mach-O object with the system linker.
use super::{Container, Options, Target, compile};
use crate::check::World;
use std::path::Path;

/// Build `entry` for `target` (default: the host) into `output`. Returns the
/// line the CLI prints.
pub fn build_command(
    world: &World,
    entry: &str,
    target: Option<&str>,
    optimize: bool,
    linker: &str,
    output: &Path,
) -> Result<String, String> {
    let target = match target {
        Some(t) => Target::from_triple(t).map_err(|e| e.to_string())?,
        None => Target::host().ok_or_else(|| {
            "verified codegen: the host is not a supported target; pass --target".to_owned()
        })?,
    };
    let options = Options {
        optimize,
        ..Options::new(target)
    };
    let artifact = compile(world, entry, &options).map_err(|e| e.to_string())?;
    match artifact.container {
        Container::ElfExecutable => {
            std::fs::write(output, &artifact.bytes)
                .map_err(|e| format!("{}: {e}", output.display()))?;
            make_executable(output)?;
        }
        Container::MachOObject => {
            let object = output.with_extension("vesper.o");
            std::fs::write(&object, &artifact.bytes)
                .map_err(|e| format!("{}: {e}", object.display()))?;
            let status = std::process::Command::new(linker)
                .arg(&object)
                .arg("-o")
                .arg(output)
                .status()
                .map_err(|e| format!("could not invoke linker '{linker}': {e}"));
            let _ = std::fs::remove_file(&object);
            if !status?.success() {
                return Err("linker failed".into());
            }
        }
    }
    Ok(format!(
        "built {} (verified codegen, {:?} {:?}, {} bytes of code)",
        output.display(),
        target.arch,
        target.os,
        artifact.code_len
    ))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("{}: {e}", path.display()))
}
#[cfg(not(unix))]
fn make_executable(_: &Path) -> Result<(), String> {
    Ok(())
}

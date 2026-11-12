//! Emit inspectable cross-target artifacts; no cross-target code is executed.
use vesper_simd::{ElfTarget, IsaProfileKind, ObjectArtifact};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("usage: emit_objects OUTPUT_DIRECTORY")?;
    let directory = std::path::Path::new(&directory);
    std::fs::create_dir_all(directory)?;
    for (target, stem) in [
        (ElfTarget::X86_64, "x86_64"),
        (ElfTarget::Aarch64, "aarch64"),
    ] {
        let artifact = ObjectArtifact::emit(target, IsaProfileKind::Baseline128)?;
        std::fs::write(directory.join(format!("{stem}.o")), artifact.bytes())?;
        std::fs::write(directory.join(format!("{stem}.clif")), artifact.clif())?;
        std::fs::write(
            directory.join(format!("{stem}.metadata")),
            format!("{:#?}\n", artifact.metadata()),
        )?;
    }
    Ok(())
}

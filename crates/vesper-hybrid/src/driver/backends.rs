//! The other backends' steps (#1895): WebAssembly, eBPF and the verified
//! code generator.
//!
//! One of the per-group halves of `Session::step` (split for #1896 so the
//! lanes that move these steps to Vesper touch only their own file).

use super::*;

impl Session {
    pub(super) fn backends_step(&mut self, step: Step) -> Result<i64, String> {
        let o = &self.o;
        match step {
            Step::EbpfBuild => {
                // Compiler eBPF target (§27): verified BPF ELF object with BTF.
                let summary = vesper::ebpf_backend::build_command(
                    o.input(),
                    &self.original,
                    o.entry(),
                    o.output().as_deref(),
                )?;
                print!("{summary}");
                Ok(0)
            }
            Step::VerifiedBuild => {
                let world = &self.checked()?.world;
                vesper::dyn_trait::refuse(world, "verified code generator")
                    .map_err(|d| format!("{}: {}", d.code, d.message))?;
                let path = o.output().unwrap_or_else(|| PathBuf::from("a.out"));
                println!(
                    "{}",
                    vesper::verified_codegen::build_command(
                        world,
                        o.entry(),
                        o.target(),
                        o.optimize,
                        o.linker(),
                        &path,
                    )?
                );
                Ok(0)
            }
            Step::WasmBuild => {
                let world = &self.checked()?.world;
                vesper::dyn_trait::refuse(world, "WebAssembly backend")
                    .map_err(|d| format!("{}: {}", d.code, d.message))?;
                let options = vesper::wasm_backend::WasmOptions {
                    optimize: o.optimize,
                    ..Default::default()
                };
                let artifact = vesper::wasm_backend::compile(world, o.entry(), &options)
                    .map_err(|e| e.to_string())?;
                let path = o.output().unwrap_or_else(|| PathBuf::from("a.wasm"));
                vesper::atomic_file::atomic_write(&path, &artifact.component)?;
                println!(
                    "built {} ({} component)",
                    path.display(),
                    vesper::wasm_backend::TARGET
                );
                Ok(0)
            }
            _ => Err(format!(
                "driver: step {} is not a backends step",
                step as u32
            )),
        }
    }
}

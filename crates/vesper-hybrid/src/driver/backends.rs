//! The other backends' steps (#1895): WebAssembly, eBPF and the verified
//! code generator.
//!
//! One of the per-group halves of `Session::step` (split for #1896 so the
//! lanes that move these steps to Vesper touch only their own file).

use std::sync::atomic::Ordering::Relaxed;

use super::*;

impl Session {
    pub(super) fn backends_step(&mut self, step: Step, arg: &[u8]) -> Result<i64, String> {
        let o = &self.o;
        match step {
            Step::EbpfBuild => {
                if self.ebpf_input {
                    crate::vesper_hybrid::DRIVER_EBPF_FALLBACKS.fetch_add(1, Relaxed);
                }
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
                if self.verified_input {
                    crate::vesper_hybrid::DRIVER_VERIFIED_FALLBACKS.fetch_add(1, Relaxed);
                }
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
                if self.wasm_input {
                    crate::vesper_hybrid::DRIVER_WASM_FALLBACKS.fetch_add(1, Relaxed);
                }
                let world = &self.checked()?.world;
                vesper::dyn_trait::refuse(world, "WebAssembly backend")
                    .map_err(|d| format!("{}: {}", d.code, d.message))?;
                let options = vesper::wasm_backend::WasmOptions {
                    optimize: o.optimize,
                    ..Default::default()
                };
                let compile = || vesper::wasm_backend::compile(world, o.entry(), &options);
                // `[1]`: the Vesper driver's backend already declined this
                // input, and the `wasm` swap would decline it again; the
                // Rust backend runs without it (the other hooks stay).
                let artifact = if self.wasm_input && arg == [1] {
                    let hooks = vesper::codegen_swap::CodegenHooks {
                        wasm: None,
                        ..vesper::codegen_swap::installed()
                    };
                    vesper::codegen_swap::with_hooks(hooks, compile)
                } else {
                    compile()
                }
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
            Step::WasmInput => {
                // The Vesper driver builds the component itself from these
                // words: the world, its Place IR and the wasm section, as
                // the `wasm` swap encodes them. 0 leaves the build to
                // `WasmBuild` (the swap is off, or the world does not
                // encode).
                let world = &self.checked()?.world;
                vesper::dyn_trait::refuse(world, "WebAssembly backend")
                    .map_err(|d| format!("{}: {}", d.code, d.message))?;
                let options = vesper::wasm_backend::WasmOptions {
                    optimize: o.optimize,
                    ..Default::default()
                };
                match crate::vesper_hybrid::wasm_build_input(world, o.entry(), &options) {
                    Some(input) => {
                        self.text = input;
                        self.wasm_input = true;
                        crate::vesper_hybrid::DRIVER_WASM_INPUTS.fetch_add(1, Relaxed);
                        Ok(1)
                    }
                    None => Ok(0),
                }
            }
            Step::VerifiedInput => {
                // The Vesper driver builds and writes the executable itself
                // from the `verified_codegen` swap's input; 0 leaves the
                // build to `VerifiedBuild` (the swap is off, the target is
                // not a Linux ELF one, or the program does not encode).
                let world = &self.checked()?.world;
                vesper::dyn_trait::refuse(world, "verified code generator")
                    .map_err(|d| format!("{}: {}", d.code, d.message))?;
                match crate::vesper_hybrid::verified_build_input(
                    world,
                    o.entry(),
                    o.target(),
                    o.optimize,
                ) {
                    Some(input) => {
                        self.text = input;
                        self.verified_input = true;
                        crate::vesper_hybrid::DRIVER_VERIFIED_INPUTS.fetch_add(1, Relaxed);
                        Ok(1)
                    }
                    None => Ok(0),
                }
            }
            Step::EbpfInput => {
                // The Vesper driver compiles, verifies and writes the
                // object itself from the `ebpf` swap's input; 0 leaves the
                // build to `EbpfBuild`.
                match crate::vesper_hybrid::ebpf_build_input(o.input(), &self.original, o.entry()) {
                    Some(input) => {
                        self.text = input;
                        self.ebpf_input = true;
                        crate::vesper_hybrid::DRIVER_EBPF_INPUTS.fetch_add(1, Relaxed);
                        Ok(1)
                    }
                    None => Ok(0),
                }
            }
            _ => Err(format!(
                "driver: step {} is not a backends step",
                step as u32
            )),
        }
    }
}

//! The IR and native steps (#1894): optimize, the IR, CLIF and Lean
//! emitters, the native object, the Windows link, `run` and the JIT.
//!
//! One of the per-group halves of `Session::step` (split for #1896 so the
//! lanes that move these steps to Vesper touch only their own file).

use super::*;

/// `EmitIr`'s flag bit: run the IR optimizer (`emit_optimize` in
/// modules/driver/src/lib.ves).
const EMIT_OPTIMIZE: u8 = 1;

impl Session {
    pub(super) fn native_step(&mut self, step: Step, arg: &[u8]) -> Result<i64, String> {
        let o = &self.o;
        match step {
            Step::Optimize => {
                let expression_types = &self
                    .checked
                    .as_ref()
                    .ok_or("driver: no check yet")?
                    .expression_types;
                let parsed = self.parsed.as_mut().ok_or("driver: no parse yet")?;
                optimize::optimize(&mut parsed.module, expression_types);
                let checked = check::check(&parsed.module);
                if !checked.diagnostics.is_empty() {
                    return Err(format!(
                        "optimizer validation failed: {:?}",
                        checked.diagnostics
                    ));
                }
                self.checked = Some(checked);
                Ok(0)
            }
            Step::Run => {
                let world = &self.checked()?.world;
                let entry = o.entry();
                // `entry`: the entry's shape, which the Vesper driver checks
                // (`run_entry_*` in modules/driver/src/lib.ves).
                if arg == b"entry" {
                    return Ok(match world.functions.get(entry) {
                        None => 1,
                        Some(f) if !f.params.is_empty() => 2,
                        Some(f) if !f.effects.instances.is_empty() || f.effects.tail.is_some() => 3,
                        Some(_) => 0,
                    });
                }
                let mut runtime = Runtime::new(world);
                runtime.fuel = o.fuel;
                runtime.optimize = o.optimize;
                println!("{}", runtime.run(entry, vec![])?);
                Ok(0)
            }
            Step::Jit => {
                println!(
                    "{}",
                    native::jit_with_options(&self.checked()?.world, o.entry(), o.optimize)?
                );
                Ok(0)
            }
            Step::NativeObject => {
                let (bytes, _) = self.native_code()?;
                self.text = bytes;
                Ok(0)
            }
            Step::WindowsLink => {
                // `<object>\0<executable>\0<linker>`: the driver chose the
                // executable and the linker (`=<path>`, or empty to find the
                // MSVC linker).
                let mut parts = utf8(arg)?.split('\0');
                let (Some(object), Some(path), Some(linker), None) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    return Err("driver: bad link request".into());
                };
                let explicit = linker.strip_prefix('=').map(Path::new);
                let path =
                    native::windows::link_executable(Path::new(object), Path::new(path), explicit)?;
                self.text = path.display().to_string().into_bytes();
                Ok(0)
            }
            Step::EmitIr => {
                // The flag byte: optimize, as the driver decided.
                let (&flag, out) = arg.split_first().ok_or("driver: bad emit request")?;
                let checked = self.checked()?;
                let mut program = ir::lower(checked);
                if flag & EMIT_OPTIMIZE != 0 {
                    // The native path's optimizer, which the hybrid swaps
                    // for `@vesper/ir-opt` (`modules/ir_opt`).
                    vesper::ir_swap::optimize(&checked.world, &mut program)?;
                }
                let text = serde_json::to_string_pretty(&program).map_err(|e| e.to_string())?;
                self.emitted(out, text.into_bytes())
            }
            Step::EmitLean => {
                let text = lean::extract(&self.checked()?.world)?;
                self.emitted(arg, text.into_bytes())
            }
            Step::EmitClif => {
                let (_, clif) = self.native_code()?;
                self.emitted(arg, clif.into_bytes())
            }
            _ => Err(format!("driver: step {} is not a native step", step as u32)),
        }
    }

    /// An emitted text: printed to stdout for `stdout` (`emit_stdout` in
    /// the driver, as `cli()`'s `write(None, ..)` does, failing on a write
    /// error), else kept as the session's text for the driver to write.
    fn emitted(&mut self, to: &[u8], bytes: Vec<u8>) -> Result<i64, String> {
        match to {
            b"stdout" => crate::write(None, &bytes)?,
            b"" => self.text = bytes,
            _ => return Err("driver: bad emit request".into()),
        }
        Ok(0)
    }

    fn native_code(&self) -> Result<(Vec<u8>, String), String> {
        crate::native_code(
            &self.checked()?.world,
            self.o.entry(),
            self.o.target(),
            self.o.optimize,
            &self.o.verifiers,
            self.o.policy(),
            (self.o.input(), &self.source),
        )
    }
}

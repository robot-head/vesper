//! The IR and native steps (#1894): optimize, the IR, CLIF and Lean
//! emitters, the native object, the Windows link, `run` and the JIT.
//!
//! One of the per-group halves of `Session::step` (split for #1896 so the
//! lanes that move these steps to Vesper touch only their own file).

use super::*;

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
                let f = world.functions.get(entry).ok_or("missing entry function")?;
                if !f.params.is_empty() {
                    return Err("entry function must take no arguments".into());
                }
                if !f.effects.instances.is_empty() || f.effects.tail.is_some() {
                    return Err("entry must handle all effects".into());
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
                let object = PathBuf::from(utf8(arg)?);
                let path = o.output().unwrap_or_else(|| PathBuf::from("a.out"));
                let linker = o.linker();
                let explicit = (linker != "cc").then(|| PathBuf::from(linker));
                let path = native::windows::link_executable(&object, &path, explicit.as_deref())?;
                self.text = path.display().to_string().into_bytes();
                Ok(0)
            }
            Step::EmitIr => {
                let checked = self.checked()?;
                let mut program = ir::lower(checked);
                if o.optimize {
                    vesper::ir_opt_policy::optimize(&checked.world, &mut program)?;
                }
                crate::write(
                    o.output(),
                    serde_json::to_string_pretty(&program)
                        .map_err(|e| e.to_string())?
                        .as_bytes(),
                )?;
                Ok(0)
            }
            Step::EmitLean => {
                crate::write(
                    o.output(),
                    lean::extract(&self.checked()?.world)?.as_bytes(),
                )?;
                Ok(0)
            }
            Step::EmitClif => {
                let (_, clif) = self.native_code()?;
                crate::write(o.output(), clif.as_bytes())?;
                Ok(0)
            }
            _ => Err(format!("driver: step {} is not a native step", step as u32)),
        }
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

//! The IR and native steps (#1894): optimize, the IR, CLIF and Lean
//! emitters, the native object, the Windows link, `run` and the JIT.
//!
//! One of the per-group halves of `Session::step` (split for #1896 so the
//! lanes that move these steps to Vesper touch only their own file).

use super::*;

/// `EmitIr`'s flag bit: run the IR optimizer (`emit_optimize` in
/// modules/driver/src/lib.ves).
const EMIT_OPTIMIZE: u8 = 1;

// The ops of a native code request, `NativeObject`'s and `EmitClif`'s
// argument `[op, has_policy, policy..]` (`native_*` in
// modules/driver/src/lib.ves).
/// The code, unverified.
const NATIVE_PLAIN: u8 = 0;
/// Whether the code needs the native verifiers: 1 or 0.
const NATIVE_NEEDS_VERIFY: u8 = 1;
/// The verified code: the report record, then the code.
const NATIVE_VERIFIED: u8 = 2;

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
                // The Vesper driver checked the entry's shape over the world
                // it holds (`run` in modules/driver/src/lib.ves, #2024).
                let world = &self.checked()?.world;
                let entry = o.entry();
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
            Step::NativeObject => self.native_request(arg, false),
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
                let [flag] = *arg else {
                    return Err("driver: bad emit request".into());
                };
                let checked = self.checked()?;
                let mut program = ir::lower(checked);
                if flag & EMIT_OPTIMIZE != 0 {
                    // The native path's optimizer, which the hybrid swaps
                    // for `@vesper/ir-opt` (`modules/ir_opt`).
                    vesper::ir_swap::optimize(&checked.world, &mut program)?;
                }
                let text = serde_json::to_string_pretty(&program).map_err(|e| e.to_string())?;
                self.text = text.into_bytes();
                Ok(0)
            }
            Step::EmitLean => {
                self.text = lean::extract(&self.checked()?.world)?.into_bytes();
                Ok(0)
            }
            Step::EmitClif => self.native_request(arg, true),
            _ => Err(format!("driver: step {} is not a native step", step as u32)),
        }
    }

    /// A native code request (see `NATIVE_PLAIN`): the object, or the CLIF
    /// text for `clif`. The Vesper driver decides whether the verifiers
    /// run, reads the policy file, prints the report and decides the
    /// outcome; this computes.
    fn native_request(&mut self, arg: &[u8], clif: bool) -> Result<i64, String> {
        let bad = || "driver: bad native code request".to_owned();
        let [op, has_policy, ref policy @ ..] = *arg else {
            return Err(bad());
        };
        let o = &self.o;
        let world = &self.checked()?.world;
        match op {
            NATIVE_NEEDS_VERIFY => Ok(i64::from(
                !o.verifiers.is_empty()
                    || !vesper::native_verifiers::capability_verifiers(world).is_empty(),
            )),
            NATIVE_PLAIN => {
                let (bytes, text) =
                    native::object_with_options(world, o.entry(), o.target(), o.optimize)?;
                self.text = if clif { text.into_bytes() } else { bytes };
                Ok(0)
            }
            NATIVE_VERIFIED => {
                use vesper::native_verifiers as nv;
                let policy = match has_policy {
                    0 => nv::NativeVerifierPolicy {
                        schema: nv::POLICY_SCHEMA.into(),
                        ..Default::default()
                    },
                    _ => nv::NativeVerifierPolicy::parse(utf8(policy)?)?,
                };
                let (bytes, text, report) = nv::verify_native_with_capabilities(
                    world,
                    o.entry(),
                    o.target(),
                    o.optimize,
                    &o.verifiers,
                    &policy,
                )?;
                let rendered = nv::render(&report, o.input(), &self.source);
                let names: Vec<_> = report.verifiers.iter().map(|v| v.name()).collect();
                let mut record = vec![];
                let mut put = |b: &[u8]| {
                    record.extend((b.len() as u64).to_le_bytes());
                    record.extend(b);
                };
                put(rendered.as_bytes());
                put(names.join(", ").as_bytes());
                for w in [
                    u64::from(report.accepted()),
                    report.findings.len() as u64,
                    report.functions as u64,
                ] {
                    record.extend(w.to_le_bytes());
                }
                record.extend(if clif { text.into_bytes() } else { bytes });
                self.text = record;
                Ok(0)
            }
            _ => Err(bad()),
        }
    }
}

//! Host-side typestate accessor generation from a [`DeviceModel`]
//! (§26.1 item 4).
//!
//! [`generate_rust`] renders Rust source text: one module per peripheral
//! with its name and base address, one module per register with its path,
//! absolute address, reset value and per-field `*_SHIFT` / `*_MASK`
//! constants, and a peripheral singleton struct whose fields are
//! [`Reg<Marker>`](crate::mmio::Reg)s typed by the register's
//! [`RegisterClass`]. `take(&Bus)` binds every register (which re-checks the
//! class against the bus's model) and claims the singleton. So a write-1-to-
//! clear status register is a `Reg<Write1ToClear>` with no `modify`, a FIFO
//! data register with `readAction=modify` is a `Reg<ConsumeWrite>` with no
//! plain `read`, and so on: the hazards [`crate::hazard`] reports are not
//! expressible through the generated API.
//!
//! The output targets the software [`SimBus`](crate::mmio::SimBus); it is
//! the shape a later `@svd_pac` Forge macro would emit, not that macro, and
//! it performs no real MMIO.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::svd::{DeviceModel, PeripheralModel, RegisterClass};

/// What to generate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PacOptions {
    /// Path of this crate as seen from the generated code.
    pub crate_path: String,
    /// Peripherals to emit, in this order; `None` emits all, in model order.
    pub peripherals: Option<Vec<String>>,
    /// Free text recorded in the header comment (for example the SVD file's
    /// provenance).
    pub source: String,
}

impl Default for PacOptions {
    fn default() -> Self {
        Self {
            crate_path: "vesper_silicon".to_string(),
            peripherals: None,
            source: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PacError {
    UnknownPeripheral(String),
    /// Two names map to the same Rust identifier in one scope.
    NameCollision {
        scope: String,
        ident: String,
    },
    /// A name that has no identifier characters at all.
    EmptyIdentifier(String),
    /// The peripheral has no registers to bind.
    NoRegisters(String),
}

impl std::fmt::Display for PacError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PacError {}

/// The `mmio` marker type that binds a register of `class`.
pub fn marker_for(class: RegisterClass) -> &'static str {
    match class {
        RegisterClass::ReadOnly => "ReadOnly",
        RegisterClass::WriteOnly => "WriteOnly",
        RegisterClass::ReadToClear => "ReadToClear",
        RegisterClass::ConsumeWrite => "ConsumeWrite",
        RegisterClass::WriteClear => "Write1ToClear",
        RegisterClass::ReadWrite => "ReadWrite",
    }
}

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
    "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe",
    "use", "where", "while", "abstract", "become", "box", "do", "final", "macro", "override",
    "priv", "try", "typeof", "unsized", "virtual", "yield",
];

fn ident(raw: &str, upper: bool) -> Result<String, PacError> {
    let mut s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                if upper {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                }
            } else {
                '_'
            }
        })
        .collect();
    if s.trim_matches('_').is_empty() {
        return Err(PacError::EmptyIdentifier(raw.to_string()));
    }
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, if upper { 'R' } else { 'r' });
    }
    if !upper && KEYWORDS.contains(&s.as_str()) {
        s.push('_');
    }
    Ok(s)
}

fn type_name(raw: &str) -> Result<String, PacError> {
    let snake = ident(raw, false)?;
    let mut out = String::new();
    let mut up = true;
    for c in snake.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.push(c.to_ascii_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    if out.is_empty() {
        return Err(PacError::EmptyIdentifier(raw.to_string()));
    }
    if out == "Self" {
        out.push('_');
    }
    Ok(out)
}

fn unique(seen: &mut BTreeSet<String>, scope: &str, id: String) -> Result<String, PacError> {
    if !seen.insert(id.clone()) {
        return Err(PacError::NameCollision {
            scope: scope.to_string(),
            ident: id,
        });
    }
    Ok(id)
}

/// Renders the accessor modules. The output is deterministic, so a checked-in
/// copy can be compared against a fresh generation.
pub fn generate_rust(model: &DeviceModel, options: &PacOptions) -> Result<String, PacError> {
    let selected: Vec<&PeripheralModel> = match &options.peripherals {
        None => model.peripherals.iter().collect(),
        Some(names) => names
            .iter()
            .map(|n| {
                model
                    .peripheral(n)
                    .ok_or_else(|| PacError::UnknownPeripheral(n.clone()))
            })
            .collect::<Result<_, _>>()?,
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// @generated by vesper_silicon::pac::generate_rust from the CMSIS-SVD model of {}. Do not edit.",
        model.name
    );
    if !options.source.is_empty() {
        for line in options.source.lines() {
            let _ = writeln!(out, "// {line}");
        }
    }
    let mut top = BTreeSet::new();
    for p in selected {
        if p.registers.is_empty() {
            return Err(PacError::NoRegisters(p.name.clone()));
        }
        let pmod = unique(&mut top, &model.name, ident(&p.name, false)?)?;
        let ty = type_name(&p.name)?;
        let _ = writeln!(out);
        let _ = writeln!(out, "/// Peripheral `{}`.", p.name);
        let _ = writeln!(out, "pub mod {pmod} {{");
        let _ = writeln!(out, "    use {}::mmio;", options.crate_path);
        let _ = writeln!(out);
        let _ = writeln!(out, "    pub const NAME: &str = {:?};", p.name);
        let _ = writeln!(
            out,
            "    pub const BASE_ADDRESS: u64 = {:#x};",
            p.base_address
        );
        let mut scope = BTreeSet::from([
            "NAME".to_string(),
            "BASE_ADDRESS".to_string(),
            "mmio".to_string(),
        ]);
        let mut fields = Vec::new();
        for r in &p.registers {
            let local = r
                .path
                .strip_prefix(&format!("{}.", p.name))
                .unwrap_or(&r.path);
            let rmod = unique(&mut scope, &p.name, ident(local, false)?)?;
            let class = r.class();
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "    /// `{}`: {}-bit, {:?}.",
                r.path, r.size_bits, class
            );
            let _ = writeln!(out, "    pub mod {rmod} {{");
            let _ = writeln!(out, "        pub const PATH: &str = {:?};", r.path);
            let _ = writeln!(out, "        pub const ADDRESS: u64 = {:#x};", r.address);
            let _ = writeln!(out, "        pub const RESET: u64 = {:#x};", r.reset_value);
            let mut fscope = BTreeSet::from([
                "PATH".to_string(),
                "ADDRESS".to_string(),
                "RESET".to_string(),
            ]);
            for f in &r.fields {
                let fid = ident(&f.name, true)?;
                let shift = unique(&mut fscope, &r.path, format!("{fid}_SHIFT"))?;
                let mask = unique(&mut fscope, &r.path, format!("{fid}_MASK"))?;
                let _ = writeln!(out, "        /// `{}`: {:?}.", f.name, f.access);
                let _ = writeln!(out, "        pub const {shift}: u32 = {};", f.offset);
                let _ = writeln!(out, "        pub const {mask}: u64 = {:#x};", f.mask());
            }
            let _ = writeln!(out, "    }}");
            fields.push((rmod, marker_for(class)));
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "    /// The `{}` singleton, each register typed by its access class.",
            p.name
        );
        let _ = writeln!(out, "    #[derive(Debug)]");
        let _ = writeln!(out, "    pub struct {ty} {{");
        for (f, m) in &fields {
            let _ = writeln!(out, "        pub {f}: mmio::Reg<mmio::{m}>,");
        }
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out);
        let _ = writeln!(out, "    impl {ty} {{");
        let _ = writeln!(
            out,
            "        /// Binds every register (checking its class against the bus model) and"
        );
        let _ = writeln!(
            out,
            "        /// claims the singleton; a second `take` fails."
        );
        let _ = writeln!(
            out,
            "        pub fn take(bus: &mmio::Bus) -> Result<Self, mmio::BusError> {{"
        );
        let _ = writeln!(out, "            let p = Self {{");
        for (f, _) in &fields {
            let _ = writeln!(
                out,
                "                {f}: mmio::Reg::bind(bus, {f}::PATH)?,"
            );
        }
        let _ = writeln!(out, "            }};");
        let _ = writeln!(out, "            mmio::claim_peripheral(bus, NAME)?;");
        let _ = writeln!(out, "            Ok(p)");
        let _ = writeln!(out, "        }}");
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out, "}}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svd::{FieldAccess, FieldModel, RegisterModel};

    fn reg(path: &str, offset: u32) -> RegisterModel {
        RegisterModel {
            path: path.to_string(),
            name: path.rsplit('.').next().unwrap().to_string(),
            offset,
            address: u64::from(offset),
            size_bits: 32,
            reset_value: 0,
            fields: vec![FieldModel {
                name: "type".into(),
                offset: 0,
                width: 1,
                access: FieldAccess::ReadWrite,
            }],
        }
    }

    fn device(regs: Vec<RegisterModel>) -> DeviceModel {
        DeviceModel {
            name: "D".into(),
            peripherals: vec![PeripheralModel {
                name: "P".into(),
                base_address: 0,
                registers: regs,
            }],
        }
    }

    #[test]
    fn identifiers_are_sanitized() {
        assert_eq!(ident("type", false).unwrap(), "type_");
        assert_eq!(ident("0CTRL", false).unwrap(), "r0ctrl");
        assert_eq!(ident("A[3]", true).unwrap(), "A_3_");
        assert!(ident("[]", false).is_err());
        assert_eq!(type_name("USB_DEVICE_DPRAM").unwrap(), "UsbDeviceDpram");
        assert_eq!(type_name("self").unwrap(), "Self_");
        let out = generate_rust(&device(vec![reg("P.self", 0)]), &PacOptions::default()).unwrap();
        assert!(out.contains("pub mod self_ {"), "{out}");
        assert!(out.contains("pub const TYPE_MASK: u64 = 0x1;"), "{out}");
    }

    #[test]
    fn collisions_and_unknown_peripherals_are_errors() {
        let d = device(vec![reg("P.A-B", 0), reg("P.A_B", 4)]);
        assert!(matches!(
            generate_rust(&d, &PacOptions::default()),
            Err(PacError::NameCollision { .. })
        ));
        let d = device(vec![reg("P.mmio", 0)]);
        assert!(matches!(
            generate_rust(&d, &PacOptions::default()),
            Err(PacError::NameCollision { .. })
        ));
        let opts = PacOptions {
            peripherals: Some(vec!["Q".into()]),
            ..PacOptions::default()
        };
        assert_eq!(
            generate_rust(&device(vec![reg("P.X", 0)]), &opts),
            Err(PacError::UnknownPeripheral("Q".into()))
        );
        assert_eq!(
            generate_rust(&device(vec![]), &PacOptions::default()),
            Err(PacError::NoRegisters("P".into()))
        );
    }
}

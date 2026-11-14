//! Source `wire struct` declarations, checked by the Vesper Bridge layout
//! checker (`vesper_ffi::Registry`, §13.2), and their binding to the
//! quantized formats of the host kernels (§28.2).
use super::{BYTE_ARRAY, ZERO_PAD, wire_integer};
use crate::ast::*;
use std::collections::{BTreeMap, BTreeSet};
use vesper_ffi::{Declaration, Endian, Field, FieldKind, Integer, Registry, WireLayout, WireType};

/// Quantized block formats with host kernels in `vesper-tensor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuantFormat {
    /// 32 4-bit weights in 16 bytes after an fp16 scale (18 bytes).
    Q4_0,
    /// 32 signed 8-bit weights after an fp16 scale (34 bytes).
    Q8_0,
}
impl QuantFormat {
    pub fn block_bytes(self) -> usize {
        match self {
            Self::Q4_0 => vesper_tensor::quant::Q4_0_BYTES,
            Self::Q8_0 => vesper_tensor::quant::Q8_0_BYTES,
        }
    }
    pub fn weight_format(self) -> vesper_tensor::quant::WeightFormat {
        match self {
            Self::Q4_0 => vesper_tensor::quant::WeightFormat::Q4_0,
            Self::Q8_0 => vesper_tensor::quant::WeightFormat::Q8_0,
        }
    }
}
/// Largest byte array or padding field (the `.vsys` parser's bound).
pub const MAX_WIRE_BYTES: usize = 1 << 20;
/// Elements per quantized block.
pub const BLOCK_ELEMENTS: u64 = vesper_tensor::quant::BLOCK as u64;

/// A checked wire struct.
#[derive(Clone, Debug)]
pub struct WireStruct {
    pub layout: WireLayout,
    /// The quantized format it lays out, if any.
    pub format: Option<QuantFormat>,
    pub span: Span,
}

/// Whether a field type is only meaningful in a wire struct.
fn wire_only(t: &Type) -> bool {
    match t {
        Type::Named(n, a) => {
            (a.is_empty() && wire_integer(n).is_some()) || n == BYTE_ARRAY || n == ZERO_PAD
        }
        _ => false,
    }
}

/// The structs of `module` declared with wire field types.
pub fn wire_struct_names(module: &Module) -> BTreeSet<String> {
    module
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Adt(a)
                if !a.is_enum
                    && a.variants
                        .iter()
                        .flat_map(|v| &v.fields)
                        .any(|f| wire_only(&f.ty)) =>
            {
                Some(a.name.clone())
            }
            _ => None,
        })
        .collect()
}

fn field_type(t: &Type, wires: &BTreeSet<String>) -> Result<WireType, String> {
    let bytes = |a: &[Type]| -> Result<usize, String> {
        match a {
            [Type::Named(n, x)] if x.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => n
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=MAX_WIRE_BYTES).contains(n))
                .ok_or_else(|| format!("byte array length must be 1..={}", MAX_WIRE_BYTES)),
            _ => Err("ByteArray and ZeroPad take one decimal length, as in ByteArray[16]".into()),
        }
    };
    match t {
        Type::Named(n, a) if a.is_empty() => {
            if let Some(i) = wire_integer(n) {
                return Ok(WireType::Integer(i));
            }
            Ok(match n.as_str() {
                "u8" => WireType::Integer(Integer::unsigned(8, Endian::Native)),
                "u32" => WireType::Integer(Integer::unsigned(32, Endian::Native)),
                "u64" => WireType::Integer(Integer::unsigned(64, Endian::Native)),
                "i64" => WireType::Integer(Integer::signed(64, Endian::Native)),
                "bool" => WireType::Bool,
                _ if wires.contains(n) => WireType::Struct(n.clone()),
                _ => {
                    return Err(format!(
                        "wire struct fields must admit every bit pattern: fixed-width integers, ByteArray[N], ZeroPad[N] or wire structs, not '{n}'"
                    ));
                }
            })
        }
        Type::Named(n, a) if n == BYTE_ARRAY => Ok(WireType::ByteArray(bytes(a)?)),
        Type::Named(n, a) if n == ZERO_PAD => Ok(WireType::ZeroPad(bytes(a)?)),
        other => Err(format!(
            "'{}' is not a wire field type",
            super::types::display_type(other)
        )),
    }
}

/// Check every wire struct with the Bridge layout checker. Errors carry the
/// declaration's span.
pub fn check(module: &Module) -> (BTreeMap<String, WireStruct>, Vec<Diagnostic>) {
    let names = wire_struct_names(module);
    let mut diagnostics = vec![];
    let mut declarations = vec![];
    let mut spans = BTreeMap::new();
    for item in &module.items {
        let Item::Adt(a) = item else { continue };
        if !names.contains(&a.name) {
            continue;
        }
        spans.insert(a.name.clone(), a.span.clone());
        if a.mode != Mode::Copy || !a.generics.is_empty() {
            diagnostics.push(Diagnostic::new(
                super::codes::WIRE,
                format!(
                    "wire struct '{}' must be a non-generic copy struct (write `wire struct`)",
                    a.name
                ),
                a.span.clone(),
            ));
            continue;
        }
        let mut fields = vec![];
        for f in a.variants.iter().flat_map(|v| &v.fields) {
            match field_type(&f.ty, &names) {
                Ok(t) => fields.push(Field::new(f.name.clone(), t)),
                Err(e) => diagnostics.push(Diagnostic::new(
                    super::codes::WIRE,
                    format!("wire struct '{}' field '{}': {e}", a.name, f.name),
                    a.span.clone(),
                )),
            }
        }
        declarations.push(Declaration::new(a.name.clone(), fields));
    }
    if !diagnostics.is_empty() {
        return (BTreeMap::new(), diagnostics);
    }
    let registry = match Registry::new(declarations) {
        Ok(r) => r,
        Err(e) => {
            let span = spans.get(&e.record).cloned().unwrap_or(0..0);
            return (
                BTreeMap::new(),
                vec![Diagnostic::new(super::codes::WIRE, e.to_string(), span)],
            );
        }
    };
    let mut out = BTreeMap::new();
    for (name, span) in spans {
        match registry.compile(&name) {
            Ok(layout) => {
                let format = quant_format(&layout);
                out.insert(
                    name,
                    WireStruct {
                        layout,
                        format,
                        span,
                    },
                );
            }
            Err(e) => diagnostics.push(Diagnostic::new(super::codes::WIRE, e.to_string(), span)),
        }
    }
    (out, diagnostics)
}

/// The quantized format a layout matches: a little-endian 16-bit scale at
/// offset 0 followed by the packed weights, with no other bytes.
pub fn quant_format(layout: &WireLayout) -> Option<QuantFormat> {
    let [scale, weights] = layout.fields() else {
        return None;
    };
    let scale_ok = scale.offset() == 0
        && matches!(
            scale.kind(),
            FieldKind::Integer(Integer {
                bits: 16,
                signed: false,
                endian: Endian::Little,
            })
        );
    if !scale_ok || *weights.kind() != FieldKind::Bytes || weights.offset() != 2 {
        return None;
    }
    [QuantFormat::Q4_0, QuantFormat::Q8_0]
        .into_iter()
        .find(|f| f.block_bytes() == layout.size() && weights.size() + 2 == layout.size())
}

/// Why a wire struct cannot serve as a quantized block.
pub fn format_mismatch(name: &str, wire: &WireStruct) -> String {
    format!(
        "wire struct '{name}' ({} bytes) is not a quantized block layout: expected `scale: u16le` then `ByteArray[16]` (Q4_0, 18 bytes) or `ByteArray[32]` (Q8_0, 34 bytes)",
        wire.layout.size()
    )
}

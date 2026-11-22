//! The evaluator's host for foreign calls (`vesper run`, manuscript §16.3):
//! source values -> the `.vsys` C ABI -> a host function in this process ->
//! source values. Every layout comes from the library's descriptors: wire
//! structs field by field, slices element by element, `CStr` arguments as
//! terminated copies, `errno` results paired with the OS error code.
//!
//! A host function is called through a signature of 64-bit integer words,
//! which every `.vsys` parameter is in the C ABI (an integer, a pointer, or a
//! pointer and a `size_t` length); at most ten words, so a variadic call
//! padded for Apple arm64 (`sys::Variadic`) still fits.
use super::{EvalCall, Handler, INVALID_TYPE, Imports, handle_value, handle_word};
use crate::runtime::Value;
use std::collections::BTreeMap;
use vesper_ffi::sys::{
    Access, FunctionDescriptor, ParameterType, Pointee, PointerAccess, Scalar, ValueType,
};
use vesper_ffi::{Endian, FieldKind, WireLayout};

/// Most argument words a host function may take.
pub const MAX_WORDS: usize = 10;

struct Foreign {
    descriptor: FunctionDescriptor,
    address: usize,
}

/// Runs the evaluator's foreign calls against host functions bound by
/// symbol ([`super::ForeignChecked::evaluate`]).
pub struct EvalHost {
    /// By (library, symbol): two libraries may declare the same symbol.
    functions: BTreeMap<(String, String), Foreign>,
}

/// One argument's C memory, kept alive across the call.
enum Slot {
    None,
    /// A slice or pointee buffer (8-byte aligned), and the element count of
    /// a slice. `None` when a nullable argument was `None`.
    Buffer(Option<(Vec<u64>, usize)>),
}

impl EvalHost {
    /// A host for every function of `imports`, each bound to the address
    /// `symbols` names for it.
    pub fn new(imports: &Imports, symbols: &[(&str, *const u8)]) -> Result<Self, String> {
        let mut functions = BTreeMap::new();
        for (library, handler) in imports.libraries() {
            // An `isolated(mpk)` grant is a trampoline only objects emit; a
            // direct call here would drop the isolation it requires.
            if handler == Handler::IsolatedMpk {
                return Err(format!(
                    "{} is imported isolated(mpk), which only `vesper build` objects enforce; the evaluator calls libraries directly",
                    library.name()
                ));
            }
            for f in library.functions() {
                // `@vesper-std/cpu`'s instructions exist only in native code.
                if super::cpu::function(f.symbol()).is_some() {
                    continue;
                }
                // A callback type's call goes through the pointer it is
                // handed, so it binds no host function.
                let address = if f.is_indirect() {
                    0
                } else {
                    symbols
                        .iter()
                        .find(|(n, _)| *n == f.link_symbol())
                        .ok_or_else(|| {
                            format!("{}::{} has no host function", library.name(), f.symbol())
                        })?
                        .1 as usize
                };
                let words = f.abi().parameters.len();
                if words > MAX_WORDS {
                    return Err(format!(
                        "{}::{} takes {words} argument words; the evaluator's host calls take at most {MAX_WORDS}",
                        library.name(),
                        f.symbol()
                    ));
                }
                functions.insert(
                    (library.name().to_owned(), f.symbol().to_owned()),
                    Foreign {
                        descriptor: f.clone(),
                        address,
                    },
                );
            }
        }
        Ok(Self { functions })
    }

    /// Make one foreign call.
    pub fn call(&self, call: &mut EvalCall<'_, '_>) -> Result<Value, String> {
        let f = self
            .functions
            .get(&(call.library.clone(), call.symbol.clone()))
            .ok_or_else(|| {
                if super::cpu::function(&call.symbol).is_some() {
                    format!(
                        "{}::{} is a CPU instruction, which only native code runs; the evaluator has none",
                        call.library, call.symbol
                    )
                } else {
                    format!("no host function for {}::{}", call.library, call.symbol)
                }
            })?;
        let d = &f.descriptor;
        let args = call.arguments().to_vec();
        let params = d.parameters();
        if args.len() != params.len() {
            return Err(format!("{}: {} arguments", call.symbol, args.len()));
        }
        // First pass: marshal into owned buffers; addresses come after,
        // once no buffer moves.
        let mut slots = vec![];
        for (p, v) in params.iter().zip(&args) {
            let slot = match p.ty() {
                ParameterType::Value(_) | ParameterType::Handle { .. } => Slot::None,
                ParameterType::CStr => {
                    let bytes = elements(v)?
                        .iter()
                        .map(|b| int(b).map(|b| b as u8))
                        .collect::<Result<Vec<u8>, _>>()?;
                    if let Some(at) = bytes.iter().position(|b| *b == 0) {
                        // Refused: `Err(ForeignInvalid { raw: offset })`.
                        return Ok(result(
                            "Err",
                            data(
                                INVALID_TYPE,
                                INVALID_TYPE,
                                vec![("raw", Value::UInt(at as u64, 64))],
                            ),
                        ));
                    }
                    let mut buf = vec![0u64; (bytes.len() + 1).div_ceil(8)];
                    as_bytes(&mut buf)[..bytes.len()].copy_from_slice(&bytes);
                    Slot::Buffer(Some((buf, bytes.len())))
                }
                ParameterType::Slice {
                    element, nullable, ..
                } => match option(v, *nullable)? {
                    None => Slot::Buffer(None),
                    Some(v) => {
                        let items = elements(v)?;
                        let size = element.size();
                        let mut buf = vec![0u64; (items.len() * size).div_ceil(8).max(1)];
                        for (i, item) in items.iter().enumerate() {
                            encode(element, item, &mut as_bytes(&mut buf)[i * size..])?;
                        }
                        Slot::Buffer(Some((buf, items.len())))
                    }
                },
                ParameterType::Pointer {
                    pointee, nullable, ..
                } => match option(v, *nullable)? {
                    None => Slot::Buffer(None),
                    Some(v) => {
                        let mut buf = vec![0u64; pointee.size().div_ceil(8).max(1)];
                        encode(pointee, v, as_bytes(&mut buf))?;
                        Slot::Buffer(Some((buf, 1)))
                    }
                },
            };
            slots.push(slot);
        }
        let mut words = vec![];
        for ((p, v), slot) in params.iter().zip(&args).zip(&mut slots) {
            match (p.ty(), slot) {
                (ParameterType::Value(ValueType::Enum(e)), _) => {
                    // An enum crosses as its declared discriminant.
                    let Value::Data { constructor, .. } = v else {
                        return Err(format!("not a {}: {v:?}", e.name()));
                    };
                    let variant = constructor.rsplit("::").next().unwrap_or(constructor);
                    let (_, word) = e
                        .variants()
                        .iter()
                        .find(|(n, _)| n == variant)
                        .ok_or_else(|| format!("{} has no variant {variant}", e.name()))?;
                    words.push(*word);
                }
                (ParameterType::Value(_), _) => words.push(int(v)?),
                (ParameterType::Handle { .. }, _) => {
                    words.push(handle_word(v).ok_or("not a handle")?);
                }
                (ParameterType::Slice { .. }, Slot::Buffer(Some((buf, n)))) => {
                    words.push(buf.as_mut_ptr() as u64);
                    words.push(*n as u64);
                }
                (ParameterType::Slice { .. }, _) => words.extend([0, 0]),
                (_, Slot::Buffer(Some((buf, _)))) => words.push(buf.as_mut_ptr() as u64),
                (_, _) => words.push(0),
            }
        }
        // A callback type's call: its first word is the C function pointer.
        let address = if d.is_indirect() {
            let callee = words.remove(0);
            if callee == 0 {
                return Err(format!("{}: call of a NULL function pointer", call.symbol));
            }
            callee as usize
        } else {
            f.address
        };
        if let Some(v) = d.abi().variadic {
            words.splice(
                v.first - v.padding..v.first - v.padding,
                std::iter::repeat_n(0, v.padding),
            );
        }
        // SAFETY: `address` is the host function bound to this `.vsys`
        // signature; each parameter is one integer-class word in the C ABI
        // (see the module comment), and every buffer outlives the call.
        let raw = unsafe { call_words(address, &words)? };
        let os_error = d.reads_errno().then(|| {
            std::io::Error::last_os_error()
                .raw_os_error()
                .map_or(0, |e| e as u32)
        });
        // Copy C's writes back into the caller's values.
        for (i, (p, slot)) in params.iter().zip(&mut slots).enumerate() {
            let Slot::Buffer(Some((buf, n))) = slot else {
                continue;
            };
            match p.ty() {
                ParameterType::Slice {
                    element,
                    access: Access::Inout,
                    nullable,
                } => {
                    let size = element.size();
                    let bytes = as_bytes(buf);
                    let items = (0..*n)
                        .map(|k| decode(element, &bytes[k * size..]))
                        .collect::<Result<Vec<_>, _>>()?;
                    match option(&args[i], *nullable)? {
                        Some(Value::Slice { seq, start, len }) => {
                            let mut cell = seq.borrow_mut();
                            let target = cell
                                .get_mut(*start..start + len)
                                .ok_or("slice out of bounds")?;
                            for (t, v) in target.iter_mut().zip(items) {
                                *t = v;
                            }
                        }
                        _ => {
                            let v = Value::Tuple(items);
                            let v = if *nullable { some(v) } else { v };
                            call.write_argument(i, v)?;
                        }
                    }
                }
                ParameterType::Pointer {
                    pointee,
                    access,
                    nullable,
                } if *access != PointerAccess::In => {
                    let v = decode(pointee, as_bytes(buf))?;
                    let v = if *nullable { some(v) } else { v };
                    call.write_argument(i, v)?;
                }
                _ => {}
            }
        }
        let mut value = foreign_value(d.result(), raw)?;
        if let Some(code) = os_error {
            value = Value::Tuple(vec![value, Value::UInt(u64::from(code), 32)]);
        }
        if d.takes_cstr() {
            value = result("Ok", value);
        }
        Ok(value)
    }
}

/// The source value of a raw foreign result (the `ForeignVal` mapping).
fn foreign_value(t: &ValueType, raw: u64) -> Result<Value, String> {
    let invalid = |raw: u64| {
        result(
            "Err",
            data(
                INVALID_TYPE,
                INVALID_TYPE,
                vec![("raw", Value::UInt(raw, 64))],
            ),
        )
    };
    Ok(match t {
        ValueType::Unit => Value::Tuple(vec![]),
        ValueType::Scalar(s) => scalar_value(*s, raw),
        ValueType::Bool => match raw & 0xff {
            0 => result("Ok", Value::Bool(false)),
            1 => result("Ok", Value::Bool(true)),
            other => invalid(other),
        },
        ValueType::Enum(e) => {
            let bits = scalar_value(e.scalar(), raw);
            let word = int(&bits)?;
            match e.variants().iter().find(|(_, v)| *v == word) {
                Some((name, _)) => result("Ok", data(e.name(), name, vec![])),
                None => invalid(word),
            }
        }
        ValueType::Handle { handle, nullable } => match (*nullable, raw) {
            (true, 0) => data("Option", "None", vec![]),
            (true, h) => some(handle_value(handle.name(), h)),
            (false, 0) => invalid(0),
            (false, h) => result("Ok", handle_value(handle.name(), h)),
        },
        ValueType::NonNullHandle { handle } => {
            if raw == 0 {
                return Err(format!(
                    "a ForeignVal[NonNull[{}]] import returned NULL",
                    handle.name()
                ));
            }
            handle_value(handle.name(), raw)
        }
        ValueType::Address { .. } => return Err("address results have no source value".into()),
    })
}

fn data(ty: &str, constructor: &str, fields: Vec<(&str, Value)>) -> Value {
    Value::Data {
        ty: ty.into(),
        constructor: constructor.into(),
        fields: fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        resource_identity: None,
    }
}
fn result(constructor: &str, v: Value) -> Value {
    data("Result", constructor, vec![("0", v)])
}
fn some(v: Value) -> Value {
    data("Option", "Some", vec![("0", v)])
}
/// The payload of a nullable argument, or the value itself.
fn option(v: &Value, nullable: bool) -> Result<Option<&Value>, String> {
    if !nullable {
        return Ok(Some(v));
    }
    match v {
        Value::Data {
            ty, constructor, ..
        } if ty == "Option" && constructor == "None" => Ok(None),
        Value::Data {
            ty,
            constructor,
            fields,
            ..
        } if ty == "Option" && constructor == "Some" => {
            Ok(Some(fields.get("0").ok_or("Some without a payload")?))
        }
        other => Err(format!("not an Option: {other:?}")),
    }
}
/// The elements of a `Slice` or tuple argument.
fn elements(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::Slice { seq, start, len } => Ok(seq
            .borrow()
            .get(*start..start + len)
            .ok_or("slice out of bounds")?
            .to_vec()),
        Value::Tuple(items) => Ok(items.clone()),
        other => Err(format!("not a slice: {other:?}")),
    }
}
fn int(v: &Value) -> Result<u64, String> {
    match v {
        Value::Int(n) | Value::SInt(n, _) => Ok(*n as u64),
        Value::UInt(n, _) => Ok(*n),
        Value::Bool(b) => Ok(u64::from(*b)),
        Value::Data {
            ty, constructor, ..
        } => Err(format!("not a scalar: {ty}::{constructor}")),
        other => Err(format!("not a scalar: {other:?}")),
    }
}
fn scalar_value(s: Scalar, raw: u64) -> Value {
    match s {
        Scalar::U8 => Value::UInt(raw & 0xff, 8),
        Scalar::U16 => Value::UInt(raw & 0xffff, 16),
        Scalar::U32 => Value::UInt(raw & 0xffff_ffff, 32),
        Scalar::U64 | Scalar::Usize => Value::UInt(raw, 64),
        Scalar::I64 => Value::Int(raw as i64),
        Scalar::I32 => Value::SInt(i64::from(raw as u32 as i32), 32),
        Scalar::I16 => Value::SInt(i64::from(raw as u16 as i16), 16),
        Scalar::I8 => Value::SInt(i64::from(raw as u8 as i8), 8),
    }
}
fn integer_value(bits: u16, signed: bool, raw: u64) -> Value {
    let s = match (bits, signed) {
        (8, false) => Scalar::U8,
        (16, false) => Scalar::U16,
        (32, false) => Scalar::U32,
        (8, true) => Scalar::I8,
        (16, true) => Scalar::I16,
        (32, true) => Scalar::I32,
        (_, true) => Scalar::I64,
        _ => Scalar::U64,
    };
    scalar_value(s, raw)
}

/// Write `v` into `out` with `p`'s C layout.
fn encode(p: &Pointee, v: &Value, out: &mut [u8]) -> Result<(), String> {
    match p {
        Pointee::Scalar(s) => {
            let width = usize::from(s.bits() / 8);
            out[..width].copy_from_slice(&int(v)?.to_ne_bytes()[..width]);
            Ok(())
        }
        Pointee::Struct(w) => encode_struct(w.layout(), v, out),
    }
}
fn encode_struct(layout: &WireLayout, v: &Value, out: &mut [u8]) -> Result<(), String> {
    let Value::Data { fields, .. } = v else {
        return Err(format!("not a {}: {v:?}", layout.name()));
    };
    for f in layout.fields() {
        let at = &mut out[f.offset()..f.offset() + f.size()];
        let value = || {
            fields
                .get(f.name())
                .ok_or_else(|| format!("{} has no field {}", layout.name(), f.name()))
        };
        match f.kind() {
            FieldKind::Padding => at.fill(0),
            FieldKind::Integer(i) => {
                let width = usize::from(i.bits / 8);
                let x = int(value()?)?;
                let bytes = match i.endian {
                    Endian::Native => x.to_ne_bytes(),
                    Endian::Little => x.to_le_bytes(),
                    Endian::Big => (x << (64 - 8 * width as u32)).to_be_bytes(),
                };
                at.copy_from_slice(&bytes[..width]);
            }
            FieldKind::Bytes => {
                for (b, item) in at.iter_mut().zip(elements(value()?)?) {
                    *b = int(&item)? as u8;
                }
            }
            FieldKind::Struct(inner) => encode_struct(inner, value()?, at)?,
        }
    }
    Ok(())
}
/// Read a value with `p`'s C layout from `bytes`.
fn decode(p: &Pointee, bytes: &[u8]) -> Result<Value, String> {
    match p {
        Pointee::Scalar(s) => {
            let width = usize::from(s.bits() / 8);
            let mut word = [0u8; 8];
            word[..width].copy_from_slice(&bytes[..width]);
            Ok(scalar_value(*s, u64::from_ne_bytes(word)))
        }
        Pointee::Struct(w) => decode_struct(w.layout(), bytes),
    }
}
fn decode_struct(layout: &WireLayout, bytes: &[u8]) -> Result<Value, String> {
    let mut out = BTreeMap::new();
    for f in layout.fields() {
        let at = &bytes[f.offset()..f.offset() + f.size()];
        let v = match f.kind() {
            FieldKind::Padding => continue,
            FieldKind::Integer(i) => {
                let width = usize::from(i.bits / 8);
                let mut word = [0u8; 8];
                let raw = match i.endian {
                    Endian::Native => {
                        word[..width].copy_from_slice(at);
                        u64::from_ne_bytes(word)
                    }
                    Endian::Little => {
                        word[..width].copy_from_slice(at);
                        u64::from_le_bytes(word)
                    }
                    Endian::Big => {
                        word[8 - width..].copy_from_slice(at);
                        u64::from_be_bytes(word)
                    }
                };
                integer_value(i.bits, i.signed, raw)
            }
            FieldKind::Bytes => {
                Value::Tuple(at.iter().map(|b| Value::UInt(u64::from(*b), 8)).collect())
            }
            FieldKind::Struct(inner) => decode_struct(inner, at)?,
        };
        out.insert(f.name().to_owned(), v);
    }
    Ok(Value::Data {
        ty: layout.name().into(),
        constructor: layout.name().into(),
        fields: out,
        resource_identity: None,
    })
}

fn as_bytes(words: &mut [u64]) -> &mut [u8] {
    // SAFETY: u64 has no padding and every byte pattern is valid; the view
    // covers exactly the words' bytes.
    unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), words.len() * 8) }
}

/// Call the C function at `address` with `w` as its integer arguments.
///
/// # Safety
/// `address` must be a C-ABI function whose parameters are `w.len()`
/// integer-class words and whose result (if any) is one.
unsafe fn call_words(address: usize, w: &[u64]) -> Result<u64, String> {
    type F0 = extern "C" fn() -> u64;
    type F1 = extern "C" fn(u64) -> u64;
    type F2 = extern "C" fn(u64, u64) -> u64;
    type F3 = extern "C" fn(u64, u64, u64) -> u64;
    type F4 = extern "C" fn(u64, u64, u64, u64) -> u64;
    type F5 = extern "C" fn(u64, u64, u64, u64, u64) -> u64;
    type F6 = extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64;
    type F7 = extern "C" fn(u64, u64, u64, u64, u64, u64, u64) -> u64;
    type F8 = extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> u64;
    type F9 = extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64, u64) -> u64;
    type F10 = extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64, u64, u64) -> u64;
    // SAFETY: the caller's contract.
    unsafe {
        Ok(match *w {
            [] => std::mem::transmute::<usize, F0>(address)(),
            [a] => std::mem::transmute::<usize, F1>(address)(a),
            [a, b] => std::mem::transmute::<usize, F2>(address)(a, b),
            [a, b, c] => std::mem::transmute::<usize, F3>(address)(a, b, c),
            [a, b, c, d] => std::mem::transmute::<usize, F4>(address)(a, b, c, d),
            [a, b, c, d, e] => std::mem::transmute::<usize, F5>(address)(a, b, c, d, e),
            [a, b, c, d, e, f] => std::mem::transmute::<usize, F6>(address)(a, b, c, d, e, f),
            [a, b, c, d, e, f, g] => std::mem::transmute::<usize, F7>(address)(a, b, c, d, e, f, g),
            [a, b, c, d, e, f, g, h] => {
                std::mem::transmute::<usize, F8>(address)(a, b, c, d, e, f, g, h)
            }
            [a, b, c, d, e, f, g, h, i] => {
                std::mem::transmute::<usize, F9>(address)(a, b, c, d, e, f, g, h, i)
            }
            [a, b, c, d, e, f, g, h, i, j] => {
                std::mem::transmute::<usize, F10>(address)(a, b, c, d, e, f, g, h, i, j)
            }
            _ => return Err(format!("{} argument words", w.len())),
        })
    }
}

//! Native lowering of foreign operations (design §13.3-13.4). A child module
//! of `crate::native`, entered from `Codegen::perform` for effects of
//! imported `.vsys` libraries. No `unsafe` Rust: the only machine code that
//! is not Cranelift output is the two generated `RDPKRU`/`WRPKRU` leaf
//! functions of the `isolated(mpk)` trampoline.
use super::*;
use crate::ffi_lowering::{self as ffi, Handler};
use cranelift_module::DataDescription;
use vesper_ffi::sys::{AbiType, Access, ParameterType, Pointee, PointerAccess, Scalar, ValueType};
use vesper_ffi::{Endian, FieldKind, WireLayout};

/// `xor ecx, ecx; rdpkru; ret` — returns PKRU in eax (System V).
const RDPKRU: [u8; 6] = [0x31, 0xc9, 0x0f, 0x01, 0xee, 0xc3];
/// `mov eax, edi; xor ecx, ecx; xor edx, edx; wrpkru; ret`.
const WRPKRU: [u8; 10] = [0x89, 0xf8, 0x31, 0xc9, 0x31, 0xd2, 0x0f, 0x01, 0xef, 0xc3];
const RDPKRU_SYMBOL: &str = "vesper_bridge_rdpkru";
const WRPKRU_SYMBOL: &str = "vesper_bridge_wrpkru";

/// Native source memory or a by-value scalar.
#[derive(Clone, Copy)]
enum Source {
    Memory(ir::Value),
    Scalar(ir::Value),
}
/// Bytes to copy back into source memory after the call.
struct CopyBack {
    guard: Option<ir::Value>,
    buffer: ir::Value,
    target: ir::Value,
    shape: Shape,
}
#[derive(Clone)]
enum Shape {
    Scalar(Scalar),
    Struct(WireLayout, Layout),
    Slice(Pointee, Layout, usize),
}

/// A marshalled C buffer and how to copy it back, when the callee writes it.
type Marshalled = Option<(ir::Value, Shape)>;

fn c_type(bits: u8) -> ir::Type {
    match bits {
        8 => types::I8,
        16 => types::I16,
        32 => types::I32,
        _ => types::I64,
    }
}
fn abi_param(t: AbiType, pointer: ir::Type) -> AbiParam {
    match t {
        AbiType::Pointer => AbiParam::new(pointer),
        AbiType::Integer(s) => {
            let p = AbiParam::new(c_type(s.bits()));
            if s.bits() == 64 {
                p
            } else if s.signed() {
                p.sext()
            } else {
                p.uext()
            }
        }
    }
}
/// Structs whose source layout already is their C layout: only 64-bit
/// native-endian integers, in order, without padding or nesting.
fn identical_struct(w: &WireLayout, little: bool) -> bool {
    w.fields().iter().enumerate().all(|(i, f)| match f.kind() {
        FieldKind::Integer(int) => {
            int.bits == 64
                && f.offset() == 8 * i
                && match int.endian {
                    Endian::Native => true,
                    Endian::Little => little,
                    Endian::Big => !little,
                }
        }
        _ => false,
    })
}
fn identical(p: &Pointee, little: bool) -> bool {
    match p {
        Pointee::Scalar(s) => s.bits() == 64,
        Pointee::Struct(w) => identical_struct(w.layout(), little),
    }
}

impl<M: Module> Codegen<'_, '_, M> {
    pub(super) fn foreign_perform(
        &mut self,
        effect: &str,
        op: &str,
        args: &[Arg],
    ) -> Result<ir::Value, String> {
        let plan = ffi::op_plan(effect, op)?;
        let signature = self
            .world
            .effects
            .get(effect)
            .and_then(|e| e.ops.iter().find(|f| f.name == op))
            .ok_or("unknown foreign operation")?
            .clone();
        let descriptors = plan.function.parameters();
        if args.len() != signature.params.len() || descriptors.len() != signature.params.len() {
            return Err(format!(
                "foreign call {}::{} has the wrong number of arguments",
                plan.library,
                plan.function.symbol()
            ));
        }
        let pointer = self.m.target_config().pointer_type();
        let mut c_args = vec![];
        let mut copies = vec![];
        let mut restores = vec![];
        for ((arg, param), descriptor) in args.iter().zip(&signature.params).zip(descriptors) {
            let value = self.expr(&arg.expr)?;
            let aggregate = Layout::of(self.world, &param.ty)?.aggregate();
            let source = if param.pass == Pass::Inout {
                let place = self.address(&arg.expr)?;
                if !aggregate && let ExprKind::Name(n) = &arg.expr.kind {
                    let var = *self.vars.get(n).ok_or("unknown mutable foreign argument")?;
                    restores.push((var, place, param.ty.clone()));
                }
                Source::Memory(place)
            } else if aggregate {
                Source::Memory(value)
            } else {
                Source::Scalar(value)
            };
            match descriptor.ty() {
                ParameterType::Value(v) => c_args.push(self.foreign_value(v, source)?),
                ParameterType::Pointer {
                    pointee,
                    access,
                    nullable,
                } => {
                    let (payload, guard, payload_ty) =
                        self.foreign_option(source, &param.ty, *nullable)?;
                    let writes = *access != PointerAccess::In;
                    let (ptr, copy) =
                        self.foreign_pointee(pointee, payload, &payload_ty, writes)?;
                    if let Some((buffer, shape)) = copy {
                        let Source::Memory(target) = payload else {
                            return Err("writable foreign pointer lacks a place".into());
                        };
                        copies.push(CopyBack {
                            guard,
                            buffer,
                            target,
                            shape,
                        });
                    }
                    c_args.push(self.foreign_guard(guard, ptr, pointer));
                }
                ParameterType::Slice {
                    element,
                    access,
                    nullable,
                } => {
                    let (payload, guard, payload_ty) =
                        self.foreign_option(source, &param.ty, *nullable)?;
                    let writes = *access == Access::Inout;
                    let (ptr, len, copy) =
                        self.foreign_slice(element, payload, &payload_ty, writes)?;
                    if let Some((buffer, shape)) = copy {
                        let Source::Memory(target) = payload else {
                            return Err("writable foreign slice lacks a place".into());
                        };
                        copies.push(CopyBack {
                            guard,
                            buffer,
                            target,
                            shape,
                        });
                    }
                    let ptr = self.foreign_guard(guard, ptr, pointer);
                    c_args.push(ptr);
                    let len = self.b.ins().iconst(types::I64, len as i64);
                    c_args.push(self.foreign_guard(guard, len, types::I64));
                }
            }
        }
        let abi = plan.function.abi();
        let mut sig = self.m.make_signature();
        for t in &abi.parameters {
            sig.params.push(abi_param(*t, pointer));
        }
        if let Some(t) = abi.result {
            sig.returns.push(abi_param(t, pointer));
        }
        let callee = self
            .m
            .declare_function(plan.function.symbol(), Linkage::Import, &sig)
            .map_err(|e| format!("foreign symbol '{}': {e}", plan.function.symbol()))?;
        let callee = self.m.declare_func_in_func(callee, self.b.func);
        let saved = if plan.handler == Handler::IsolatedMpk {
            Some(self.foreign_isolate(&plan.library)?)
        } else {
            None
        };
        let call = self.b.ins().call(callee, &c_args);
        let raw = self.b.inst_results(call).first().copied();
        if let Some(saved) = saved {
            let wrpkru = self.foreign_helper(WRPKRU_SYMBOL, &[types::I32], None, &WRPKRU)?;
            self.b.ins().call(wrpkru, &[saved]);
        }
        for copy in copies {
            self.foreign_copy_back(copy)?;
        }
        for (var, ptr, ty) in restores {
            let value = self.load(ptr, &ty)?;
            self.def_var(var, value);
            if let Some(destination) = self.pointer(var) {
                self.b
                    .ins()
                    .store(ir::MemFlags::new(), value, destination, 0);
            }
        }
        self.foreign_result(plan.function.result(), raw, &signature.result)
    }
    fn foreign_guard(
        &mut self,
        guard: Option<ir::Value>,
        value: ir::Value,
        ty: ir::Type,
    ) -> ir::Value {
        match guard {
            Some(is_some) => {
                let zero = self.b.ins().iconst(ty, 0);
                self.b.ins().select(is_some, value, zero)
            }
            None => value,
        }
    }
    fn foreign_narrow(&mut self, v: ir::Value, bits: u8) -> ir::Value {
        if bits == 64 {
            v
        } else {
            self.b.ins().ireduce(c_type(bits), v)
        }
    }
    fn foreign_widen(&mut self, v: ir::Value, bits: u8, signed: bool) -> ir::Value {
        if bits == 64 {
            v
        } else if signed {
            self.b.ins().sextend(types::I64, v)
        } else {
            self.b.ins().uextend(types::I64, v)
        }
    }
    fn foreign_word(&mut self, source: Source) -> ir::Value {
        match source {
            Source::Scalar(v) => v,
            Source::Memory(p) => self.b.ins().load(types::I64, ir::MemFlags::new(), p, 0),
        }
    }
    fn foreign_value(&mut self, v: &ValueType, source: Source) -> Result<ir::Value, String> {
        match v {
            ValueType::Scalar(s) => {
                let word = self.foreign_word(source);
                Ok(self.foreign_narrow(word, s.bits()))
            }
            ValueType::Bool => {
                let word = self.foreign_word(source);
                Ok(self.b.ins().ireduce(types::I8, word))
            }
            ValueType::Enum(e) => {
                let Source::Memory(p) = source else {
                    return Err("foreign enum argument is not in memory".into());
                };
                let tag = self.b.ins().load(types::I64, ir::MemFlags::new(), p, 0);
                let variants = e.variants();
                let first = variants.first().map_or(0, |(_, v)| *v);
                let mut value = self.b.ins().iconst(types::I64, first as i64);
                for (i, (_, raw)) in variants.iter().enumerate().skip(1) {
                    let hit = self.b.ins().icmp_imm(IntCC::Equal, tag, i as i64);
                    let candidate = self.b.ins().iconst(types::I64, *raw as i64);
                    value = self.b.ins().select(hit, candidate, value);
                }
                Ok(self.foreign_narrow(value, e.scalar().bits()))
            }
            ValueType::Unit | ValueType::Address { .. } => {
                Err("foreign parameter type has no native lowering".into())
            }
        }
    }
    /// For a nullable parameter, the `Some` payload and its presence flag.
    fn foreign_option(
        &mut self,
        source: Source,
        ty: &Type,
        nullable: bool,
    ) -> Result<(Source, Option<ir::Value>, Type), String> {
        if !nullable {
            return Ok((source, None, ty.clone()));
        }
        let Type::Named(n, args) = ty else {
            return Err("nullable foreign argument is not an Option".into());
        };
        if n != "Option" || args.len() != 1 {
            return Err("nullable foreign argument is not an Option".into());
        }
        let Source::Memory(p) = source else {
            return Err("Option foreign argument is not in memory".into());
        };
        let layout = Layout::of(self.world, ty)?;
        let (index, fields) = layout.variant("Some").ok_or("Option lacks Some")?;
        let offset = fields.first().ok_or("Some lacks a payload")?.offset;
        let tag = self.b.ins().load(types::I64, ir::MemFlags::new(), p, 0);
        let is_some = self.b.ins().icmp_imm(IntCC::Equal, tag, index as i64);
        let payload = self.b.ins().iadd_imm(p, i64::from(offset));
        Ok((Source::Memory(payload), Some(is_some), args[0].clone()))
    }
    /// The C pointer of a single-element pointee, and the copy-back buffer
    /// when the callee may write it and it was marshalled.
    fn foreign_pointee(
        &mut self,
        pointee: &Pointee,
        source: Source,
        ty: &Type,
        writes: bool,
    ) -> Result<(ir::Value, Marshalled), String> {
        let little = self.m.isa().endianness() == ir::Endianness::Little;
        match pointee {
            Pointee::Scalar(s) => {
                let buffer = self.alloc(8);
                let word = self.foreign_word(source);
                let v = self.foreign_narrow(word, s.bits());
                self.b.ins().store(ir::MemFlags::new(), v, buffer, 0);
                Ok((buffer, writes.then_some((buffer, Shape::Scalar(*s)))))
            }
            Pointee::Struct(w) => {
                let Source::Memory(p) = source else {
                    return Err("foreign struct argument is not in memory".into());
                };
                if identical(pointee, little) {
                    return Ok((p, None));
                }
                let native = Layout::of(self.world, ty)?;
                let buffer = self
                    .alloc(u32::try_from(w.layout().size()).map_err(|_| "wire struct too large")?);
                self.foreign_write_struct(p, &native, w.layout(), buffer, 0)?;
                Ok((
                    buffer,
                    writes.then(|| (buffer, Shape::Struct(w.layout().clone(), native))),
                ))
            }
        }
    }
    fn foreign_slice(
        &mut self,
        element: &Pointee,
        source: Source,
        ty: &Type,
        writes: bool,
    ) -> Result<(ir::Value, usize, Marshalled), String> {
        let little = self.m.isa().endianness() == ir::Endianness::Little;
        let n = match ty {
            Type::Tuple(ts) => ts.len(),
            _ => 0,
        };
        if n == 0 {
            // A non-null, zero-length slice.
            return Ok((self.alloc(8), 0, None));
        }
        let Source::Memory(p) = source else {
            return Err("foreign slice argument is not in memory".into());
        };
        if identical(element, little) {
            return Ok((p, n, None));
        }
        let native = Layout::of(self.world, ty)?;
        let bytes = n
            .checked_mul(element.size())
            .and_then(|b| u32::try_from(b).ok())
            .ok_or("foreign slice buffer is too large")?;
        let buffer = self.alloc(bytes);
        self.foreign_write_slice(element, p, &native, buffer, n)?;
        Ok((
            buffer,
            n,
            writes.then(|| (buffer, Shape::Slice(element.clone(), native, n))),
        ))
    }
    fn foreign_write_slice(
        &mut self,
        element: &Pointee,
        native_ptr: ir::Value,
        native: &Layout,
        buffer: ir::Value,
        n: usize,
    ) -> Result<(), String> {
        let LayoutKind::Product(fields) = &native.kind else {
            return Err("foreign slice is not a tuple".into());
        };
        for (i, f) in fields.iter().enumerate().take(n) {
            let at = i * element.size();
            match element {
                Pointee::Scalar(s) => {
                    let v =
                        self.b
                            .ins()
                            .load(types::I64, ir::MemFlags::new(), native_ptr, f.offset);
                    let v = self.foreign_narrow(v, s.bits());
                    self.b
                        .ins()
                        .store(ir::MemFlags::new(), v, buffer, at as i32);
                }
                Pointee::Struct(w) => {
                    let inner = self.b.ins().iadd_imm(native_ptr, i64::from(f.offset));
                    self.foreign_write_struct(inner, &f.layout, w.layout(), buffer, at)?;
                }
            }
        }
        Ok(())
    }
    fn foreign_read_slice(
        &mut self,
        element: &Pointee,
        buffer: ir::Value,
        native_ptr: ir::Value,
        native: &Layout,
        n: usize,
    ) -> Result<(), String> {
        let LayoutKind::Product(fields) = &native.kind else {
            return Err("foreign slice is not a tuple".into());
        };
        for (i, f) in fields.iter().enumerate().take(n) {
            let at = i * element.size();
            match element {
                Pointee::Scalar(s) => {
                    let v =
                        self.b
                            .ins()
                            .load(c_type(s.bits()), ir::MemFlags::new(), buffer, at as i32);
                    let v = self.foreign_widen(v, s.bits(), s.signed());
                    self.b
                        .ins()
                        .store(ir::MemFlags::new(), v, native_ptr, f.offset);
                }
                Pointee::Struct(w) => {
                    let inner = self.b.ins().iadd_imm(native_ptr, i64::from(f.offset));
                    self.foreign_read_struct(buffer, at, w.layout(), inner, &f.layout)?;
                }
            }
        }
        Ok(())
    }
    fn foreign_swap(&mut self, v: ir::Value, int: &vesper_ffi::Integer) -> ir::Value {
        let little = self.m.isa().endianness() == ir::Endianness::Little;
        let swap = match int.endian {
            Endian::Native => false,
            Endian::Little => !little,
            Endian::Big => little,
        };
        if swap && int.bits > 8 {
            self.b.ins().bswap(v)
        } else {
            v
        }
    }
    fn foreign_write_struct(
        &mut self,
        native_ptr: ir::Value,
        native: &Layout,
        wire: &WireLayout,
        buffer: ir::Value,
        base: usize,
    ) -> Result<(), String> {
        for f in wire.fields() {
            let at = i32::try_from(base + f.offset()).map_err(|_| "wire offset too large")?;
            let field = || {
                native
                    .field(f.name())
                    .ok_or_else(|| format!("source struct lacks wire field '{}'", f.name()))
            };
            match f.kind() {
                FieldKind::Integer(int) => {
                    let nf = field()?;
                    let bits = u8::try_from(int.bits).map_err(|_| "wire integer too wide")?;
                    let v =
                        self.b
                            .ins()
                            .load(types::I64, ir::MemFlags::new(), native_ptr, nf.offset);
                    let v = self.foreign_narrow(v, bits);
                    let v = self.foreign_swap(v, int);
                    self.b.ins().store(ir::MemFlags::new(), v, buffer, at);
                }
                FieldKind::Bytes => {
                    let nf = field()?;
                    let LayoutKind::Product(bytes) = &nf.layout.kind else {
                        return Err("ByteArray field is not a tuple".into());
                    };
                    for (j, b) in bytes.iter().enumerate().take(f.size()) {
                        let v = self.b.ins().load(
                            types::I64,
                            ir::MemFlags::new(),
                            native_ptr,
                            nf.offset + b.offset,
                        );
                        let v = self.b.ins().ireduce(types::I8, v);
                        self.b
                            .ins()
                            .store(ir::MemFlags::new(), v, buffer, at + j as i32);
                    }
                }
                // Buffers are zeroed when allocated.
                FieldKind::Padding => {}
                FieldKind::Struct(inner) => {
                    let nf = field()?;
                    let p = self.b.ins().iadd_imm(native_ptr, i64::from(nf.offset));
                    self.foreign_write_struct(p, &nf.layout, inner, buffer, base + f.offset())?;
                }
            }
        }
        Ok(())
    }
    fn foreign_read_struct(
        &mut self,
        buffer: ir::Value,
        base: usize,
        wire: &WireLayout,
        native_ptr: ir::Value,
        native: &Layout,
    ) -> Result<(), String> {
        for f in wire.fields() {
            let at = i32::try_from(base + f.offset()).map_err(|_| "wire offset too large")?;
            let field = || {
                native
                    .field(f.name())
                    .ok_or_else(|| format!("source struct lacks wire field '{}'", f.name()))
            };
            match f.kind() {
                FieldKind::Integer(int) => {
                    let nf = field()?;
                    let bits = u8::try_from(int.bits).map_err(|_| "wire integer too wide")?;
                    let v = self
                        .b
                        .ins()
                        .load(c_type(bits), ir::MemFlags::new(), buffer, at);
                    let v = self.foreign_swap(v, int);
                    let v = self.foreign_widen(v, bits, int.signed);
                    self.b
                        .ins()
                        .store(ir::MemFlags::new(), v, native_ptr, nf.offset);
                }
                FieldKind::Bytes => {
                    let nf = field()?;
                    let LayoutKind::Product(bytes) = &nf.layout.kind else {
                        return Err("ByteArray field is not a tuple".into());
                    };
                    for (j, b) in bytes.iter().enumerate().take(f.size()) {
                        let v = self.b.ins().load(
                            types::I8,
                            ir::MemFlags::new(),
                            buffer,
                            at + j as i32,
                        );
                        let v = self.b.ins().uextend(types::I64, v);
                        self.b.ins().store(
                            ir::MemFlags::new(),
                            v,
                            native_ptr,
                            nf.offset + b.offset,
                        );
                    }
                }
                FieldKind::Padding => {}
                FieldKind::Struct(inner) => {
                    let nf = field()?;
                    let p = self.b.ins().iadd_imm(native_ptr, i64::from(nf.offset));
                    self.foreign_read_struct(buffer, base + f.offset(), inner, p, &nf.layout)?;
                }
            }
        }
        Ok(())
    }
    fn foreign_copy_back(&mut self, copy: CopyBack) -> Result<(), String> {
        let join = match copy.guard {
            Some(is_some) => {
                let then = self.b.create_block();
                let join = self.b.create_block();
                self.branch(is_some, then, &[], join, &[]);
                self.b.switch_to_block(then);
                Some(join)
            }
            None => None,
        };
        match &copy.shape {
            Shape::Scalar(s) => {
                let v = self
                    .b
                    .ins()
                    .load(c_type(s.bits()), ir::MemFlags::new(), copy.buffer, 0);
                let v = self.foreign_widen(v, s.bits(), s.signed());
                self.b.ins().store(ir::MemFlags::new(), v, copy.target, 0);
            }
            Shape::Struct(wire, native) => {
                self.foreign_read_struct(copy.buffer, 0, wire, copy.target, native)?;
            }
            Shape::Slice(element, native, n) => {
                self.foreign_read_slice(element, copy.buffer, copy.target, native, *n)?;
            }
        }
        if let Some(join) = join {
            self.jump(join, &[]);
            self.b.switch_to_block(join);
        }
        Ok(())
    }
    /// The source value of a foreign result. Bool and enum results are
    /// validated: an out-of-range raw value becomes `Err(ForeignInvalid)`.
    fn foreign_result(
        &mut self,
        result: &ValueType,
        raw: Option<ir::Value>,
        ty: &Type,
    ) -> Result<ir::Value, String> {
        let raw = || raw.ok_or("foreign call returned no value");
        match result {
            ValueType::Unit => Ok(self.b.ins().iconst(types::I64, 0)),
            ValueType::Scalar(s) => {
                let v = raw()?;
                Ok(self.foreign_widen(v, s.bits(), s.signed()))
            }
            ValueType::Bool => {
                let v = raw()?;
                let word = self.b.ins().uextend(types::I64, v);
                let valid = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::UnsignedLessThanOrEqual, word, 1);
                self.foreign_validated(ty, valid, word, word)
            }
            ValueType::Enum(e) => {
                let v = raw()?;
                let word = self.foreign_widen(v, e.scalar().bits(), false);
                let mut valid = self.b.ins().iconst(types::I8, 0);
                let mut index = self.b.ins().iconst(types::I64, 0);
                for (i, (_, value)) in e.variants().iter().enumerate() {
                    let hit = self.b.ins().icmp_imm(IntCC::Equal, word, *value as i64);
                    let candidate = self.b.ins().iconst(types::I64, i as i64);
                    index = self.b.ins().select(hit, candidate, index);
                    valid = self.b.ins().bor(valid, hit);
                }
                self.foreign_validated(ty, valid, index, word)
            }
            ValueType::Address { .. } => Err("foreign address results have no lowering".into()),
        }
    }
    /// `Ok(ok)` when `valid`, else `Err(ForeignInvalid { raw })`. Both
    /// payloads are one machine word at the same offset.
    fn foreign_validated(
        &mut self,
        ty: &Type,
        valid: ir::Value,
        ok: ir::Value,
        raw: ir::Value,
    ) -> Result<ir::Value, String> {
        let layout = Layout::of(self.world, ty)?;
        let (ok_index, ok_fields) = layout.variant("Ok").ok_or("Result lacks Ok")?;
        let ok_offset = ok_fields.first().ok_or("Ok lacks a payload")?.offset;
        let (err_index, err_fields) = layout.variant("Err").ok_or("Result lacks Err")?;
        let err_offset = err_fields.first().ok_or("Err lacks a payload")?.offset;
        if ok_offset != err_offset {
            return Err("Result payload offsets differ".into());
        }
        let ptr = self.alloc(layout.size);
        let ok_tag = self.b.ins().iconst(types::I64, ok_index as i64);
        let err_tag = self.b.ins().iconst(types::I64, err_index as i64);
        let tag = self.b.ins().select(valid, ok_tag, err_tag);
        self.b.ins().store(ir::MemFlags::new(), tag, ptr, 0);
        let payload = self.b.ins().select(valid, ok, raw);
        self.b
            .ins()
            .store(ir::MemFlags::new(), payload, ptr, ok_offset);
        Ok(ptr)
    }
    /// A local leaf function defined from generated machine code, declared
    /// and defined once per module.
    fn foreign_helper(
        &mut self,
        name: &str,
        params: &[ir::Type],
        result: Option<ir::Type>,
        code: &[u8],
    ) -> Result<ir::FuncRef, String> {
        let defined = self.m.declarations().get_name(name).is_some();
        let mut sig = self.m.make_signature();
        sig.call_conv = cranelift_codegen::isa::CallConv::SystemV;
        for p in params {
            sig.params.push(AbiParam::new(*p));
        }
        if let Some(r) = result {
            sig.returns.push(AbiParam::new(r));
        }
        let id = self
            .m
            .declare_function(name, Linkage::Local, &sig)
            .map_err(|e| e.to_string())?;
        if !defined {
            self.m
                .define_function_bytes(id, 16, code, &[])
                .map_err(|e| e.to_string())?;
        }
        Ok(self.m.declare_func_in_func(id, self.b.func))
    }
    /// Revoke the library's protection key; returns the saved PKRU.
    fn foreign_isolate(&mut self, library: &str) -> Result<ir::Value, String> {
        let isa = self.m.isa();
        if isa.triple().architecture != target_lexicon::Architecture::X86_64
            || isa.triple().operating_system != target_lexicon::OperatingSystem::Linux
        {
            return Err("isolated(mpk) trampolines are generated for Linux x86-64 only".into());
        }
        let symbol = ffi::isolation_symbol(library);
        let defined = self.m.declarations().get_name(&symbol).is_some();
        let data = self
            .m
            .declare_data(&symbol, Linkage::Export, true, false)
            .map_err(|e| e.to_string())?;
        if !defined {
            let mut description = DataDescription::new();
            description.define(vec![0u8; 4].into_boxed_slice());
            description.set_align(4);
            self.m
                .define_data(data, &description)
                .map_err(|e| e.to_string())?;
        }
        let global = self.m.declare_data_in_func(data, self.b.func);
        let pointer = self.m.target_config().pointer_type();
        let address = self.b.ins().symbol_value(pointer, global);
        let mask = self
            .b
            .ins()
            .load(types::I32, ir::MemFlags::new(), address, 0);
        let unset = self.b.ins().icmp_imm(IntCC::Equal, mask, 0);
        let host = self.b.ins().band_imm(mask, 0b11);
        let host = self.b.ins().icmp_imm(IntCC::NotEqual, host, 0);
        let bad = self.b.ins().bor(unset, host);
        self.b
            .ins()
            .trapnz(bad, ir::TrapCode::unwrap_user(ffi::ISOLATION_TRAP));
        let rdpkru = self.foreign_helper(RDPKRU_SYMBOL, &[], Some(types::I32), &RDPKRU)?;
        let call = self.b.ins().call(rdpkru, &[]);
        let saved = self.b.inst_results(call)[0];
        let revoked = self.b.ins().bor(saved, mask);
        let wrpkru = self.foreign_helper(WRPKRU_SYMBOL, &[types::I32], None, &WRPKRU)?;
        self.b.ins().call(wrpkru, &[revoked]);
        Ok(saved)
    }
}
